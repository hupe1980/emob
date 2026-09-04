//! One session, three ways out, and the same money on each.
//!
//! The property M4 exists to prove: a session settles **identically** whether
//! it stays inside one company, goes to a partner on OCPI 2.3.0, or goes to
//! one still on 2.2.1. Going multi-party changes the transport and nothing
//! about the arithmetic — and where it *does* change something, the record
//! says so rather than the two sides finding out from a reconciliation report.
//!
//! Every fixture is signed for real: the OCMF records are produced with a
//! private key at test time and verified through the same code path a station
//! would go through, so this proves the path rather than a constant.

use emob_cdr::{CdrBuilder, EvidenceRef};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_poi::site::{
    Address, ChargingPoint, Connector, ConnectorType, Coordinates, Facility, Site,
};
use emob_roam::ocpi::cdr::{Context, SignedPayload, check_conserves, downgrade, to_ocpi};
use emob_roam::ocpi::inbound::from_ocpi;
use emob_roam::ocpi::location::cdr_location;
use emob_roam::{Partner, PartnerRegistry, Reach, RoamError, RoamingToken, TokenType};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::Curve;
use ocmf::PublicKey;
use ocpi_kit::types::Validate;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::datetime;

const METER_SERIAL: &str = "BQ27400330016";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(dec(s)).unwrap()
}

fn at(minute: i64) -> time::OffsetDateTime {
    datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
}

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
    registry
        .insert(
            ComponentRef::GatewayAndMeter {
                gateway: "GW-1".into(),
                meter: METER_SERIAL.into(),
            },
            RegisteredKey::unbounded(
                PublicKey::from_sec1(
                    Curve::Secp256r1,
                    signing_key()
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes(),
                )
                .expect("a well-formed SEC1 point"),
                "type approval 2026-01",
            ),
        )
        .unwrap();
    registry
}

fn raw_records() -> Vec<String> {
    vec![
        signed_record(1, "B", "100.000", 0),
        signed_record(2, "C", "110.000", 15),
        signed_record(3, "E", "118.000", 30),
    ]
}

/// A 30-minute session on a contract, 18.000 kWh, ending at 10:30.
fn session() -> Session {
    let mut session = Session::open(
        "s-1".parse().unwrap(),
        "DE*AB7*E840*6487".parse().unwrap(),
        Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: "NL-TNM-C00122045-K".parse().unwrap(),
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

fn tariff() -> Tariff {
    Tariff::simple(
        "ad-hoc-2026".parse().unwrap(),
        Currency::new("EUR").unwrap(),
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

fn evidence() -> Evidence {
    // The texts have to outlive the records: a `Record` borrows the bytes its
    // signature covers, which is the format's central rule.
    let texts = raw_records();
    let records = texts
        .iter()
        .map(|r| ocmf::Record::parse(r))
        .collect::<Result<Vec<_>, _>>()
        .expect("well-formed OCMF");
    Evidence::assemble(&records, &registry(), at(0))
}

fn cdr(evidence: &Evidence) -> emob_cdr::Cdr {
    let binding = tariff();
    CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(evidence, "OCMF"))
        .rated_with(&binding)
        .build()
        .expect("a genuine session builds a CDR")
}

fn site() -> Site {
    Site {
        facility: Facility::new("site-1"),
        name: "Autohof Nord".to_owned(),
        coordinates: Coordinates {
            latitude: dec("52.520008"),
            longitude: dec("13.404954"),
        },
        address: Address {
            street: "Hauptstraße".to_owned(),
            house_number: "12".to_owned(),
            postcode: "10719".to_owned(),
            city: "Berlin".to_owned(),
            country_code: "DE".to_owned(),
        },
        time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        stations: Vec::new(),
    }
}

fn point() -> ChargingPoint {
    ChargingPoint::new(
        Facility::new("evse-1"),
        "DE*AB7*E840*6487".parse().unwrap(),
        Connector::new(ConnectorType::Iec62196T2Combo, dec("150")),
    )
}

fn token() -> RoamingToken {
    RoamingToken::new(
        PartyId::new("NL", "TNM").unwrap(),
        "045F2C9A",
        TokenType::Rfid,
        "NL-TNM-C00122045-K".parse().unwrap(),
    )
    .expect("the reference contract id checks out")
}

fn context<'a>(token: &'a RoamingToken, signed: Vec<SignedPayload>) -> Context<'a> {
    let point = point();
    Context {
        token,
        location: cdr_location(&site(), &point, &point.connectors[0], 0).unwrap(),
        signed,
        public_key: Some(hex::encode(
            signing_key()
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        )),
        last_updated: at(31),
    }
}

fn payloads() -> Vec<SignedPayload> {
    raw_records()
        .iter()
        .zip(["Start", "Intermediate", "End"])
        .map(|(record, nature)| SignedPayload {
            nature: (*nature).to_owned(),
            plain_data: record.clone(),
            signed_data: record.clone(),
        })
        .collect()
}

#[test]
fn one_session_settles_at_the_same_money_on_every_wire() {
    let evidence = evidence();
    let cdr = cdr(&evidence);

    // What the session is worth, computed once, from its own quarter hours.
    assert_eq!(cdr.total_energy.to_string(), "18.000 kWh");
    let gross = cdr.total_cost().unwrap();
    assert_eq!(gross.to_string(), "8.82 EUR");

    let token = token();
    let context = context(&token, payloads());
    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data();

    // ── The 2.3.0 leg ────────────────────────────────────────────────────
    let crossing = to_ocpi(&cdr, &partner, &context).expect("a rated import CDR crosses");
    let wire = &crossing.value;

    assert_eq!(wire.total_energy.get().to_string(), "18.000");
    assert_eq!(
        wire.total_cost.after_taxes().get(),
        gross.amount(),
        "the euros on the wire are the euros the tariff produced, not a re-derivation"
    );
    assert_eq!(wire.currency.as_str(), "EUR");
    assert_eq!(wire.charging_periods.len(), 2);
    assert_eq!(wire.cdr_token.contract_id.as_str(), "NL-TNM-C00122045-K");

    // The periods still sum to the total after the crossing — the first thing
    // a receiving party checks, and the thing the canonical record guarantees.
    assert_eq!(
        check_conserves(wire).unwrap().to_string(),
        "18.000 kWh",
        "a crossing that loses a kilowatt-hour is a crossing that loses money"
    );

    // …and the document is one OCPI itself accepts.
    assert!(
        wire.validate().is_ok(),
        "{:?}",
        wire.validate().unwrap_err().iter().collect::<Vec<_>>()
    );

    // ── The 2.2.1 leg ────────────────────────────────────────────────────
    let older = downgrade(crossing.clone());
    assert_eq!(
        older.value.total_energy.get().to_string(),
        "18.000",
        "the same session, and the same energy, on the version most of the market runs"
    );
    assert!(
        older.notes().len() >= crossing.notes().len(),
        "a downgrade never explains less than the crossing it came from"
    );

    // ── Self-roaming ─────────────────────────────────────────────────────
    // The same path with this operator as its own provider. Nothing about the
    // arithmetic may depend on who is on the other end.
    let myself = Partner::emsp(PartyId::new("DE", "ABC").unwrap());
    let mine = to_ocpi(&cdr, &myself, &context).expect("self-roaming is the same path");
    assert_eq!(mine.value.total_cost, wire.total_cost);
    assert_eq!(mine.value.total_energy, wire.total_energy);
    assert_eq!(mine.value.charging_periods, wire.charging_periods);
}

#[test]
fn the_record_comes_back_as_the_record_that_went_out() {
    // The other half of M4. A CDR that crosses to a partner has to be readable
    // by the party that receives it, and the only way to know that is to read
    // it back: same key, same session window, same periods, same energy — and
    // every place OCPI could not carry what the canonical model holds named
    // rather than quietly restored.
    let evidence = evidence();
    let original = cdr(&evidence);
    let token = token();
    let context = context(&token, payloads());
    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data();

    let wire = to_ocpi(&original, &partner, &context)
        .expect("a rated import CDR crosses")
        .into_value_discarding_notes();

    // The receiver verifies the payloads against **its own** registry — never
    // the key in the document — and only then reads the record in.
    let payloads = emob_roam::ocpi::cdr::inbound_payloads(&wire);
    assert_eq!(payloads.len(), 3, "the signed records arrived verbatim");
    let records: Vec<_> = payloads
        .iter()
        .map(|p| ocmf::Record::parse(&p.signed_data).expect("verbatim OCMF"))
        .collect();
    let theirs = Evidence::assemble(&records, &registry(), at(0));
    assert_eq!(
        theirs.billable_energy().unwrap().to_string(),
        "18.000 kWh",
        "the far end re-verifies against its own registry and gets the same energy"
    );

    let back = from_ocpi(&wire, Some(EvidenceRef::from_evidence(&theirs, "OCMF")))
        .expect("the document this crate wrote is one it can read");
    let read = &back.value.cdr;

    assert_eq!(read.key, original.key);
    assert_eq!(read.session_id, original.session_id);
    assert_eq!(
        read.authorization_reference, original.authorization_reference,
        "the provider's own reference for the authorisation it granted survives"
    );
    assert_eq!(
        wire.authorization_reference.as_ref().map(|r| r.as_str()),
        Some("auth-9"),
        "…because OCPI has a field for it [OCPI 2.3.0 §mod_cdrs_cdr_object]"
    );
    assert_eq!(read.evse_id, original.evse_id);
    assert_eq!(read.started_at, original.started_at);
    assert_eq!(read.ended_at, original.ended_at);
    assert_eq!(read.total_energy, original.total_energy);
    assert_eq!(read.direction, original.direction);
    assert!(read.conserves(), "the periods still sum to the total");
    assert_eq!(
        read.periods.iter().map(|p| p.energy).collect::<Vec<_>>(),
        original
            .periods
            .iter()
            .map(|p| p.energy)
            .collect::<Vec<_>>(),
        "every period's energy survives the round trip"
    );
    assert_eq!(
        read.periods
            .iter()
            .map(|p| (p.start, p.end))
            .collect::<Vec<_>>(),
        original
            .periods
            .iter()
            .map(|p| (p.start, p.end))
            .collect::<Vec<_>>(),
        "and so does every window, though OCPI carries only the starts"
    );

    // The partner's own figure, for the comparison an eMSP re-rating makes.
    assert_eq!(
        back.value.stated_total,
        original.total_cost().unwrap(),
        "what the CPO says it costs comes back beside the record, not inside it"
    );
    assert!(
        read.cost.is_none(),
        "a `Rated` cannot be rebuilt from totals with no unit prices, and inventing one \
         would make `validate` check this crate's arithmetic instead of the partner's"
    );

    // What the wire could not carry is on the record of the crossing rather
    // than silently restored.
    let reasons: Vec<String> = back.reasons().collect();
    assert!(
        reasons.iter().any(|r| r.contains("interpolated")),
        "provenance is not a field OCPI has: {reasons:?}"
    );
    assert!(
        read.periods
            .iter()
            .all(|p| p.provenance == emob_session::Provenance::Interpolated),
        "and the weaker answer is the one on the record"
    );

    // …and the record the far end holds is one its own pre-flight settles.
    let report = emob_cdr::validate(read);
    assert!(
        report.is_settleable(),
        "{:?}",
        report.blocking().collect::<Vec<_>>()
    );
}

#[test]
fn a_thirty_minute_session_has_an_exact_duration_and_a_twenty_minute_one_does_not() {
    // The arithmetic that decides whether the partner's re-rating can agree
    // with ours: an hour is 3600 seconds, 3600 has two factors of three, and
    // only durations that nine divides survive as decimals.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let context = context(&token, payloads());
    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data();

    let crossing = to_ocpi(&cdr, &partner, &context).unwrap();
    assert_eq!(
        crossing.value.total_time.get().to_string(),
        "0.5",
        "thirty minutes is exactly half an hour"
    );
    assert!(
        !crossing.reasons().any(|r| r.contains("/total_time")),
        "an exact duration has nothing to report: {:?}",
        crossing.reasons().collect::<Vec<_>>()
    );

    // Now the ordinary case. Twenty minutes is a third of an hour.
    assert_eq!(
        emob_roam::ocpi::cdr::exact_hours(30 * 60).map(|h| h.normalize()),
        Some(dec("0.5"))
    );
    assert_eq!(emob_roam::ocpi::cdr::exact_hours(20 * 60), None);
}

#[test]
fn a_partner_that_settles_on_signed_data_does_not_get_a_record_without_it() {
    // [MessEG §33]: a value the customer cannot check is not one they can be
    // billed for. The eMSP is the party that has to answer the driver.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let bare = context(&token, Vec::new());

    let strict = Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data();
    let err = to_ocpi(&cdr, &strict, &bare).unwrap_err();
    assert!(matches!(err, RoamError::SignedDataRequired { .. }), "{err}");

    // A partner that does not ask still gets told what it is missing, because
    // the digests are on this side and the payloads are not on the wire.
    let relaxed = Partner::emsp(PartyId::new("NL", "TNM").unwrap());
    let crossing = to_ocpi(&cdr, &relaxed, &bare).expect("not every partner asks");
    assert!(
        crossing.reasons().any(|r| r.contains("repeat the check")),
        "{:?}",
        crossing.reasons().collect::<Vec<_>>()
    );
}

#[test]
fn a_v2g_discharge_is_refused_rather_than_settled_backwards() {
    // OCPI's ENERGY_EXPORT is Session-only and `total_energy` has no sign, so
    // an export CDR arrives at the provider as an ordinary draw. Import and
    // export never net; a translation that re-signed one as the other would
    // break that at the last possible moment, on our side.
    let evidence = evidence();
    let mut export = cdr(&evidence);
    export.direction = Direction::Export;

    let token = token();
    let err = to_ocpi(
        &export,
        &Partner::emsp(PartyId::new("NL", "TNM").unwrap()),
        &context(&token, payloads()),
    )
    .unwrap_err();

    assert!(
        matches!(err, RoamError::ExportNotExpressible { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("pay the wrong way round"));
}

#[test]
fn an_unrated_record_cannot_cross_because_zero_means_free() {
    let evidence = evidence();
    let mut unrated = cdr(&evidence);
    unrated.cost = None;

    let token = token();
    let err = to_ocpi(
        &unrated,
        &Partner::emsp(PartyId::new("NL", "TNM").unwrap()),
        &context(&token, payloads()),
    )
    .unwrap_err();

    assert!(matches!(err, RoamError::NotRated), "{err}");
}

#[test]
fn the_record_is_routed_by_what_the_contract_itself_says() {
    // Not by a map somebody maintains beside it. The contract names NL*TNM,
    // and NL*TNM is who pays.
    let registry = PartnerRegistry::new(PartyId::new("DE", "ABC").unwrap())
        .with(Partner::hub(PartyId::new("DE", "HUB").unwrap()))
        .with(Partner::emsp(PartyId::new("NL", "TNM").unwrap()));

    let contract = token().contract_id;
    assert_eq!(
        registry.route(&contract),
        Some(Reach::Direct(PartyId::new("NL", "TNM").unwrap()))
    );

    // A provider this node does not peer with goes through the hub, and the
    // CPO still knows who the contract named.
    let elsewhere = "FR-XYZ-C00000001-4".parse().unwrap();
    assert!(matches!(
        registry.route(&elsewhere),
        Some(Reach::Hub { .. })
    ));
}

#[test]
fn the_signed_records_reach_the_partner_verbatim() {
    // The signature covers the bytes as written. A payload re-serialised on
    // the way through does not verify at the far end, and the partner's only
    // available conclusion is that the operator tampered with it.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let crossing = to_ocpi(
        &cdr,
        &Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data(),
        &context(&token, payloads()),
    )
    .unwrap();

    let received = emob_roam::ocpi::cdr::inbound_payloads(&crossing.value);
    assert_eq!(received.len(), 3);
    for (sent, back) in raw_records().iter().zip(&received) {
        assert_eq!(&back.signed_data, sent, "a byte moved");
    }

    // And they verify at the far end, against a registry the receiver holds —
    // never against the key the document carries, which is the artefact under
    // examination.
    let records = received
        .iter()
        .map(|p| ocmf::Record::parse(&p.signed_data))
        .collect::<Result<Vec<_>, _>>()
        .expect("what arrived is still OCMF");
    let theirs = Evidence::assemble(&records, &registry(), at(0));
    assert_eq!(
        theirs.billable_energy().unwrap().to_string(),
        "18.000 kWh",
        "the partner reaches the same kilowatt-hours from the same bytes"
    );

    // The key beside them is a claim and the crossing says so.
    assert!(
        crossing
            .reasons()
            .any(|r| r.contains("claim, not a binding")),
        "{:?}",
        crossing.reasons().collect::<Vec<_>>()
    );
}

#[test]
fn the_total_is_broken_out_per_dimension_so_the_partner_can_check_a_part_of_it() {
    // Most implementations fill `total_cost` alone, which leaves the receiver
    // able to disagree with the whole number and with no part of it.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let crossing = to_ocpi(
        &cdr,
        &Partner::emsp(PartyId::new("NL", "TNM").unwrap()),
        &context(&token, payloads()),
    )
    .unwrap();

    let energy = crossing
        .value
        .total_energy_cost
        .as_ref()
        .expect("an energy tariff produces an energy cost");
    assert_eq!(
        energy.after_taxes(),
        crossing.value.total_cost.after_taxes(),
        "one dimension priced this session, so its share is the whole of it"
    );
}

#[test]
fn the_cable_loss_the_meter_compensated_reaches_the_partner() {
    // `[OCMF Tab. 7, CL]` states how much of the register is cable rather than
    // vehicle. OCPI has no field for it, the compensation is already inside
    // `total_energy` so nothing is adjusted — and a partner disputing the
    // energy will ask exactly this. `[REA 6-A §3.2]` makes telling the
    // affected party what is inside a measured value a duty, not a courtesy.
    let evidence = evidence();
    let mut cdr = cdr(&evidence);
    cdr.evidence.as_mut().unwrap().compensated_loss = Some(Energy::from_kwh(dec("0.150")).unwrap());

    let token = token();
    let context = context(&token, payloads());
    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap());
    let crossing = to_ocpi(&cdr, &partner, &context).expect("a rated import CDR crosses");

    let notes: Vec<String> = crossing.reasons().collect();
    assert!(
        notes
            .iter()
            .any(|note| note.contains("cable loss") && note.contains("0.150")),
        "{notes:?}"
    );
    // …and the energy on the wire is untouched: the compensation is inside it.
    assert_eq!(
        crossing.value.total_energy.get(),
        cdr.total_energy.kwh(),
        "nothing is adjusted"
    );
}

#[test]
fn a_correction_crosses_as_a_credit_cdr_and_a_replacement_that_names_nothing() {
    // OCPI corrects a record in two documents [OCPI 2.3.0 §mod_cdrs_cdr_object]:
    // a Credit CDR — `credit = true`, `credit_reference_id` naming the
    // original, `total_cost` negated and nothing else — and then a new CDR
    // "with the fields `credit` and `credit_reference_id` omitted". The
    // replacement used to name the original in `credit_reference_id`, which is
    // the reversal's field and which `ocpi-kit`'s own validator refuses on a
    // record that is not one.
    let evidence = evidence();
    let original = cdr(&evidence);
    let token = token();
    let context = context(&token, payloads());
    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap());

    // The reversal.
    let credit = emob_roam::ocpi::cdr::to_ocpi_credit(&original, &partner, &context, "cdr-1-C")
        .expect("a rated record has a reversal");
    let wire = &credit.value;
    assert!(wire.is_credit());
    assert_eq!(wire.id.as_str(), "cdr-1-C");
    assert_eq!(
        wire.credit_reference_id.as_ref().map(|id| id.as_str()),
        Some("cdr-1")
    );
    let sent = to_ocpi(&original, &partner, &context).unwrap().value;
    assert_eq!(
        wire.total_cost.after_taxes().get(),
        -sent.total_cost.after_taxes().get(),
        "`total_cost` carries the negative of the original"
    );
    assert_eq!(
        wire.total_energy, sent.total_energy,
        "…and, as the specification prescribes, nothing else is negated"
    );
    assert_eq!(wire.total_energy_cost, sent.total_energy_cost);
    assert!(wire.validate().is_ok(), "{:?}", wire.validate().err());
    assert!(credit.reasons().any(|r| r.contains("Credit CDR")));

    // A Credit CDR is an instruction about a record already held, not a
    // session: read in, it would put the same kilowatt-hours in the ledger
    // twice. So the inbound side refuses it by name.
    let err = from_ocpi(wire, None).unwrap_err();
    assert!(
        matches!(&err, RoamError::CreditCdr { credit_reference_id } if credit_reference_id == "cdr-1"),
        "{err}"
    );

    // The replacement names nothing — the pairing is the ledger's — and the
    // crossing says so rather than putting the original's id in a field that
    // would make the document claim to be the reversal.
    let replacement = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-2".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&tariff())
        .supersedes(original.key.clone())
        .build()
        .unwrap();
    let crossing = to_ocpi(&replacement, &partner, &context).unwrap();
    assert!(!crossing.value.is_credit());
    assert!(crossing.value.credit_reference_id.is_none());
    assert!(
        crossing.value.validate().is_ok(),
        "{:?}",
        crossing.value.validate().err()
    );
    assert!(
        crossing
            .reasons()
            .any(|r| r.contains("supersedes") && r.contains("to_ocpi_credit")),
        "{:?}",
        crossing.reasons().collect::<Vec<_>>()
    );
    let back = from_ocpi(&crossing.value, None).unwrap();
    assert_eq!(back.value.cdr.supersedes, None);
}

/// The sum a receiver checks `total_cost` against has **five** terms.
///
/// `total_reservation_cost` is one of the per-dimension fields OCPI's own
/// pre-flight adds up `[OCPI 2.3.0 §mod_cdrs_cdr_object]`, and `total_cost` is
/// the session and the reservation together. The crossing's own reconciliation
/// summed four of the five and compared them against the **session's** total —
/// self-consistent, and therefore silent, and therefore incapable of noticing
/// anything about the fifth. It also meant the note it would have printed
/// quoted a `total_cost` the document does not state (D250).
///
/// What is asserted here is the receiver's own arithmetic, on the wire record.
#[test]
fn the_five_costs_on_the_wire_sum_to_the_total_it_states() {
    let evidence = evidence();
    let mut cdr = cdr(&evidence);
    let token = token();
    let context = context(&token, payloads());

    // A tariff that prices the hold as well as the session, and a half-hour
    // reservation before the cable went in.
    let mut priced = tariff();
    priced.elements.push(emob_tariff::TariffElement {
        components: vec![PriceComponent::new(Dimension::Time, dec("6.00")).with_vat(dec("19"))],
        restrictions: emob_tariff::Restrictions {
            reservation: Some(emob_tariff::ReservationRestriction::Reservation),
            ..emob_tariff::Restrictions::default()
        },
    });
    let held = emob_tariff::Reservation::honoured(
        cdr.started_at - time::Duration::minutes(30),
        cdr.started_at,
    );
    cdr.reservation = Some(held);
    cdr.cost.as_mut().unwrap().reservation = Some(emob_tariff::rate_reservation(&priced, &held));

    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data();
    let crossing = to_ocpi(&cdr, &partner, &context).expect("a rated import CDR crosses");
    let account: Vec<String> = crossing.reasons().collect();
    let record = crossing.into_value_discarding_notes();

    // The reservation is on the wire, in the field the specification keeps.
    let reserved = record
        .total_reservation_cost
        .as_ref()
        .expect("a priced reservation crosses");
    assert_eq!(reserved.after_taxes().get(), dec("3.00"));

    // `total_cost` is both parts — what the record itself says is owed.
    assert_eq!(
        record.total_cost.after_taxes().get(),
        cdr.total_cost().unwrap().amount(),
        "the wire states what the record says is owed"
    );

    // …and the five per-dimension fields add up to it, which is the sum a
    // receiving pre-flight performs.
    let parts: rust_decimal::Decimal = [
        record.total_energy_cost.as_ref(),
        record.total_time_cost.as_ref(),
        record.total_parking_cost.as_ref(),
        record.total_fixed_cost.as_ref(),
        record.total_reservation_cost.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|p| p.after_taxes().get())
    .sum();
    assert_eq!(
        parts,
        record.total_cost.after_taxes().get(),
        "a receiver summing the parts has to reach the total"
    );
    assert!(
        !account
            .iter()
            .any(|note| note.contains("per-dimension costs")),
        "the document adds up and the account should say nothing: {account:#?}"
    );
}
