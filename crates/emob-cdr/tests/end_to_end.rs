//! The whole chain, end to end: signed meter values → evidence → session →
//! CDR → ledger → money.
//!
//! Each crate tests its own half. This is the test that they compose — that the
//! kilowatt-hour the station signed is the kilowatt-hour on the record a partner
//! settles against, with nothing invented and nothing lost in between.
//!
//! Every fixture here is *signed for real*: the OCMF records are produced with
//! a private key at test time and verified through the same code path a station
//! would go through, so the test proves the path rather than a constant.

use emob_cdr::{Acceptance, CdrBuilder, CdrLedger, EvidenceRef, validate};
use emob_core::{Currency, Direction, Energy, IdentificationStrength, PartyId};
use emob_eichrecht::ocmf::KeyType;
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry, PublicKey, ocmf, transparency};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, check_afir, describe};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::datetime;

const METER_SERIAL: &str = "BQ27400330016";

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
}

fn at(minute: i64) -> time::OffsetDateTime {
    datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
}

fn signing_key() -> SigningKey {
    // Deterministic: a test that fails once a month for reasons nobody can
    // reproduce is worse than no test.
    SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
}

/// A real OCMF record, signed with the key the registry knows.
fn signed_record(pagination: u64, marker: &str, register: &str, minute: i64) -> String {
    let payload = format!(
        r#"{{"FV":"1.4","GI":"ACME CS-1","GS":"GW-1","PG":"T{pagination}","MV":"Phoenix Contact","MM":"EEM-350-D-MCB","MS":"{METER_SERIAL}","IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","RD":[{{"TM":"2026-01-02T{:02}:{:02}:00,000+0100 S","TX":"{marker}","RV":{register},"RI":"01-00:B2.08.00*FF","RU":"kWh","RT":"AC","EF":"","ST":"G"}}]}}"#,
        10 + (minute / 60),
        minute % 60,
    );
    let digest = Sha256::digest(payload.as_bytes());
    let sig: DerSignature = signing_key().sign_prehash(&digest).unwrap();
    format!(
        "OCMF|{payload}|{{\"SD\":\"{}\"}}",
        hex::encode(sig.as_bytes())
    )
}

fn registry() -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    registry.insert(
        // A gateway serving one charge point, identified by both serials —
        // the case the specification says needs the pair.
        ComponentRef::GatewayAndMeter {
            gateway: "GW-1".into(),
            meter: METER_SERIAL.into(),
        },
        RegisteredKey::unbounded(
            PublicKey {
                algorithm: KeyType::Secp256r1,
                bytes: signing_key()
                    .verifying_key()
                    .to_encoded_point(false)
                    .as_bytes()
                    .to_vec(),
            },
            "type approval 2026-01",
        ),
    );
    registry
}

/// The station's three signed records for a 30-minute session: 100.000 kWh at
/// 10:00, 110.000 at the 10:15 clock boundary, 118.000 at 10:30.
fn raw_records() -> Vec<String> {
    vec![
        signed_record(1, "B", "100.000", 0),
        signed_record(2, "C", "110.000", 15),
        signed_record(3, "E", "118.000", 30),
    ]
}

/// The session the CSMS assembled from the same transaction.
fn session() -> Session {
    let mut session = Session::open(
        "s-1".parse().unwrap(),
        "DE*AB7*E840*6487".parse().unwrap(),
        Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: "c-1".parse().unwrap(),
                emaid: Some("NL-TNM-C00122045-K".parse().unwrap()),
            },
            token_ref: Some(emob_session::TokenRef::new("a".repeat(64)).unwrap()),
            authorization_reference: Some("auth-9".into()),
        },
        at(0),
    );
    session
        .transition_to(SessionState::Charging, session.started_at)
        .unwrap();
    session
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    )
                    .signed(),
                    MeterReading::new(
                        at(15),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    )
                    .signed(),
                    MeterReading::new(
                        at(30),
                        kwh("118.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    )
                    .signed(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    session.end(at(30), EndReason::Local).unwrap();
    session
}

fn evidence_of(raw: &[String]) -> Evidence {
    let records = raw
        .iter()
        .map(|r| ocmf::parse(r))
        .collect::<Result<Vec<_>, _>>()
        .expect("the fixtures are well-formed OCMF");
    Evidence::assemble(&records, &registry(), at(0))
}

/// Read off the verified records — never filled in by hand.
fn evidence_ref(evidence: &Evidence) -> EvidenceRef {
    EvidenceRef::from_evidence(evidence, "OCMF")
}

#[test]
fn a_genuine_session_reaches_a_settleable_cdr() {
    // 1. The station's signed records verify against the registered key.
    let evidence = evidence_of(&raw_records());
    assert!(
        evidence.problems.is_empty(),
        "{:?}",
        evidence.reasons().collect::<Vec<_>>()
    );
    let billable = evidence
        .billable_energy()
        .expect("a genuine session is billable");
    assert_eq!(billable.to_string(), "18.000 kWh");

    // 2. The session the CSMS assembled agrees with them, to the last digit.
    let session = session();
    assert_eq!(
        session.total(Direction::Import).unwrap(),
        billable,
        "the evidence and the session must agree about the energy"
    );

    // 3. The CDR built from it conserves and is fully measured — the 10:15
    //    boundary had a Sample.Clock reading on it.
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .build()
        .expect("a genuine session builds a CDR");

    assert_eq!(cdr.total_energy, billable);
    assert!(cdr.conserves());
    assert!(cdr.fully_measured());
    assert_eq!(cdr.periods.len(), 2);
    assert_eq!(cdr.periods[0].energy.to_string(), "10.000 kWh");
    assert_eq!(cdr.periods[1].energy.to_string(), "8.000 kWh");

    // 4. Every period is traceable to a signed payload.
    assert_eq!(cdr.evidence.as_ref().unwrap().payload_digests.len(), 3);

    // 5. A partner receiving it finds nothing wrong.
    let report = validate(&cdr);
    assert!(report.is_settleable(), "{:?}", report.findings);
    assert_eq!(report.findings.len(), 0);

    // 6. And it is accepted exactly once, however many times it is sent.
    let mut ledger = CdrLedger::new();
    assert_eq!(ledger.accept(cdr.clone()), Acceptance::Stored);
    assert_eq!(ledger.accept(cdr.clone()), Acceptance::Duplicate);
    assert_eq!(ledger.accept(cdr), Acceptance::Duplicate);
    assert_eq!(ledger.len(), 1);
}

#[test]
fn one_changed_digit_stops_the_whole_chain() {
    // A backend edits 118.000 kWh to 218.000 kWh. Everything downstream would
    // happily carry the larger number; the signature does not.
    let mut raw = raw_records();
    raw[2] = raw[2].replace("118.000", "218.000");

    let evidence = evidence_of(&raw);
    assert!(!evidence.is_billable());
    assert_eq!(evidence.billable_energy(), None);
    assert!(
        evidence.reasons().any(|r| r.contains("record 3")),
        "the reason names the record: {:?}",
        evidence.reasons().collect::<Vec<_>>()
    );
}

#[test]
fn a_deleted_middle_record_stops_the_chain_though_every_signature_holds() {
    // The 10:15 record is dropped. Both remaining signatures are genuine.
    let raw = vec![
        signed_record(1, "B", "100.000", 0),
        signed_record(3, "E", "118.000", 30),
    ];

    let evidence = evidence_of(&raw);
    assert!(
        evidence
            .problems
            .iter()
            .all(|p| matches!(p, emob_eichrecht::EvidenceProblem::Chain(_))),
        "the signatures are fine; the chain is not: {:?}",
        evidence.reasons().collect::<Vec<_>>()
    );
    assert!(!evidence.is_billable());
    assert!(
        evidence
            .reasons()
            .any(|r| r.contains("pagination jumped from 1 to 3"))
    );
}

#[test]
fn a_substitute_reading_stops_the_chain() {
    // The meter could not measure and formed a substitute value. Legitimate
    // telemetry; never an invoice.
    let raw = [
        signed_record(1, "B", "100.000", 0),
        signed_record(2, "C", "110.000", 15).replace(r#""ST":"G""#, r#""ST":"S""#),
        signed_record(3, "E", "118.000", 30),
    ];
    // Re-sign the edited record so the *only* thing wrong is the meter state.
    let payload = raw[1]
        .split('|')
        .nth(1)
        .expect("a payload section")
        .to_owned();
    let digest = Sha256::digest(payload.as_bytes());
    let sig: DerSignature = signing_key().sign_prehash(&digest).unwrap();
    let resigned = format!(
        "OCMF|{payload}|{{\"SD\":\"{}\"}}",
        hex::encode(sig.as_bytes())
    );
    let raw = vec![raw[0].clone(), resigned, raw[2].clone()];

    let evidence = evidence_of(&raw);
    assert!(
        evidence
            .problems
            .iter()
            .all(|p| matches!(p, emob_eichrecht::EvidenceProblem::Chain(_))),
        "the signature is genuine: {:?}",
        evidence.reasons().collect::<Vec<_>>()
    );
    assert!(!evidence.is_billable());
    assert!(evidence.reasons().any(|r| r.contains("Substitute")));
}

#[test]
fn an_unregistered_station_cannot_bill() {
    let records = raw_records()
        .iter()
        .map(|r| ocmf::parse(r))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let evidence = Evidence::assemble(&records, &KeyRegistry::new(), at(0));

    assert!(!evidence.is_billable());
    assert!(
        evidence.chain.is_none(),
        "no chain report is produced over records that did not verify"
    );
}

#[test]
fn an_interpolated_session_still_conserves_and_says_so() {
    // The same session with no clock-aligned reading in the middle: the 10:15
    // boundary has to be interpolated, and that travels all the way to the
    // partner's copy of the record.
    let mut session = Session::open(
        "s-2".parse().unwrap(),
        "DE*AB7*E840*6487".parse().unwrap(),
        Authorization::ad_hoc(),
        at(1),
    );
    session
        .transition_to(SessionState::Charging, session.started_at)
        .unwrap();
    session
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(1),
                        kwh("0"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    // 10:01 → 10:22, so the boundary at 10:15 is two thirds of
                    // the way through and 7 × 2/3 does not terminate.
                    MeterReading::new(
                        at(22),
                        kwh("7"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    session.end(at(22), EndReason::Local).unwrap();

    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-2".parse().unwrap())
        .build()
        .unwrap();

    assert!(cdr.conserves(), "the sum telescopes, whatever the ratio");
    assert_eq!(cdr.total_energy, kwh("7"));
    assert!(!cdr.fully_measured());

    // Settleable, with the interpolation reported as something to know rather
    // than something that blocks.
    let report = validate(&cdr);
    assert!(report.is_settleable());
    assert!(report.reasons().any(|r| r.contains("interpolated")));
}

#[test]
fn a_partner_restating_a_settled_number_is_a_conflict() {
    let session = session();
    let evidence = evidence_of(&raw_records());
    let party = PartyId::new("DE", "ABC").unwrap();

    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(party.clone(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .build()
        .unwrap();

    let mut ledger = CdrLedger::new();
    ledger.accept(cdr.clone());

    // The same id, a larger number.
    let mut restated = cdr.clone();
    restated.total_energy = kwh("118.000");
    let outcome = ledger.accept(restated);

    assert!(outcome.is_conflict());
    let Acceptance::Conflict { difference } = outcome else {
        panic!("expected a conflict");
    };
    assert!(difference.contains("total energy"), "{difference}");

    // And the settled number is untouched.
    assert_eq!(ledger.get(&cdr.key).unwrap().total_energy, kwh("18.000"));
}

#[test]
fn a_session_overstating_its_authorisation_cannot_produce_a_cdr() {
    // An ad-hoc session whose signed record claims a secure, certificate-backed
    // identity. Two stories about one event.
    let mut session = session();
    session.authorization = Authorization::ad_hoc();

    let evidence = evidence_of(&raw_records());
    let mut overstated = evidence_ref(&evidence);
    overstated.identification_strength = IdentificationStrength::Secure;
    assert_eq!(
        evidence.identification_strength(),
        Some(IdentificationStrength::Trusted),
        "the station's own records say TRUSTED; the overstatement is the fixture's"
    );

    let err = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(overstated)
        .build()
        .unwrap_err();

    assert!(
        err.to_string().contains("claims ad-hoc authorisation"),
        "{err}"
    );
}

// ── The last leg: from a settled record to money ────────────────────────────

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// A lawful fast-charger ad-hoc tariff: per kWh, with an occupancy fee on top —
/// exactly the shape `[AFIR Art. 5(4)]` permits at 50 kW and above.
fn fast_charger_tariff() -> Tariff {
    Tariff::simple(
        "ad-hoc-dc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")).with_vat(dec("19")),
        ],
    )
}

#[test]
fn the_price_shown_is_the_price_charged_and_it_rides_on_the_record() {
    // The whole claim in one test: a signed kilowatt-hour becomes a quarter
    // hour becomes a line becomes a taxable amount, and every step is checkable
    // against the one before it.
    let evidence = evidence_of(&raw_records());
    let session = session();
    let tariff = fast_charger_tariff();

    // 1. What a driver approaching the charger is shown, before anything
    //    happens — derived from the tariff that will rate them.
    let shown = describe(&tariff, session.started_at);
    assert_eq!(shown.one_line(), "0.49 EUR / kWh · 0.10 EUR / min");
    assert_eq!(shown.per_kwh(), Some(dec("0.49")));
    assert!(!shown.varies_by_condition());

    // 2. The record, priced from its own charging periods.
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&tariff)
        .build()
        .unwrap();

    let cost = cdr.cost.as_ref().expect("the record was rated");
    assert_eq!(cost.tariff_id.as_str(), "ad-hoc-dc");
    assert_eq!(
        cost.rated.lines[0].unit_price,
        shown.per_kwh().unwrap(),
        "the displayed price and the charged price are the same number"
    );

    // 3. Every kilowatt-hour priced is a kilowatt-hour the evidence signed.
    assert_eq!(
        cost.rated.quantity_for(Dimension::Energy),
        evidence.billable_energy().unwrap().kwh(),
    );

    // 4. 18.000 kWh at 0.49 gross, with the taxable amount an invoice needs.
    assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
    assert_eq!(cost.rated.net().to_string(), "7.41 EUR");
    assert_eq!(cost.rated.tax().to_string(), "1.41 EUR");
    let summary = cost.rated.tax_summary();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].rate, dec("19"));
    assert_eq!(summary[0].net + summary[0].tax, summary[0].gross);

    // 5. And the partner who receives it finds the price consistent with the
    //    energy.
    let report = validate(&cdr);
    assert!(report.is_settleable(), "{:?}", report.findings);
    assert!(report.findings.is_empty(), "{:?}", report.findings);
}

#[test]
fn a_tiered_tariff_prices_the_quarter_hours_the_split_produced() {
    // The two halves of the workspace meeting: the split that conserves energy
    // exactly is the same list of periods the tariff walks to apply its tiers.
    let evidence = evidence_of(&raw_records());
    let tiered = Tariff {
        id: "tiered".parse().unwrap(),
        currency: Currency::EUR,
        kind: TariffKind::AdHoc,
        tax_included: emob_tariff::TaxIncluded::Yes,
        elements: vec![
            emob_tariff::TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: emob_tariff::Restrictions {
                    max_kwh: Some(dec("10")),
                    ..emob_tariff::Restrictions::default()
                },
            },
            emob_tariff::TariffElement::unrestricted(vec![PriceComponent::new(
                Dimension::Energy,
                dec("0.59"),
            )]),
        ],
        min_price: None,
        max_price: None,
        valid_from: None,
        valid_until: None,
    };

    let cdr = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&tiered)
        .build()
        .unwrap();

    let rated = &cdr.cost.as_ref().unwrap().rated;
    assert_eq!(rated.lines.len(), 2, "two tiers, two lines");
    assert_eq!(rated.lines[0].quantity, dec("10.000")); // the 10:00 slot
    assert_eq!(rated.lines[1].quantity, dec("8.000")); // the 10:15 slot
    assert_eq!(
        rated.quantity_for(Dimension::Energy),
        cdr.total_energy.kwh(),
        "the tiers partition the session and lose nothing"
    );
    assert_eq!(rated.gross().to_string(), "8.62 EUR"); // 3.90 + 4.72
}

#[test]
fn a_fast_charger_may_not_offer_a_per_minute_only_tariff() {
    let lawful = fast_charger_tariff();
    assert!(check_afir(&lawful, dec("150")).is_lawful());

    // The same operator, the same charger, priced by the minute alone. The
    // hourly rate divides exactly into minutes — 0.30 an hour is 0.005 a
    // minute — so the only thing wrong with it is the article's own rule.
    let unlawful = Tariff::simple(
        "ad-hoc-dc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![PriceComponent::new(Dimension::Time, dec("0.30"))],
    );
    let verdict = check_afir(&unlawful, dec("150"));
    assert!(!verdict.is_lawful());
    assert!(
        verdict.reasons().any(|r| r.contains("price per kWh")),
        "{:?}",
        verdict.reasons().collect::<Vec<_>>()
    );

    // …and the identical tariff is fine on a 22 kW post.
    assert!(check_afir(&unlawful, dec("22")).is_lawful());
}

#[test]
fn an_unbillable_session_never_reaches_a_price() {
    // The chain's central promise, carried to the end: a tampered record has no
    // energy, so there is nothing for a tariff to rate.
    let mut raw = raw_records();
    raw[2] = raw[2].replace("118.000", "218.000");
    let evidence = evidence_of(&raw);

    assert_eq!(evidence.billable_energy(), None);
    // There is no path from here to a number: the only energy this session has
    // is the `None` above, and a `Chargeable` cannot be built from it.
    assert!(!evidence.is_billable());
}

#[test]
fn a_v2g_discharge_cannot_be_billed_as_a_draw() {
    // The invariant this workspace claims from its first commit — import and
    // export never net — finally checkable against the evidence rather than
    // only against the session model. `[OCMF Tab. 25]` reserves `C2` for
    // transaction export, so the signed register states which way the energy
    // went, and the CDR is refused if it claims the other.
    let discharge: Vec<String> = raw_records()
        .iter()
        .map(|r| {
            let payload = r
                .split('|')
                .nth(1)
                .unwrap()
                .replace("01-00:B2.08.00*FF", "01-00:C2.08.00*FF");
            let digest = Sha256::digest(payload.as_bytes());
            let sig: DerSignature = signing_key().sign_prehash(&digest).unwrap();
            format!(
                "OCMF|{payload}|{{\"SD\":\"{}\"}}",
                hex::encode(sig.as_bytes())
            )
        })
        .collect();

    let evidence = evidence_of(&discharge);
    assert_eq!(evidence.direction(), Some(Direction::Export));
    assert_eq!(
        evidence.billable_energy().unwrap().to_string(),
        "18.000 kWh",
        "the energy is real; only the direction is not what the session claims"
    );

    // The session was assembled as an import — which is the bug, and nothing
    // downstream of the register could have seen it.
    let err = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("never net"), "{err}");
}

#[test]
fn the_driver_can_repeat_the_whole_check_in_software_nobody_here_wrote() {
    // What `[MessEG §33]` actually requires: not that the value is right, but
    // that the affected party can check it. The deliverable is a file the
    // S.A.F.E. Transparenzsoftware reads.
    let evidence = evidence_of(&raw_records());
    let xml = transparency::to_xml(&evidence);

    // One dataset per signed record, each verbatim, each beside the key it was
    // checked against — which is the key the registry supplied out of band,
    // not one chosen to make the file verify.
    assert_eq!(xml.matches("<value ").count(), 3);
    for raw in raw_records() {
        assert!(xml.contains(&raw), "the record must survive verbatim");
    }
    assert!(xml.contains("context=\"Transaction.Begin\""));
    assert!(xml.contains("context=\"Transaction.End\""));
    assert!(xml.contains("format=\"OCMF\" encoding=\"plain\""));

    // And the file is the same artefact two years later: nothing in the export
    // path reads a clock or a network.
    assert_eq!(transparency::to_xml(&evidence_of(&raw_records())), xml);
}

#[test]
fn an_unsynchronised_clock_bills_the_energy_and_refuses_the_occupancy_fee() {
    // The distinction OCMF carries and nothing in the field reads. Same
    // session, same signatures, clock status `U` instead of `S`.
    let raw: Vec<String> = raw_records()
        .iter()
        .map(|r| {
            let payload = r
                .split('|')
                .nth(1)
                .unwrap()
                .replace(":00,000+0100 S", ":00,000+0100 U");
            let digest = Sha256::digest(payload.as_bytes());
            let sig: DerSignature = signing_key().sign_prehash(&digest).unwrap();
            format!(
                "OCMF|{payload}|{{\"SD\":\"{}\"}}",
                hex::encode(sig.as_bytes())
            )
        })
        .collect();

    let evidence = evidence_of(&raw);
    assert_eq!(
        evidence.billable_energy().unwrap().to_string(),
        "18.000 kWh",
        "the register is untouched by a bad clock"
    );
    assert!(!evidence.is_billable_for_time());

    let session = session();
    let occupancy = fast_charger_tariff();
    let err = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&occupancy)
        .build()
        .unwrap_err();
    assert!(
        err.to_string().contains("price this session per kWh"),
        "{err}"
    );

    // Per kWh, the same session settles for the same 8.82 as always.
    let per_kwh = Tariff::simple(
        "ad-hoc-dc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    );
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&per_kwh)
        .build()
        .unwrap();
    assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
}

#[test]
fn a_priced_record_survives_the_wire_with_its_reasons_intact() {
    // A note is a term of the price. One that stays behind in the process that
    // produced it is a note nobody can invoke in a dispute, so it has to
    // survive serialisation.
    let evidence = evidence_of(&raw_records());
    let mut tariff = fast_charger_tariff();
    tariff.min_price = Some(dec("15.00"));

    let cdr = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&tariff)
        .build()
        .unwrap();

    assert_eq!(cdr.total_cost().unwrap().to_string(), "15.00 EUR");

    let json = serde_json::to_string(&cdr).unwrap();
    let back: emob_cdr::Cdr = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cdr);
    assert!(
        back.cost
            .as_ref()
            .unwrap()
            .rated
            .reasons()
            .any(|r| r.contains("minimum")),
        "the reason the driver paid 15.00 rather than 8.82 travels with the record"
    );

    // …and the record is one the *other party* can read, which a round trip
    // cannot tell you. `time`'s own serialisation writes an instant as a
    // nine-element array of its internal fields and a currency newtype writes
    // three bytes — both round-trip through this crate perfectly and neither is
    // a record two companies settle against.
    assert!(
        json.contains(r#""started_at":"2026-01-02T10:00:00+01:00""#),
        "instants are RFC 3339, as every wire this stack meets writes them: {json}"
    );
    assert!(
        json.contains(r#""quarter_hour":"2026-01-02T10:00:00+01:00""#),
        "…settlement slots included: {json}"
    );
    assert!(
        json.contains(r#""currency":"EUR""#),
        "a currency is its ISO 4217 code, not its bytes: {json}"
    );
    assert!(
        json.contains(r#""total_energy":"18.000""#),
        "and the meter's own scale survives as a string rather than a float: {json}"
    );
    assert!(
        !json.contains("[2026,"),
        "no field may go out in a shape only this codebase can read: {json}"
    );
}

// ── A meter that exists ─────────────────────────────────────────────────────
//
// Everything above signs its own fixtures, which proves this workspace agrees
// with itself. The two tests below run a record produced by an **eBZ LD3** — an
// ordinary German charging meter — against the key it is published with, taken
// from the reference data set the S.A.F.E. Transparenzsoftware ships
// (`ocmf-compleo-daten.xml`, © S.A.F.E. e.V., Apache-2.0).
//
// It is a different question from "does the code agree with itself", and it is
// the one that decides whether a driver's own verifier and this platform reach
// the same answer about the same bill. Three things about it are not what a
// self-signed fixture would produce, and each of them broke this workspace
// until it was fixed:
//
//   * it is signed on **secp192r1**, not one of the 256-bit curves;
//   * its DER signature pads both integers to a fixed 24 bytes, so `r` reads as
//     a negative number to any strict parser;
//   * its clock is `I` — informative — so the energy is billable and the
//     duration is not, which is precisely the distinction the chain keeps.

/// The record, verbatim, pipes and all.
const EBZ_LD3_RECORD: &str = concat!(
    r#"OCMF|{"FV":"1.0","GI":"eBZ LD3","GS":"1EBZ0300034628","GV":"V207","MS":"1EBZ0300034628","PG":"T120","IS":true,"IL":"TRUSTED","IT":"EMAID","ID":"DKV_Testbox2","CI":"Dies","CT":"EVSEID","#,
    r#""RD":[{"TM":"2022-10-27T19:38:50,000+0200 I","TX":"B","RV":2851485,"RI":"1-b:1.8.e","RU":"Wh","ST":"G"},"#,
    r#"{"TM":"2022-10-27T19:43:38,000+0200 I","TX":"E","RV":2851753,"RI":"1-b:1.8.e","RU":"Wh","ST":"G"}]}"#,
    r#"|{"SA":"ECDSA-secp192r1-SHA256","SD":"30340218e10a077929f593717affdff69a5df0d2862989a6638f873d0218007d21b1c0255c5b24a3c5a01d600839ebae2bb67bcb1159"}"#,
);

/// The public key it is published with — a DER `SubjectPublicKeyInfo` on
/// `prime192v1`, which is how a type-approval document hands one over.
const EBZ_LD3_KEY: &str = concat!(
    "3049301306072a8648ce3d020106082a8648ce3d03010103320004",
    "1e155ef46fbcc56005769c08d792127c006c242ccccd96bf",
    "7051b6fbc278497036659e7bae57f542776a17c7f8b28600",
);

fn ebz_evidence() -> Evidence {
    let mut registry = KeyRegistry::new();
    registry.insert(
        ComponentRef::Meter {
            serial: "1EBZ0300034628".into(),
        },
        RegisteredKey::unbounded(
            PublicKey::from_hex(KeyType::Secp192r1, EBZ_LD3_KEY).unwrap(),
            "S.A.F.E. reference data set",
        ),
    );
    let record = ocmf::parse(EBZ_LD3_RECORD).unwrap();
    Evidence::assemble(&[record], &registry, datetime!(2022-10-27 19:38 +2))
}

#[test]
fn a_record_from_a_real_german_meter_verifies_and_bills() {
    let evidence = ebz_evidence();

    // The clock is informative, so there is something to say — and it is about
    // the duration, not the register.
    assert!(
        evidence
            .reasons()
            .all(|r| r.contains("energy is unaffected")),
        "{:?}",
        evidence.reasons().collect::<Vec<_>>()
    );

    // 2851753 − 2851485 = 268 Wh, and the Wh register's one-watt-hour
    // resolution survives into the invoice rather than becoming 0.268.
    assert_eq!(evidence.billable_energy().unwrap().to_string(), "0.268 kWh");
    assert_eq!(evidence.direction(), Some(Direction::Import));
    assert_eq!(
        evidence.identification_strength(),
        Some(IdentificationStrength::Trusted)
    );

    // …and the duration is not billable, because `TM` says the clock was only
    // informative. A per-minute occupancy fee on this session would be a
    // number nobody can defend.
    assert!(!evidence.is_billable_for_time());
}

#[test]
fn the_driver_of_a_real_meter_gets_a_file_their_own_verifier_reads() {
    let xml = transparency::to_xml(&ebz_evidence());

    // The record verbatim, beside the key the registry supplied — not one
    // chosen to make the file verify.
    assert!(xml.contains(EBZ_LD3_RECORD));
    assert!(xml.contains(&EBZ_LD3_KEY.to_uppercase()));
    assert!(xml.contains(r#"transactionId="120""#));

    // …and *no* context label, because this meter puts the whole transaction in
    // one signed data set: `TX=B` and `TX=E` are two readings of one record, so
    // it is neither a begin nor an end. The reference sample files omit the
    // attribute for exactly this shape, and the reference verifier pairs the
    // readings itself.
    assert!(
        !xml.contains("context="),
        "a whole transaction is not half of one: {xml}"
    );
}

#[test]
fn the_record_names_the_tariff_by_content_and_it_survives_the_wire() {
    // A tariff id is a name and names get reused. The receiving party's real
    // question before it re-rates is not "does the id match" but "is the
    // tariff I hold the one that produced these euros" — and the answer has to
    // survive serialisation, because that is where the question is asked.
    let evidence = evidence_of(&raw_records());
    let tariff = fast_charger_tariff();

    let cdr = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&tariff)
        .build()
        .unwrap();

    assert!(cdr.was_priced_with(&tariff));

    let back: emob_cdr::Cdr = serde_json::from_str(&serde_json::to_string(&cdr).unwrap()).unwrap();
    assert!(
        back.was_priced_with(&tariff),
        "the fingerprint has to cross the wire, or the question cannot be asked where it matters"
    );

    // The same id, one price edited in place. The id says nothing; the
    // fingerprint says everything.
    let mut edited = fast_charger_tariff();
    edited.elements[0].components[0].price = dec("0.59");
    assert_eq!(edited.id, tariff.id);
    assert!(!back.was_priced_with(&edited));
}

#[test]
fn a_price_change_does_not_reach_back_into_a_session_already_running() {
    // `[AFIR Art. 5(4)]`: the ad-hoc price must be known to the driver "before
    // they initiate a recharging session". A CPO that raises its price at
    // 10:15 has not raised it for the driver who plugged in at 10:00.
    let evidence = evidence_of(&raw_records());

    let before = fast_charger_tariff().valid_between(None, Some(at(15)));
    let after = {
        let mut t = fast_charger_tariff();
        t.elements[0].components[0].price = dec("0.99");
        t.valid_between(Some(at(15)), None)
    };
    let history = emob_tariff::TariffHistory::new(vec![before.clone(), after]).unwrap();

    let cdr = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with_history(&history)
        .unwrap()
        .build()
        .unwrap();

    assert!(
        cdr.was_priced_with(&before),
        "the version in force at 10:00 is the one the driver was shown"
    );
    // 18.000 kWh at 0.49, not at 0.99.
    assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
    assert!(validate(&cdr).is_settleable());
}

#[test]
fn the_two_ways_a_duration_stops_being_billable_are_both_enforced() {
    // `[OCMF Tab. 19]` and `[REA 6-A §3.1]` are the same rule from opposite
    // ends: there the clock cannot be *placed*, here the span cannot be
    // *resolved*. Both leave a session whose register an invoice may use and
    // whose duration it may not — which is why the two quantities were
    // separated in the first place.
    let evidence = evidence_of(&raw_records());
    let occupancy = fast_charger_tariff();

    // The session in `session()` runs half an hour on a synchronised clock, so
    // both gates open and the occupancy fee is charged.
    let ok = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .rated_with(&occupancy)
        .build()
        .unwrap();
    assert!(ok.total_cost().is_some());

    // Close the first gate: the signed records no longer vouch for the clock.
    let mut unplaceable = evidence_ref(&evidence);
    unplaceable.duration_billable = false;
    let err = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(unplaceable)
        .rated_with(&occupancy)
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("price this session per kWh"));

    // Close the second: the clock cannot resolve a span this long, so nothing
    // it measured is long enough to bill. The session is genuinely brief —
    // readings and all — because moving only `ended_at` would leave a meter
    // series running past the session's own window, which is a different fault
    // and one the builder now refuses first.
    let coarse = emob_core::ClockResolution::conforming();
    let mut brief = Session::open(
        "s-brief".parse().unwrap(),
        "DE*AB7*E840*6487".parse().unwrap(),
        Authorization::ad_hoc(),
        at(0),
    );
    brief.transition_to(SessionState::Charging, at(0)).unwrap();
    brief
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(0) + time::Duration::seconds(30),
                        kwh("100.400"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    brief
        .end(at(0) + time::Duration::seconds(30), EndReason::Local)
        .unwrap();

    let err = CdrBuilder::from_session(&brief, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .clock(coarse)
        .rated_with(&occupancy)
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("its clock can resolve"), "{err}");
    assert!(err.to_string().contains("price this session per kWh"));
}
