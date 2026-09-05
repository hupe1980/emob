//! The property the two halves of this crate owe each other, over generated
//! sessions rather than over one fixture.
//!
//! `CdrBuilder` refuses what does not add up; `validate` refuses what a partner
//! sent that does not add up. A builder that emits a record its own validator
//! blocks is **two rules about one record**, and which of the two a settlement
//! sees then depends on which side of the wire it stands on.
//!
//! Three statements, over sessions shaped the way real ones are — a charge, a
//! suspension, a wait before the charge begins, a series narrower than the
//! session window, a session that never charged at all:
//!
//! 1. **A record that builds is a record that validates.** The one blocking
//!    finding permitted is `EnergyNotBillable`, which is a fact about the
//!    *evidence* rather than about the arithmetic: nothing here is signed, and
//!    `[MessEG §33]` is what that finding states.
//! 2. **The periods partition the record.** They begin at the session's start,
//!    end at its end, and leave no gap and no overlap — so no second of a
//!    session is priced twice or not at all.
//! 3. **The price is the record's own.** `Cdr::total_cost` is what rating the
//!    record's own periods produces, so a partner re-deriving it from the
//!    periods reaches the figure the record states.

use emob_cdr::{CdrBuilder, Finding, validate};
use emob_core::{CdrId, ClockResolution, Currency, Direction, Energy, PartyId, TimeZone};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{
    Chargeable, Dimension, PriceComponent, PriceLimit, Tariff, TariffKind, TaxIncluded, rate,
};
use rust_decimal::Decimal;
use time::macros::datetime;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.between(1, 100) <= percent
    }
}

fn dec(units: u64, scale: u32) -> Decimal {
    Decimal::new(i64::try_from(units).unwrap_or(i64::MAX), scale)
}

fn minutes(n: u64) -> time::Duration {
    time::Duration::minutes(i64::try_from(n).unwrap_or(0))
}

const START: time::OffsetDateTime = datetime!(2026-06-01 09:07:23 +2);

/// A tariff that prices energy and occupancy, with a first tier often enough
/// that a record carries two energy lines.
fn tariff(rng: &mut Rng) -> Tariff {
    let mut t = Tariff::simple(
        "prop".parse().expect("a valid tariff id"),
        Currency::EUR,
        TariffKind::Contract,
        TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        vec![
            PriceComponent::new(Dimension::Energy, dec(rng.between(20, 60), 2)),
            PriceComponent::new(Dimension::ParkingTime, dec(rng.between(100, 600), 2)),
        ],
    );
    t.tax_included = TaxIncluded::Yes;
    if rng.chance(50) {
        t.min_price = Some(PriceLimit::gross(dec(rng.between(0, 400), 2)));
    }
    // A tariff whose energy price is **conditional**, and a block size on the
    // occupancy fee. Both are shapes an operator writes on purpose — a
    // promotional first tier, a fee billed in whole minutes — and neither was
    // reachable: every generated tariff priced energy unconditionally, in one
    // step, so the two lawful ways a billed quantity differs from a measured one
    // were the one region this property never entered. Ten per cent of the
    // records built from a generator that could reach them were records the
    // builder emitted and the validator blocked (D258).
    if rng.chance(35) {
        t.elements.insert(
            0,
            emob_tariff::TariffElement {
                components: vec![PriceComponent::new(
                    Dimension::Energy,
                    dec(rng.between(20, 60), 2),
                )],
                restrictions: emob_tariff::Restrictions {
                    min_kwh: Some(Decimal::from(rng.between(1, 25))),
                    ..emob_tariff::Restrictions::default()
                },
            },
        );
        // …and half the time nothing behind it, so the first kilowatt-hours are
        // charged by nothing at all rather than by a cheaper tier.
        if rng.chance(50) {
            t.elements.retain(|e| {
                e.component(Dimension::Energy).is_none() || e.restrictions.min_kwh.is_some()
            });
        }
    }
    if rng.chance(30)
        && let Some(component) = t
            .elements
            .iter_mut()
            .flat_map(|e| e.components.iter_mut())
            .find(|c| c.dimension == Dimension::ParkingTime)
    {
        *component = component.clone().with_step_size(60 * 15);
    }
    // A reservation element, so a generated record can carry the **second**
    // rating a CDR holds. Without it the property below said nothing about the
    // half of `Cost` that `validate` had never read (D250).
    if rng.chance(40) {
        t.elements.push(emob_tariff::TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Time, dec(rng.between(100, 900), 2)),
                PriceComponent::new(Dimension::Flat, dec(rng.between(0, 200), 2)),
            ],
            restrictions: emob_tariff::Restrictions {
                reservation: Some(emob_tariff::ReservationRestriction::Reservation),
                ..emob_tariff::Restrictions::default()
            },
        });
    }
    t
}

/// One generated session: the ordinary OCPP 2.0.1 shape, with the wait before
/// the charge, the suspension after it and the meter series' own window all
/// varied.
fn session(rng: &mut Rng) -> Session {
    let waiting = minutes(rng.between(0, 3));
    let charging = minutes(rng.between(1, 240));
    let parked = minutes(rng.between(0, 180));

    let charge_from = START + waiting;
    let charge_to = charge_from + charging;
    let end = charge_to + parked;

    let mut session = Session::open(
        "s-1".parse().expect("a valid session id"),
        "DE*PRP*E000001".parse().expect("a valid EVSE id"),
        Authorization::ad_hoc(),
        START,
    );
    if waiting > time::Duration::ZERO {
        session
            .transition_to(SessionState::SuspendedByVehicle, START)
            .expect("a vehicle connects before it charges");
    }
    session
        .transition_to(SessionState::Charging, charge_from)
        .expect("and then it charges");
    if parked > time::Duration::ZERO {
        session
            .transition_to(SessionState::SuspendedByVehicle, charge_to)
            .expect("and then it sits there");
    }

    // The series covers the charge and, some of the time, less of the session
    // than the session covers — the shape a record's unmetered periods come
    // from.
    let register = dec(rng.between(1_000, 5_000_000), 3);
    let delivered = dec(rng.between(0, 80_000), 3);
    let mut readings = vec![MeterReading::new(
        charge_from,
        Energy::from_kwh(register).expect("non-negative"),
        Direction::Import,
        ReadingContext::TransactionBegin,
    )];
    if rng.chance(50) {
        let half = charge_from + charging / 2;
        readings.push(MeterReading::new(
            half,
            Energy::from_kwh(register + delivered / Decimal::from(2)).expect("non-negative"),
            Direction::Import,
            ReadingContext::SampleClock,
        ));
    }
    readings.push(MeterReading::new(
        charge_to,
        Energy::from_kwh(register + delivered).expect("non-negative"),
        Direction::Import,
        ReadingContext::TransactionEnd,
    ));
    session
        .attach_series(
            MeterSeries::new(Direction::Import, readings).expect("ascending, non-decreasing"),
        )
        .expect("one series per direction");
    session
        .end(end, EndReason::Local)
        .expect("a session ends after it starts");
    session
}

#[test]
fn a_record_that_builds_is_a_record_that_validates() {
    let mut rng = Rng(0xCD_0000_0000_0001);
    let mut built = 0usize;
    let mut refused = 0usize;
    let mut reserved = 0usize;
    let mut gave_energy_away = 0usize;
    let mut rounded_to_a_block = 0usize;

    for case in 0..1000 {
        let t = tariff(&mut rng);
        let session = session(&mut rng);
        // Held for a while before the cable went in, some of the time. The
        // window ends where the session begins, which is what `RESERVATION`
        // means `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`.
        let held = rng.chance(45).then(|| {
            emob_tariff::Reservation::honoured(START - minutes(rng.between(1, 90)), START)
        });
        let record = CdrBuilder::from_session(&session, Direction::Import).and_then(|builder| {
            let builder = match held {
                Some(reservation) => builder.reserved(reservation),
                None => builder,
            };
            builder
                .key(
                    PartyId::new("DE", "PRP").expect("a valid party id"),
                    format!("c-{case}").parse::<CdrId>().expect("a valid id"),
                )
                .rated_with(&t)
                .build()
        });
        let Ok(cdr) = record else {
            // A refusal is an answer, and it names its reason.
            refused += 1;
            continue;
        };
        built += 1;

        // 1. The builder emits nothing its own validator blocks — except the
        //    one finding that is about evidence rather than arithmetic, and
        //    nothing here is signed.
        if let Some(cost) = &cdr.cost {
            if !cost
                .rated
                .unpriced_for(emob_tariff::Dimension::Energy)
                .is_zero()
            {
                gave_energy_away += 1;
            }
            if !cost
                .rated
                .block_surplus_for(emob_tariff::Dimension::ParkingTime)
                .is_zero()
            {
                rounded_to_a_block += 1;
            }
        }

        let report = validate(&cdr);
        assert!(
            report
                .blocking()
                .all(|finding| matches!(finding, Finding::EnergyNotBillable)),
            "case {case}: the builder emitted a record it would refuse: {:?}",
            report.reasons().collect::<Vec<_>>()
        );

        // 2. The periods partition the record.
        assert_eq!(
            cdr.periods.first().expect("a record has periods").start,
            cdr.started_at,
            "case {case}: the first period starts late"
        );
        assert_eq!(
            cdr.periods.last().expect("a record has periods").end,
            cdr.ended_at,
            "case {case}: the last period stops early"
        );
        for pair in cdr.periods.windows(2) {
            assert_eq!(
                pair[0].end, pair[1].start,
                "case {case}: a second is priced twice or not at all"
            );
        }
        assert!(cdr.conserves(), "case {case}: the periods lost energy");

        // 3. The price is what rating the record's own periods produces — and
        //    where a reservation preceded the session, **both** ratings, because
        //    `total_cost` is both and an invoice bills both (D250).
        if let Some(total) = cdr.total_cost() {
            let chargeable: Chargeable = cdr.chargeable().expect("its own periods");
            let session_price = rate(&t, &chargeable).gross();
            let expected = match held {
                None => session_price,
                Some(reservation) => emob_core::Money::new(
                    session_price.amount()
                        + emob_tariff::rate_reservation(&t, &reservation)
                            .gross()
                            .amount(),
                    session_price.currency(),
                ),
            };
            assert_eq!(
                expected, total,
                "case {case}: the record states a price its own periods do not"
            );
            if held.is_some() {
                reserved += 1;
            }
        }
    }

    assert!(built > 800, "only {built} of 1000 records built");
    assert!(
        reserved > 200,
        "only {reserved} records carried a reservation, so the second rating on a `Cost` \
         is a shape this property does not reach"
    );
    assert!(
        gave_energy_away > 100,
        "only {gave_energy_away} records left energy unpriced, so the shape that blocked a lawful \
         promotional tariff is one this property does not reach"
    );
    assert!(
        rounded_to_a_block > 50,
        "only {rounded_to_a_block} records rounded a duration to a block, so the other lawful \
         difference between a billed quantity and a measured one is unreached"
    );
    assert!(
        refused < 200,
        "{refused} of 1000 sessions were refused, which is a generator that stopped generating"
    );
}

#[test]
fn a_record_replays_at_its_own_clock_rather_than_at_the_callers() {
    // The resolution a duration is judged against decides a price
    // `[REA 6-A §3.1]`, and it is a fact about the *station's* type approval.
    // A record that did not carry it could only be re-priced by a caller who
    // remembered it — and a caller who can be told it can be told the wrong
    // one, which is the whole of what a two-year-old dispute turns on.
    let t = Tariff::simple(
        "occupancy".parse().unwrap(),
        Currency::EUR,
        TariffKind::Contract,
        TimeZone::new("Europe/Berlin").unwrap(),
        vec![
            PriceComponent::new(Dimension::Energy, dec(49, 2)),
            PriceComponent::new(Dimension::ParkingTime, dec(600, 2)),
        ],
    );

    // Thirty seconds of occupancy after a half-hour charge: billable on a
    // station whose approval states ten seconds, and not on one judged at the
    // regulation's sixty-second cap.
    let mut session = Session::open(
        "s-1".parse().unwrap(),
        "DE*PRP*E000001".parse().unwrap(),
        Authorization::ad_hoc(),
        START,
    );
    session
        .transition_to(SessionState::Charging, START)
        .unwrap();
    session
        .transition_to(SessionState::SuspendedByVehicle, START + minutes(30))
        .unwrap();
    session
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        START,
                        Energy::from_kwh(dec(100_000, 3)).unwrap(),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        START + minutes(30),
                        Energy::from_kwh(dec(115_000, 3)).unwrap(),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    session
        .end(
            START + minutes(30) + time::Duration::seconds(30),
            EndReason::Local,
        )
        .unwrap();

    let precise = ClockResolution::stated(time::Duration::seconds(10)).unwrap();
    let record = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(
            PartyId::new("DE", "PRP").unwrap(),
            "c-1".parse::<CdrId>().unwrap(),
        )
        .clock(precise)
        .rated_with(&t)
        .build()
        .unwrap();

    // 7.35 for the electricity plus five cents of occupancy.
    assert_eq!(record.clock, precise);
    assert_eq!(record.total_cost().unwrap().to_string(), "7.40 EUR");

    // …and re-pricing it reaches the same number without being told anything.
    assert_eq!(
        record.rerated_with(&t).unwrap().total_cost(),
        record.total_cost(),
        "a record replayed against its own tariff must reproduce its own price"
    );

    // The same session on a station that states nothing is judged at the
    // regulation's cap, and the occupancy line goes.
    let default_clock = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(
            PartyId::new("DE", "PRP").unwrap(),
            "c-2".parse::<CdrId>().unwrap(),
        )
        .rated_with(&t)
        .build()
        .unwrap();
    assert_eq!(default_clock.clock, ClockResolution::conforming());
    assert_eq!(default_clock.total_cost().unwrap().to_string(), "7.35 EUR");
}
