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
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::ocmf::KeyType;
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry, PublicKey, ocmf};
use emob_session::{
    Authorization, EndReason, IdentificationStrength, MeterReading, MeterSeries, ReadingContext,
    Session, SessionState,
};
use emob_tariff::{
    Chargeable, Dimension, PriceComponent, Tariff, TariffKind, check_afir, describe, rate,
};
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
                emaid: Some("DE-8AA-CA2B3C4D5-1".parse().unwrap()),
            },
            token_ref: Some(emob_session::TokenRef::new("a".repeat(64)).unwrap()),
            authorization_reference: Some("auth-9".into()),
        },
        at(0),
    );
    session.transition_to(SessionState::Charging).unwrap();
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

fn evidence_ref(evidence: &Evidence) -> EvidenceRef {
    EvidenceRef {
        encoding_method: "OCMF".into(),
        payload_digests: evidence.verified.iter().map(|v| v.payload_digest).collect(),
        identification_strength: IdentificationStrength::Trusted,
    }
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
    session.transition_to(SessionState::Charging).unwrap();
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
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ],
    )
}

#[test]
fn the_price_shown_is_the_price_charged() {
    // The property the whole tariff crate exists for, exercised on a record
    // that came out of the real chain rather than a fixture.
    let evidence = evidence_of(&raw_records());
    let session = session();
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(evidence_ref(&evidence))
        .build()
        .unwrap();

    let tariff = fast_charger_tariff();

    // What a driver approaching the charger is shown, before anything happens.
    let shown = describe(&tariff, cdr.started_at);
    assert_eq!(shown.one_line(), "0.49 EUR / kWh · 0.10 EUR / min");
    assert_eq!(shown.per_kwh(), Some(dec("0.49")));

    // What the session is actually rated at, from the CDR the chain produced.
    let rated = rate(
        &tariff,
        &Chargeable::energy_only(cdr.total_energy, cdr.started_at),
    );

    assert_eq!(
        rated.lines[0].unit_price,
        shown.per_kwh().unwrap(),
        "the displayed price and the charged price are the same number"
    );
    // 18.000 kWh at 0.49.
    assert_eq!(rated.total().to_string(), "8.82 EUR");
    assert!(rated.lines_sum_to_total());
    assert!(rated.notes.is_empty(), "{:?}", rated.notes);
}

#[test]
fn a_fast_charger_may_not_offer_a_per_minute_only_tariff() {
    let lawful = fast_charger_tariff();
    assert!(check_afir(&lawful, dec("150")).is_lawful());

    // The same operator, the same charger, priced by the minute alone.
    let unlawful = Tariff::simple(
        "ad-hoc-dc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![PriceComponent::new(Dimension::Time, dec("0.35"))],
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
    // There is no path from here to a number: `rate` takes a `Chargeable`, and
    // the only energy this session has is the `None` above.
    assert!(!evidence.is_billable());
}
