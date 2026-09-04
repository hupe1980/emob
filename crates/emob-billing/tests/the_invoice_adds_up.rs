//! Properties of an invoice built from rated records, over pseudo-random
//! tariffs and sessions.
//!
//! The examples in `a_month_closes.rs` are each written after the failure that
//! motivated it, so each one is a shape somebody already thought of. These are
//! the statements those examples are instances of:
//!
//! 1. **The document adds up at every level the standard states one.**
//!    `Invoice::reconciles` is `PEPPOL-EN16931-R120` on each line, `BR-CO-13`
//!    with `BR-S-08` across the VAT breakdown, and `BR-CO-15` on the payable
//!    total — checked over invoices whose tariffs mix VAT rates and carry the
//!    minimum and maximum that become a document level allowance or charge.
//! 2. **The EN 16931 verdict is `valid`.** Not "we believe it is": the semantic
//!    model carries the standard's own 317 business rules, and the document is
//!    run through them.
//! 3. **The residual is the whole of what the document approximates.** Rounding
//!    happens once, at the line, so the difference between the taxable total and
//!    what the records came to exactly is bounded by a minor unit per line — and
//!    it is [`Invoice::rounding_residual`] rather than a figure a reconciliation
//!    has to discover.
//! 4. **Every record billed is a record supplied**, once.
//!
//! The evidence on each record is **stated** rather than assembled from a real
//! chain: what these assert is the arithmetic of the document rather than the
//! provenance of the kilowatt-hours, which `a_month_closes.rs` exercises from a
//! real signature. It has to be there all the same, because a German invoice
//! resting on measured values has to be one its recipient can check
//! `[MessEG §33]` and `InvoiceBuilder` enforces that (D232).

use emob_billing::{Counterparty, InvoiceBuilder, TaxStatus, en16931};
use emob_cdr::{Cdr, CdrBuilder, EvidenceRef};
use emob_core::{
    CdrId, Currency, Direction, Energy, IdentificationStrength, Money, PartyId, TimeZone,
};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{
    Dimension, PriceComponent, PriceLimit, Restrictions, Tariff, TariffElement, TariffKind,
    TaxIncluded,
};
use rust_decimal::Decimal;
use time::macros::{date, datetime};

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

fn party() -> PartyId {
    PartyId::new("DE", "PRP").expect("a valid party id")
}

/// A tariff that prices energy, occupancy and a session fee — the last two at a
/// **different** VAT rate a fifth of the time, because a document whose lines
/// sit in one category is a document whose breakdown cannot disagree with its
/// lines.
fn tariff(rng: &mut Rng, basis: TaxIncluded) -> Tariff {
    let electricity = dec(19, 0);
    let service = if rng.chance(20) {
        dec(7, 0)
    } else {
        electricity
    };

    let mut elements = Vec::new();
    if rng.chance(60) {
        // A first tier, so a session can be charged at two prices.
        elements.push(TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Energy, dec(rng.between(20, 60), 2))
                    .with_vat(electricity),
            ],
            restrictions: Restrictions {
                max_kwh: Some(dec(rng.between(1, 30), 0)),
                ..Restrictions::default()
            },
        });
    }
    // A reservation element, before the unrestricted one — a record's *second*
    // rating, priced over a window that ran before any energy moved. Without it
    // the generator could not build the shape `emob-billing` silently dropped:
    // an invoice that omits a term is internally consistent, so every property
    // below held over a document short by the whole reservation (D250).
    if rng.chance(35) {
        elements.push(TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Time, dec(rng.between(100, 900), 2))
                    .with_vat(service),
                PriceComponent::new(Dimension::Flat, dec(rng.between(0, 200), 2)).with_vat(service),
            ],
            restrictions: Restrictions {
                reservation: Some(emob_tariff::ReservationRestriction::Reservation),
                ..Restrictions::default()
            },
        });
    }
    elements.push(TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Energy, dec(rng.between(20, 60), 2)).with_vat(electricity),
        PriceComponent::new(Dimension::ParkingTime, dec(rng.between(100, 600), 2))
            .with_vat(service),
        PriceComponent::new(Dimension::Flat, dec(rng.between(0, 150), 2)).with_vat(service),
    ]));

    Tariff {
        id: "prop".parse().expect("a valid tariff id"),
        currency: Currency::EUR,
        kind: TariffKind::Contract,
        time_zone: TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        tax_included: basis,
        elements,
        // The two bounds that become a document level allowance or charge, and
        // the one that leaves an invoice with no priced line to adjust.
        min_price: {
            let take = rng.chance(30);
            let amount = dec(rng.between(0, 1500), 2);
            take.then(|| PriceLimit::net(amount))
        },
        max_price: {
            let take = rng.chance(15);
            let amount = dec(rng.between(1500, 6000), 2);
            take.then(|| PriceLimit::net(amount))
        },
        valid_from: None,
        valid_until: None,
    }
}

/// When the nth generated session begins — a function of the ordinal alone, so
/// the reservation that precedes it can be placed without unpicking the session.
fn session_start(ordinal: u64) -> time::OffsetDateTime {
    datetime!(2026-06-01 09:00 +2)
        + time::Duration::days(i64::from(u32::try_from(ordinal).unwrap_or(0)))
}

/// A session that charged for a while and then sat there, at whole watt-hours.
fn session(rng: &mut Rng, ordinal: u64) -> Session {
    let start = session_start(ordinal);
    let charging_for = time::Duration::minutes(i64::try_from(rng.between(5, 240)).unwrap_or(30));
    let parked_for = time::Duration::minutes(i64::try_from(rng.between(0, 120)).unwrap_or(0));
    let stopped = start + charging_for;
    let end = stopped + parked_for;

    let register = dec(rng.between(1_000, 5_000_000), 3);
    let delivered = dec(rng.between(500, 80_000), 3);

    let mut session = Session::open(
        format!("s-{ordinal}").parse().expect("a valid session id"),
        "DE*PRP*E000001".parse().expect("a valid EVSE id"),
        Authorization::ad_hoc(),
        start,
    );
    session
        .transition_to(SessionState::Charging, start)
        .expect("a session begins charging");
    if parked_for > time::Duration::ZERO {
        session
            .transition_to(SessionState::SuspendedByVehicle, stopped)
            .expect("charging then suspended is a lawful transition");
    }

    let mut readings = vec![MeterReading::new(
        start,
        Energy::from_kwh(register).expect("non-negative"),
        Direction::Import,
        ReadingContext::TransactionBegin,
    )];
    if parked_for > time::Duration::ZERO {
        readings.push(MeterReading::new(
            stopped,
            Energy::from_kwh(register + delivered).expect("non-negative"),
            Direction::Import,
            ReadingContext::SamplePeriodic,
        ));
    }
    readings.push(MeterReading::new(
        end,
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

fn record(rng: &mut Rng, ordinal: u64, tariff: &Tariff) -> Cdr {
    let session = session(rng, ordinal);
    let mut builder = CdrBuilder::from_session(&session, Direction::Import)
        .expect("an ended session with a series is a record")
        .key(
            party(),
            format!("c-{ordinal}").parse::<CdrId>().expect("a valid id"),
        )
        .evidence(signed(rng));
    // Held for a while before the cable went in, half the time. The window ends
    // where the session begins, which is what `RESERVATION` means.
    if rng.chance(50) {
        let held = time::Duration::minutes(i64::try_from(rng.between(1, 90)).unwrap_or(15));
        let began = session_start(ordinal);
        builder = builder.reserved(emob_tariff::Reservation::honoured(began - held, began));
    }
    builder.rated_with(tariff).build().expect("a rated record")
}

/// A record behind a record, so the document is one `[MessEG §33]` permits.
///
/// Stated rather than assembled from a real chain: these are generated sessions
/// and the subject is the **arithmetic** of the document, which
/// `a_month_closes.rs` exercises from a real signature. The reference has to be
/// here all the same, because a German invoice resting on measured values has to
/// be one the recipient can check, and `InvoiceBuilder` enforces that (D232) —
/// which is itself worth asserting over generated months rather than over the
/// one example that motivated it.
fn signed(rng: &mut Rng) -> EvidenceRef {
    EvidenceRef {
        encoding_method: "OCMF".into(),
        payload_digests: vec![[7u8; 32]],
        identification_strength: IdentificationStrength::Trusted,
        energy_billable: true,
        duration_billable: true,
        direction: Some(Direction::Import),
        // A DC station metering on the AC side of its rectifier, a fifth of the
        // time. The figure is inside the register the session billed and
        // `[REA 6-A §3.2]` obliges the operator to say so on *"einem Messwert
        // oder einer Rechnung"* — so an invoice that omits it is one the
        // regulation names and the document does not carry (D253).
        compensated_loss: rng
            .chance(20)
            .then(|| Energy::from_kwh(dec(rng.between(1, 400), 3)).expect("non-negative")),
        tariff_changes: Vec::new(),
    }
}

fn cpo() -> Counterparty {
    Counterparty::new(
        "Stadtwerke Musterstadt GmbH",
        "Musterstadt",
        TaxStatus::business("DE", "DE123456789"),
    )
    .at("Hauptstraße 1", "12345")
    .reachable_at("de-cpo@example.org", "EM")
    .registered_as("HRB 12345", None)
    .contactable("Rechnungswesen", "+49 555 123456", "rechnung@example.org")
}

fn driver() -> Counterparty {
    Counterparty::new(
        "Erika Mustermann",
        "Beispielstadt",
        TaxStatus::consumer("DE"),
    )
    .at("Nebenweg 7", "54321")
}

#[test]
fn a_generated_month_is_a_document_that_adds_up_and_the_standard_accepts() {
    let mut rng = Rng(0xB111_1AB1_0000_0001);

    let mut built = 0usize;
    let mut with_reservation = 0usize;
    let mut disclosed = 0usize;
    for case in 0..250 {
        let basis = if case % 2 == 0 {
            TaxIncluded::Yes
        } else {
            TaxIncluded::No
        };
        let t = tariff(&mut rng, basis);
        let records: Vec<Cdr> = (0..rng.between(1, 6))
            .map(|n| record(&mut rng, n, &t))
            .collect();
        assert!(!records.is_empty(), "case {case}: no record built at all");
        built += records.len();

        let mut builder = InvoiceBuilder::new(
            format!("R-{case:04}"),
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            cpo(),
            driver(),
        )
        .supplied_from("DE", dec(19, 0))
        .due_on(date!(2026 - 07 - 15));
        for cdr in &records {
            builder = builder.record(cdr);
        }
        let invoice = builder
            .build()
            .expect("rated records in one currency")
            .value;

        // 1. The document's own arithmetic, at every level the standard states
        //    one.
        assert!(
            invoice.reconciles(),
            "case {case}: the invoice does not add up: {:#?}",
            invoice.tax
        );
        assert_eq!(
            invoice.gross_total(),
            Money::new(
                invoice.taxable_total().amount() + invoice.tax_total().amount(),
                Currency::EUR
            ),
            "case {case}"
        );

        // 2. And the standard's own 317 rules agree.
        let crossed = en16931::to_en16931(&invoice, en16931::Specification::Core)
            .expect("every figure fits an EN 16931 amount");
        assert!(
            crossed.value.is_valid(),
            "case {case}: {:?}",
            crossed.value.reasons().collect::<Vec<_>>()
        );

        // 3. The residual is bounded by a minor unit per line and is stated
        //    rather than absorbed.
        let residual = invoice.rounding_residual().amount().abs();
        let bound = dec(1, 2) * Decimal::from(invoice.lines.len() + invoice.adjustments.len());
        assert!(
            residual <= bound,
            "case {case}: the document approximates {residual}, above {bound}"
        );
        assert_eq!(
            invoice.taxable_total().amount() - invoice.exact_taxable_total().amount(),
            invoice.rounding_residual().amount(),
            "case {case}"
        );

        // 4. Every record supplied is billed, once.
        assert_eq!(
            invoice.records().len(),
            records.len(),
            "case {case}: a record was billed twice or not at all"
        );

        // 5. **The document bills what the records say is owed.** Every property
        //    above asks whether the invoice adds up to *itself*, and all of them
        //    held over a document that silently omitted each record's
        //    reservation — the whole of D250. This is the one that compares the
        //    document with its inputs, and the only slack it allows is the
        //    rounding already bounded in 3: each record's own total rounds once
        //    per VAT category, and this document rounds once per line.
        let owed: Decimal = records
            .iter()
            .map(|cdr| cdr.total_cost().expect("a rated record").amount())
            .sum();
        let billed = invoice.gross_total().amount();
        assert!(
            (billed - owed).abs() <= bound,
            "case {case}: the records say {owed} is owed and the invoice bills {billed}"
        );

        if invoice
            .lines
            .iter()
            .any(|line| line.description.contains("reservation"))
        {
            with_reservation += 1;
        }

        // 6. **Every measured value that contains compensated loss says so, on
        //    the document.** `[REA 6-A §3.2]` names the invoice, and the figure
        //    used to reach a roaming partner and stop there (D253).
        for (cdr, line) in records.iter().flat_map(|cdr| {
            invoice
                .lines
                .iter()
                .filter(move |line| line.cdr == cdr.key)
                .map(move |line| (cdr, line))
        }) {
            let stated = cdr
                .evidence
                .as_ref()
                .and_then(|evidence| evidence.compensated_loss);
            let expected = if line.dimension == emob_tariff::Dimension::Energy {
                stated
            } else {
                None
            };
            assert_eq!(
                line.compensated_loss, expected,
                "case {case}: line {} does not state what its measured value contains",
                line.id
            );
            if expected.is_some() {
                disclosed += 1;
            }
        }
    }
    assert!(built > 500, "only {built} records were built");
    // A generator that cannot reach a shape is a generator whose properties say
    // nothing about it, so the coverage is asserted rather than assumed.
    assert!(
        with_reservation > 25,
        "only {with_reservation} of 250 documents carried a reservation line"
    );
    assert!(
        disclosed > 25,
        "only {disclosed} lines disclosed what their measured value contains"
    );
}
