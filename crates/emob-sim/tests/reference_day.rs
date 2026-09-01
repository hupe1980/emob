//! The M2 demo, as a test rather than a screenshot.
//!
//! > **100 virtual stations boot, authorize, charge, and the ledger reconciles
//! > to the watt-second.**
//!
//! Every session here is signed with a real key, verified through the real
//! verifier, split on the real settlement grid, priced by the real rating
//! engine and accepted by the real ledger. Nothing is stubbed; the only thing
//! that is imaginary is the hardware.

use emob_core::Energy;
use emob_sim::{Fault, FaultPlan, Rate, ReferenceDay};

/// A watt-second, as a fraction of a kilowatt-hour: 1 / 3 600 000.
///
/// The unit the milestone is phrased in. Nothing here reconciles *to* it — the
/// arithmetic is exact decimal throughout, so the residual is zero rather than
/// small — but stating the tolerance makes the difference visible.
fn one_watt_second() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ONE / rust_decimal::Decimal::from(3_600_000)
}

#[test]
fn a_hundred_stations_charge_and_the_ledger_reconciles() {
    let day = ReferenceDay::builder()
        .stations(100)
        .sessions_per_station(4)
        .faults(FaultPlan::everything(Rate::one_in(9)))
        .build();

    assert_eq!(day.stations(), 100);
    assert_eq!(day.planned_sessions(), 400);

    let outcome = day.run();
    println!("{}", outcome.summary());

    // ── The identity ────────────────────────────────────────────────────────
    //
    // Every kilowatt-hour a meter moved either reached a settled record or was
    // refused with a reason. This is `Σ allocated + residual = total` over a
    // day, and it is the only assertion a fleet run can make that a silent
    // failure cannot satisfy — a run asserting "no errors" would pass by
    // throwing sessions away.
    assert!(outcome.reconciles(), "{}", outcome.summary());
    let residual = (outcome.billed().kwh() + outcome.unbilled().kwh()) - outcome.metered.kwh();
    assert!(
        residual.abs() < one_watt_second(),
        "residual {residual} kWh is not below a watt-second"
    );
    assert_eq!(
        residual,
        rust_decimal::Decimal::ZERO,
        "…and is in fact zero"
    );

    // ── Nothing vanishes ────────────────────────────────────────────────────
    assert_eq!(outcome.attempted, 400);
    assert!(outcome.every_session_is_accounted_for());
    assert!(outcome.every_refusal_has_a_reason());

    // ── Every settled record carries its own arithmetic ─────────────────────
    assert!(outcome.every_record_conserves());
    let ledger_energy: Energy = outcome.ledger.iter().map(|c| c.total_energy).sum();
    assert_eq!(ledger_energy, outcome.billed());
    assert_eq!(outcome.ledger.len(), outcome.settled.len());

    // ── And the day is worth running: both outcomes occur ───────────────────
    assert!(!outcome.settled.is_empty(), "nothing settled");
    assert!(!outcome.refused.is_empty(), "nothing was refused");
}

#[test]
fn every_fault_in_the_catalogue_is_actually_exercised() {
    // A run that exercises only the rules somebody remembered to list is a run
    // that drifts. Injecting the whole catalogue and asserting each one reached
    // a refusal is what keeps the fleet honest about its own coverage.
    let outcome = ReferenceDay::builder()
        .stations(60)
        .sessions_per_station(4)
        .faults(FaultPlan::everything(Rate::one_in(4)))
        .build()
        .run();

    for (fault, seen) in outcome.faults_seen() {
        // `UnsynchronisedClock` is the one fault that leaves the energy
        // billable, so it only refuses on the half of the fleet that prices a
        // duration. Every other fault must refuse wherever it lands.
        assert!(
            seen > 0,
            "{fault} was never exercised: {}",
            outcome.summary()
        );
    }

    // …and the clock fault really does leave the energy alone somewhere: the
    // energy-only posts bill straight through it.
    let clock_refusals = outcome
        .refused
        .iter()
        .filter(|r| r.faults == vec![Fault::UnsynchronisedClock])
        .count();
    assert!(
        clock_refusals < outcome.refused.len(),
        "an unsynchronised clock must not be the only reason anything is refused"
    );
}

#[test]
fn a_tariff_a_post_may_not_offer_is_refused_before_it_prices_anything() {
    // The one fault in the catalogue that is nothing to do with the meter, and
    // the gate a fleet of metering faults alone never runs. The energy is
    // measured perfectly and every signature holds; the session must still not
    // be priced, because `[AFIR Art. 5(4)]` is a rule about the *pairing* of a
    // tariff with a charge point and this post is a 150 kW charger.
    let outcome = ReferenceDay::builder()
        .seed(0xAF14)
        .stations(40)
        .sessions_per_station(3)
        .faults(FaultPlan::none().with(Fault::UnlawfulTariff, Rate::ALWAYS))
        .build()
        .run();

    // Half the fleet is a 22 kW post, where the identical tariff is an ordinary
    // product — so the fault refuses exactly the fast half and nothing else.
    assert_eq!(outcome.refused.len() * 2, outcome.attempted as usize);
    assert!(!outcome.settled.is_empty(), "the AC posts bill straight on");
    assert!(outcome.reconciles(), "{}", outcome.summary());

    for refusal in &outcome.refused {
        assert!(
            refusal.reasons.iter().any(|r| r.contains("price per kWh")),
            "a refusal has to name the rule: {:?}",
            refusal.reasons
        );
    }
}

#[test]
fn every_settled_kilowatt_hour_came_through_the_ocpp_seam_and_was_signed() {
    // The M2b property. The fleet's sessions are not constructed — they are
    // *assembled from OCPP transaction events*, the way a CSMS assembles them,
    // and the only numbers that survive that assembly are the ones inside a
    // signed data set.
    //
    // `emob-ocpp`'s input vocabulary has no numeric meter value in it, so there
    // is no field a float could arrive in. This asserts the consequence: every
    // reading behind every settled record is signed, and every settled record's
    // total is the difference of two of them.
    let outcome = ReferenceDay::builder()
        .stations(40)
        .sessions_per_station(3)
        .faults(FaultPlan::everything(Rate::one_in(5)))
        .build()
        .run();

    assert!(!outcome.settled.is_empty());
    for cdr in &outcome.settled {
        let evidence = cdr
            .evidence
            .as_ref()
            .expect("every settled record carries its evidence");
        assert!(
            evidence.energy_billable,
            "{}: a settled record whose own evidence refuses its energy",
            cdr.key
        );
        assert!(
            !evidence.payload_digests.is_empty(),
            "{}: a settled record with no signed payload behind it",
            cdr.key
        );
        assert!(cdr.conserves());
    }
    assert!(outcome.reconciles(), "{}", outcome.summary());
}

#[test]
fn a_clock_aligned_reading_still_measures_its_slot_after_the_seam() {
    // `Sample.Clock` is the one reading context that makes a settlement slot
    // **measured** rather than interpolated, and it lives in the *protocol* —
    // nothing in a signed record says why a reading was taken. A seam that
    // carried the context and did not read it marked every quarter hour in the
    // fleet as an assumption, silently, while every other number stayed right.
    //
    // Four stations in five report clock-aligned readings, so most sessions
    // have to come out fully measured.
    let outcome = ReferenceDay::builder()
        .stations(20)
        .sessions_per_station(3)
        .build()
        .run();

    let fully_measured = outcome
        .settled
        .iter()
        .filter(|cdr| cdr.fully_measured())
        .count();
    assert!(
        fully_measured * 2 > outcome.settled.len(),
        "only {fully_measured} of {} sessions measured their own slots: the OCPP \
         reading context is not reaching the split",
        outcome.settled.len()
    );

    // …and the posts that do not report aligned readings are still honest about
    // it, so this is a distinction rather than a constant.
    assert!(
        fully_measured < outcome.settled.len(),
        "every session measured: the interpolated case is not being exercised"
    );
}

#[test]
fn a_transaction_with_no_signed_value_never_reaches_a_price() {
    // The seam rule, stated where a CSMS would hit it. OCPP's own numeric
    // fields — `meterStart`, `meterStop`, `SampledValue.value` — are telemetry,
    // and a station that sends only those has produced no billable quantity at
    // all. There is no repair; there is a different question, about whether
    // this jurisdiction bills unsigned values `[MessEG §33]`.
    use emob_core::Direction;
    use emob_ocpp::{Transaction, TransactionEvent};
    use emob_session::{Authorization, EndReason};

    let at = time::macros::datetime!(2026-01-02 10:00 +1);
    let unsigned = Transaction::new(
        "t-1".parse().unwrap(),
        "DE*SIM*E00001".parse().unwrap(),
        Authorization::ad_hoc(),
    )
    .with(TransactionEvent::started(at, vec![]))
    .with(TransactionEvent::ended(
        at + time::Duration::minutes(30),
        vec![],
        EndReason::Local,
    ));

    let error = unsigned.assemble(Direction::Import).unwrap_err();
    assert!(error.to_string().contains("telemetry"), "{error}");
}

#[test]
fn a_clean_fleet_bills_every_kilowatt_hour_it_measured() {
    // The other end of the same identity: with nothing injected, the residual
    // is not merely accounted for, it is empty.
    let outcome = ReferenceDay::builder()
        .stations(100)
        .sessions_per_station(2)
        .build()
        .run();

    assert_eq!(outcome.refused.len(), 0, "{}", outcome.summary());
    assert_eq!(outcome.billed(), outcome.metered);
    assert_eq!(outcome.unbilled(), Energy::ZERO);
    assert!(outcome.reconciles());
}

#[test]
fn the_whole_day_replays_from_its_seed() {
    // A dispute about a fleet run is answered by running it again, which is
    // only possible while nothing in the chain reads a clock.
    let day = || {
        ReferenceDay::builder()
            .seed(0x5EED)
            .stations(40)
            .sessions_per_station(3)
            .faults(FaultPlan::everything(Rate::one_in(7)))
            .build()
            .run()
    };
    let (first, second) = (day(), day());

    assert_eq!(first.summary(), second.summary());
    assert_eq!(first.billed(), second.billed());
    assert_eq!(first.settled.len(), second.settled.len());
    for (a, b) in first.settled.iter().zip(&second.settled) {
        assert_eq!(a, b, "the same seed must produce the same records");
    }
}

#[test]
fn a_refusal_says_what_a_human_would_need_to_act_on_it() {
    // The half of the identity that is not arithmetic. A residual nobody can
    // explain is a leak with a total attached.
    let outcome = ReferenceDay::builder()
        .seed(0x1234)
        .stations(30)
        .sessions_per_station(3)
        .faults(FaultPlan::everything(Rate::one_in(3)))
        .build()
        .run();

    for refusal in outcome.refused.iter().take(5) {
        println!(
            "{} ({}): {}",
            refusal.session_id,
            refusal
                .faults
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            refusal.reasons.join(" | ")
        );
    }

    // Every refusal names its session, its energy and at least one reason long
    // enough to act on — not a code somebody has to look up.
    for refusal in &outcome.refused {
        assert!(!refusal.reasons.is_empty(), "{}", refusal.session_id);
        assert!(
            refusal.reasons.iter().any(|r| r.len() > 20),
            "{}: {:?}",
            refusal.session_id,
            refusal.reasons
        );
    }
}

#[test]
fn the_fleet_prices_against_more_than_one_tariff_element() {
    // Both fleet tariffs carry a night rate, so the fleet reaches the rating
    // engine's *element matching* across a boundary rather than only its
    // arithmetic against a single element.
    //
    // Not its *subdivision*: every period here is a quarter-hour slot and the
    // boundaries are on the quarter-hour grid, so a period never spans one.
    // That is the split doing its job `[PTB-A 50.7 §3.1.7.2]`, and the
    // subdivision is unit-tested in `emob_tariff::rating` instead.
    let outcome = ReferenceDay::builder()
        .stations(100)
        .sessions_per_station(4)
        .faults(FaultPlan::everything(Rate::one_in(9)))
        .build()
        .run();

    let multi_priced: Vec<&emob_cdr::Cdr> = outcome
        .settled
        .iter()
        .filter(|cdr| {
            cdr.cost.as_ref().is_some_and(|cost| {
                cost.rated
                    .lines
                    .iter()
                    .filter(|line| line.dimension == emob_tariff::Dimension::Energy)
                    .count()
                    > 1
            })
        })
        .collect();

    assert!(
        !multi_priced.is_empty(),
        "no settled session was priced across the night boundary, so the fleet \
         still offers what is effectively one tariff element"
    );

    // Every one of them still conserves, and prices exactly what it metered.
    for cdr in &multi_priced {
        assert!(
            cdr.conserves(),
            "{} lost energy across the boundary",
            cdr.key
        );
        let cost = cdr.cost.as_ref().expect("filtered on having one");
        assert_eq!(
            cost.rated.quantity_for(emob_tariff::Dimension::Energy),
            cdr.total_energy.kwh(),
            "{} priced a different quantity than it metered",
            cdr.key
        );
    }

    assert!(outcome.reconciles(), "{}", outcome.summary());
}
