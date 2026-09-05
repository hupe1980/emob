//! M4e's exit criterion: a record leaves for the party that will pay it, and
//! one that arrives is answered.
//!
//! Both crossings were built and proven before this service existed — one signed
//! session settles at the same money over OCPI 2.3.0, 2.2.1 and OICP, and a
//! partner's record re-rates into this side's invoice. What neither of them
//! could answer is the half that needs a **ledger of what was sent to whom**:
//!
//! 1. **Who is owed it**, routed out of the contract identifier's own issuer.
//! 2. **Whether it may be sent at all** — `[OCPI 2.3.0 §mod_cdrs]` seals a CDR
//!    once the eMSP has taken it, and a correction has an order.
//! 3. **Which records are late**, against the window *that partner* agreed to
//!    rather than a constant, because the same paragraph makes the cadence a
//!    contract between the two parties.
//! 4. **What to do with one that arrives** — accept, dispute, or refuse, with a
//!    retry told apart from a restatement.
//!
//! Every payload below is a domain crate's. This service decides who, when, and
//! whether it arrived, and nothing about what a record says.

use emob_cdr::{CdrBuilder, CdrKey, EvidenceRef};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_roam::ocpi::cdr::{Context, Outbound, SignedPayload};
use emob_roam::{OcpiVersion, Partner, PartnerRegistry, Reach, RoamingToken, TokenType, Wire};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::{Curve, PublicKey};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use roamd::{Delivery, DispatchError, Roamd, Verdict};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::datetime;

const METER_SERIAL: &str = "BQ27400330016";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("a decimal literal")
}

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(dec(s)).expect("a non-negative energy")
}

fn at(minute: i64) -> time::OffsetDateTime {
    datetime!(2026-06-02 10:00 +2) + time::Duration::minutes(minute)
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x42u8; 32].into()).expect("a valid scalar")
}

/// A genuinely signed record, so the far end verifies rather than trusts.
fn signed_record(pagination: u64, marker: &str, register: &str, minute: i64) -> String {
    let payload = format!(
        r#"{{"FV":"1.4","GI":"ACME CS-1","GS":"GW-1","PG":"T{pagination}","MV":"Phoenix Contact","MM":"EEM-350-D-MCB","MS":"{METER_SERIAL}","IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","RD":[{{"TM":"2026-06-02T{:02}:{:02}:00,000+0200 S","TX":"{marker}","RV":{register},"RI":"01-00:B2.08.00*FF","RU":"kWh","RT":"AC","EF":"","ST":"G"}}]}}"#,
        10 + (minute / 60),
        minute % 60,
    );
    let digest = Sha256::digest(payload.as_bytes());
    let signature: DerSignature = signing_key().sign_prehash(&digest).expect("a signature");
    format!(
        "OCMF|{payload}|{{\"SD\":\"{}\"}}",
        hex::encode(signature.as_bytes())
    )
}

fn raw_records() -> Vec<String> {
    vec![
        signed_record(1, "B", "100.000", 0),
        signed_record(2, "E", "129.500", 30),
    ]
}

fn key_registry() -> KeyRegistry {
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
        .expect("one key per component");
    registry
}

fn session() -> Session {
    let mut session = Session::open(
        "s-roam".parse().expect("a valid session id"),
        "DE*AB7*E840*6487".parse().expect("a valid EVSE id"),
        Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: "c-1".parse().expect("a valid contract id"),
                emaid: Some("NL-TNM-C00122045-K".parse().expect("a valid eMAID")),
            },
            token_ref: Some(emob_session::TokenRef::new("a".repeat(64)).expect("a digest")),
            authorization_reference: Some("auth-77".into()),
        },
        at(0),
    );
    session
        .transition_to(SessionState::Charging, at(0))
        .expect("a session charges");
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
                        at(30),
                        kwh("129.500"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    )
                    .signed(),
                ],
            )
            .expect("an ascending series"),
        )
        .expect("one series per direction");
    session.end(at(30), EndReason::Local).expect("it ends");
    session
}

/// What the CPO charges the provider.
fn wholesale() -> Tariff {
    Tariff::simple(
        "cpo-ad-hoc".parse().expect("a valid tariff id"),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

/// What the eMSP charges its own driver.
fn retail() -> Tariff {
    Tariff::simple(
        "emsp-retail".parse().expect("a valid tariff id"),
        Currency::EUR,
        TariffKind::Contract,
        emob_core::TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        vec![PriceComponent::new(Dimension::Energy, dec("0.59")).with_vat(dec("19"))],
    )
}

fn token() -> RoamingToken {
    RoamingToken::new(
        PartyId::new("NL", "TNM").expect("a valid party"),
        "045F2C9A",
        TokenType::Rfid,
        "NL-TNM-C00122045-K".parse().expect("a valid contract id"),
    )
    .expect("a routable token")
}

fn payloads() -> Vec<SignedPayload> {
    raw_records()
        .iter()
        .zip(["Start", "End"])
        .map(|(record, nature)| SignedPayload {
            nature: (*nature).to_owned(),
            plain_data: record.clone(),
            signed_data: record.clone(),
        })
        .collect()
}

fn evidence() -> Evidence {
    let texts = raw_records();
    let records: Vec<_> = texts
        .iter()
        .map(|r| ocmf::Record::parse(r).expect("well-formed OCMF"))
        .collect();
    Evidence::assemble(&records, &key_registry(), at(0))
}

fn issued(id: &str, supersedes: Option<CdrKey>) -> emob_cdr::Cdr {
    let binding = wholesale();
    let mut builder = CdrBuilder::from_session(&session(), Direction::Import)
        .expect("an ended session")
        .key(
            PartyId::new("DE", "ABC").expect("a valid party"),
            id.parse().expect("a valid CDR id"),
        )
        .evidence(EvidenceRef::from_evidence(&evidence(), "OCMF"))
        .rated_with(&binding);
    if let Some(previous) = supersedes {
        builder = builder.supersedes(previous);
    }
    builder.build().expect("a genuine session builds a record")
}

fn location() -> ocpi_kit::v2_3_0::cdrs::CdrLocation {
    use emob_poi::site::{
        Address, ChargingPoint, Connector, ConnectorType, Coordinates, Facility, Site,
    };
    let site = Site {
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
        time_zone: emob_core::TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        stations: Vec::new(),
    };
    let point = ChargingPoint::new(
        Facility::new("evse-1"),
        "DE*AB7*E840*6487".parse().expect("a valid EVSE id"),
        Connector::new(ConnectorType::Iec62196T2Combo, dec("150")),
    );
    emob_roam::ocpi::location::cdr_location(&site, &point, &point.connectors[0], 0)
        .expect("a publishable location")
}

fn node() -> Roamd {
    Roamd::new(
        PartnerRegistry::new(PartyId::new("DE", "ABC").expect("a valid party")).with(
            Partner::emsp(PartyId::new("NL", "TNM").expect("a valid party"))
                .on_signed_data()
                .settling_within(time::Duration::days(30)),
        ),
    )
}

#[test]
fn a_record_is_routed_by_the_contract_and_sent_on_the_wire_the_registry_records() {
    let mut node = node();
    let cdr = issued("cdr-77", None);
    let token = token();

    // ── Routed by what the contract itself says ─────────────────────────────
    let consignment = node
        .consign(&cdr, &token)
        .expect("the contract's own issuer is a partner")
        .clone();
    assert_eq!(
        consignment.reach,
        Reach::Direct(PartyId::new("NL", "TNM").unwrap()),
        "a direct peer claiming the contract's namespace wins over a hub"
    );
    assert_eq!(consignment.wire, Wire::Ocpi);
    assert_eq!(consignment.delivery, Delivery::Pending);

    // ── The document is the crossing's, in the version the registry records ─
    let signed = payloads();
    let context = Context {
        token: &token,
        location: location(),
        signed,
        public_key: None,
        last_updated: at(31),
    };
    let crossing = node
        .prepare(&cdr.key, &cdr, &context)
        .expect("a rated import record crosses");
    assert_eq!(crossing.value.version(), OcpiVersion::V2_3_0);
    let Outbound::V2_3_0(document) = &crossing.value else {
        panic!("the registry records 2.3.0");
    };
    assert_eq!(document.total_energy.get().to_string(), "29.500");

    // ── …and a delivery is recorded after the answer, never before ──────────
    assert!(
        node.unsettled(at(31)).is_empty(),
        "nothing is late inside the window this partner agreed to"
    );
    node.accepted(
        &cdr.key,
        at(32),
        Some("https://emsp.example/ocpi/2.3.0/cdrs/9".to_owned()),
    )
    .expect("a consigned record can be acknowledged");

    let held = node.consignment(&cdr.key).expect("it is held");
    assert!(matches!(
        &held.delivery,
        Delivery::Accepted { location: Some(url), .. } if url.ends_with("/cdrs/9")
    ));
    assert_eq!(node.pending().count(), 0);
}

#[test]
fn a_record_the_partner_took_is_sealed_and_a_correction_has_an_order() {
    // `[OCPI 2.3.0 §mod_cdrs]`: "Because a CDR is for billing purposes, it
    // cannot be changed or replaced once sent to the eMSP. Changes are simply
    // not allowed. Instead, a Credit CDR can be sent."
    //
    // No crate can hold this rule: `to_ocpi_credit` builds the reversal and
    // `emob_cdr` refuses to bill both halves, and neither of them knows whether
    // the original ever left the building.
    let mut node = node();
    let original = issued("cdr-77", None);
    let token = token();
    node.consign(&original, &token).expect("it routes");
    node.accepted(&original.key, at(32), None)
        .expect("the partner took it");

    let error = node
        .consign(&original, &token)
        .expect_err("a sealed record is not sent again");
    assert!(
        matches!(error, DispatchError::AlreadyAccepted { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("Credit CDR"), "{error}");

    // …and the replacement may not overtake its own reversal. Sent the other
    // way round, the partner holds two records for one session and settles
    // both.
    let replacement = issued("cdr-78", Some(original.key.clone()));
    let error = node
        .consign(&replacement, &token)
        .expect_err("the reversal goes first");
    assert!(
        matches!(error, DispatchError::CreditNotAcceptedYet { .. }),
        "{error}"
    );

    // The reversal, addressed to the same partner and with its own id — the
    // specification's own "-C".
    let credit_key = CdrKey {
        party: PartyId::new("DE", "ABC").unwrap(),
        id: "cdr-77-C".parse().unwrap(),
    };
    node.credit(&original.key, credit_key.clone(), at(30))
        .expect("the original was consigned, so it can be reversed");
    assert!(
        node.consign(&replacement, &token).is_err(),
        "sending the reversal is not the same as it being taken"
    );

    node.accepted(&credit_key, at(40), None)
        .expect("the partner took the reversal");
    let consigned = node
        .consign(&replacement, &token)
        .expect("and now the replacement may go");
    assert_eq!(consigned.supersedes.as_ref(), Some(&original.key));
}

#[test]
fn a_record_past_the_window_its_partner_agreed_to_is_named() {
    // `[OCPI 2.3.0 §mod_cdrs]` makes the cadence an agreement between the two
    // parties: "if there is an agreement between parties to send them, for
    // example, once a month, that is also allowed by OCPI." So a node peering
    // with a monthly settler and a same-day settler has two answers to one
    // question, and a single constant would report the first in breach daily.
    let monthly = PartyId::new("NL", "TNM").unwrap();
    let daily = PartyId::new("FR", "GIR").unwrap();
    let mut node = Roamd::new(
        PartnerRegistry::new(PartyId::new("DE", "ABC").unwrap())
            .with(Partner::emsp(monthly.clone()).settling_within(time::Duration::days(30)))
            .with(
                Partner::emsp(daily.clone())
                    .issuing(daily.clone())
                    .settling_within(time::Duration::days(1)),
            ),
    );

    let cdr = issued("cdr-77", None);
    node.consign(&cdr, &token()).expect("it routes to NL*TNM");

    // A week later: inside one partner's window and well past the other's.
    let week = at(30) + time::Duration::days(7);
    assert!(
        node.unsettled(week).is_empty(),
        "a monthly settler is not late after a week"
    );

    let month = at(30) + time::Duration::days(31);
    let late = node.unsettled(month);
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].recipient, monthly);
    assert_eq!(late[0].agreed, time::Duration::days(30));
    assert!(late[0].refused.is_none());
    let sentence = late[0].to_string();
    assert!(sentence.contains("NL*TNM"), "{sentence}");
    assert!(sentence.contains("[OCPI 2.3.0 §mod_cdrs]"), "{sentence}");

    // …and a refusal is late with its reason, because a refused record can be
    // corrected and sent again while an accepted one cannot.
    node.refused(&cdr.key, month, "total_cost does not match the periods")
        .expect("a consigned record can be refused");
    let late = node.unsettled(month);
    assert_eq!(
        late[0].refused.as_deref(),
        Some("total_cost does not match the periods")
    );
    assert!(late[0].to_string().contains("corrected and sent again"));
}

#[test]
fn a_partners_record_is_accepted_re_rated_and_never_taken_twice() {
    // The inbound half, which is the question `from_ocpi` deliberately does not
    // answer: it lands a record unpriced, and what to *do* with it is a
    // service's.
    let mut node = node();
    let cdr = issued("cdr-77", None);
    let token = token();
    let context = Context {
        token: &token,
        location: location(),
        signed: payloads(),
        public_key: None,
        last_updated: at(31),
    };
    // The same document this node would send, arriving at the other end.
    let document = emob_roam::ocpi::cdr::to_ocpi(
        &cdr,
        node.registry()
            .get(&PartyId::new("NL", "TNM").unwrap())
            .unwrap(),
        &context,
    )
    .expect("a rated import record crosses")
    .into_value_discarding_notes();

    // The payloads are verified against **this** node's registry, never the key
    // the document carries.
    let payloads = emob_roam::ocpi::cdr::inbound_payloads(&document);
    let records: Vec<_> = payloads
        .iter()
        .map(|p| ocmf::Record::parse(&p.signed_data).expect("verbatim OCMF"))
        .collect();
    let verified = Evidence::assemble(&records, &key_registry(), at(0));
    let ours = EvidenceRef::from_evidence(&verified, "OCMF");

    let Verdict::Accepted(inbound) = node.receive(&document, Some(ours.clone())) else {
        panic!("a coherent document from a known partner is accepted");
    };
    assert!(
        inbound.cdr.cost.is_none(),
        "a partner's record arrives unpriced"
    );

    // Re-rated through the same door the issuer priced with, so every gate
    // applies rather than being skipped by a caller reaching for the rating
    // engine directly.
    let settlement = node
        .settle(&inbound, &retail())
        .expect("this side's own retail tariff prices it");
    assert_eq!(settlement.owed_to_partner.to_string(), "14.46 EUR");
    assert_eq!(settlement.owed_by_driver.to_string(), "17.41 EUR");
    assert_eq!(settlement.margin().unwrap().to_string(), "2.95 EUR");

    // A partner that re-sends after a timeout has not created a second session.
    assert!(matches!(
        node.receive(&document, Some(ours.clone())),
        Verdict::Duplicate { .. }
    ));
    assert_eq!(node.received().len(), 1);

    // …and a restatement under a held id is a **conflict**, never an upsert:
    // OCPI does not permit a CDR to be changed or replaced, so the record
    // already held is the one that stands and a human answers the difference.
    //
    // The document below is perfectly coherent — it says the car sat there a
    // quarter of an hour longer — which is the point: this is not a malformed
    // message, it is a partner restating a settled number.
    let mut restated = document.clone();
    restated.end_date_time = at(45).into();
    restated.total_time = ocpi_kit::types::Number::new(dec("0.75"));
    let verdict = node.receive(&restated, Some(ours));
    assert!(matches!(verdict, Verdict::Conflicted { .. }), "{verdict:?}");
    assert_eq!(node.received().len(), 1, "the held record stands");
}

#[test]
fn a_document_that_is_wrong_and_a_claim_that_does_not_hold_are_different_answers() {
    let mut node = node();
    let cdr = issued("cdr-77", None);
    let token = token();
    let context = Context {
        token: &token,
        location: location(),
        signed: payloads(),
        public_key: None,
        last_updated: at(31),
    };
    let document = emob_roam::ocpi::cdr::to_ocpi(
        &cdr,
        node.registry()
            .get(&PartyId::new("NL", "TNM").unwrap())
            .unwrap(),
        &context,
    )
    .expect("it crosses")
    .into_value_discarding_notes();

    // ── The document is wrong: the sender can fix it and send it again ──────
    let mut broken = document.clone();
    broken.total_energy = ocpi_kit::types::Number::new(dec("999.000"));
    let Verdict::Rejected { reasons } = node.receive(&broken, None) else {
        panic!("a document whose periods do not sum to its total is refused");
    };
    assert!(reasons.iter().any(|r| r.contains("periods")), "{reasons:?}");
    assert_eq!(node.received().len(), 0, "nothing was taken");

    // ── The document is fine and the claim does not hold on this side ───────
    // The partner's signed records do not verify against **our** key registry,
    // which is not a malformed document — it is a settlement conversation.
    let unverifiable = EvidenceRef {
        encoding_method: "OCMF".into(),
        payload_digests: vec![[0u8; 32]],
        identification_strength: emob_core::IdentificationStrength::None,
        energy_billable: false,
        duration_billable: false,
        direction: Some(Direction::Import),
        compensated_loss: None,
        tariff_changes: Vec::new(),
    };
    let Verdict::Disputed { key, reasons } = node.receive(&document, Some(unverifiable)) else {
        panic!("a record this side cannot settle is disputed rather than refused");
    };
    assert_eq!(key.id.as_str(), "cdr-77");
    assert!(
        reasons.iter().any(|r| r.contains("not billable")),
        "{reasons:?}"
    );
    assert_eq!(
        node.received().len(),
        0,
        "a disputed record is not in the ledger: it has not been settled"
    );
}

#[test]
fn the_wire_a_partner_is_reached_on_is_a_field_rather_than_an_inference() {
    // Hubject is a hub **and** speaks OICP; GIREVE is a hub and speaks OCPI. A
    // service that read the wire off `Role::Hub` would hand a broker a document
    // it parses none of.
    let broker = PartyId::new("DE", "HUB").unwrap();
    let mut node = Roamd::new(
        PartnerRegistry::new(PartyId::new("DE", "ABC").unwrap())
            .with(Partner::hub(broker.clone()).over(Wire::Oicp)),
    );

    let cdr = issued("cdr-77", None);
    let consignment = node
        .consign(&cdr, &token())
        .expect("no direct peer claims the namespace, so the hub takes it")
        .clone();
    assert_eq!(
        consignment.reach,
        Reach::Hub {
            hub: broker.clone(),
            issuer: PartyId::new("NL", "TNM").unwrap(),
        },
        "the CPO still knows which provider the contract names"
    );
    assert_eq!(consignment.wire, Wire::Oicp);

    // …and reaching for the wrong builder is refused rather than sent.
    let token = token();
    let context = Context {
        token: &token,
        location: location(),
        signed: payloads(),
        public_key: None,
        last_updated: at(31),
    };
    let error = node
        .prepare(&cdr.key, &cdr, &context)
        .expect_err("this partner is not on OCPI");
    assert!(matches!(error, DispatchError::WrongWire { .. }), "{error}");
    assert!(error.to_string().contains("OICP"), "{error}");
}

#[test]
fn the_two_ways_a_record_cannot_be_routed_are_two_messages() {
    // `PartnerRegistry::route` answers `None` for both, and they are different
    // operational problems: one wants a partner added, the other wants that
    // partner's own namespace declared. An operator told the first when it is
    // the second goes looking for something that is not missing (D268).
    let cdr = issued("cdr-77", None);

    // A contract naming a provider nobody peers with: a partner is missing.
    let mut empty = Roamd::new(PartnerRegistry::new(PartyId::new("DE", "ABC").unwrap()));
    let error = empty
        .consign(&cdr, &token())
        .expect_err("nobody claims NL*TNM");
    assert!(error.to_string().contains("issued by NL*TNM"), "{error}");

    // …and one in an eMSP's own scheme, which OCPI permits: the registry needs
    // the explicit namespace entry rather than a route by prefix.
    let own_scheme = RoamingToken::new(
        PartyId::new("NL", "TNM").unwrap(),
        "045F2C9A",
        TokenType::AppUser,
        "loyalty-9931".parse().unwrap(),
    )
    .unwrap();
    let error = empty
        .consign(&cdr, &own_scheme)
        .expect_err("no provider can be read out of it");
    assert!(
        matches!(error, DispatchError::UnroutableContract { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("Partner::issuing"), "{error}");
}

#[test]
fn a_node_that_settles_german_sessions_refuses_a_record_with_no_signed_data() {
    // `[MessEG §33]` lets a measured value be billed only where the customer
    // can check it, so a node settling German sessions cannot take a record
    // whose energy nothing signed. A node peering into jurisdictions that do
    // not ask should not refuse a lawful record over it, which is why the
    // policy is stated rather than assumed.
    let cdr = issued("cdr-77", None);
    let token = token();
    let bare = Context {
        token: &token,
        location: location(),
        signed: Vec::new(),
        public_key: None,
        last_updated: at(31),
    };
    let relaxed = Partner::emsp(PartyId::new("NL", "TNM").unwrap());
    let document = emob_roam::ocpi::cdr::to_ocpi(&cdr, &relaxed, &bare)
        .expect("not every partner asks for signed data")
        .into_value_discarding_notes();

    // A node that does not ask reads it.
    let mut lenient = node();
    assert!(matches!(
        lenient.receive(&document, None),
        Verdict::Accepted(_)
    ));

    // …and one that does refuses the same document, before any conversion.
    let mut strict = node().requiring_signed_data();
    let Verdict::Rejected { reasons } = strict.receive(&document, None) else {
        panic!("a record with no signed data is not one this node may settle");
    };
    assert!(reasons.iter().any(|r| r.contains("signed")), "{reasons:?}");
    assert_eq!(strict.received().len(), 0);
}
