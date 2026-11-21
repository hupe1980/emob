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
                        .to_sec1_point(false)
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

/// The document this node would send, arriving at the other end, and the
/// evidence **this** node's own registry makes of the payloads inside it.
fn crossing() -> (ocpi_kit::v2_3_0::Cdr, EvidenceRef) {
    let node = node();
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
            .get(&PartyId::new("NL", "TNM").expect("a valid party"))
            .expect("a peered partner"),
        &context,
    )
    .expect("a rated import record crosses")
    .into_value_discarding_notes();

    let payloads = emob_roam::ocpi::cdr::inbound_payloads(&document);
    let records: Vec<_> = payloads
        .iter()
        .map(|payload| ocmf::Record::parse(&payload.signed_data).expect("verbatim OCMF"))
        .collect();
    let verified = Evidence::assemble(&records, &key_registry(), at(0));
    (document, EvidenceRef::from_evidence(&verified, "OCMF"))
}

/// The credential the partner pushes with: its own party, its own role, and
/// reaching only its own records.
fn peer() -> emob_service::Principal {
    emob_service::Principal::peer(
        PartyId::new("DE", "ABC").expect("a valid party"),
        emob_core::Role::Cpo,
        emob_service::Capabilities::root(),
    )
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
    // The same document this node would send, arriving at the other end — with
    // its payloads verified against **this** node's registry, never the key the
    // document carries.
    let (document, ours) = crossing();

    let Verdict::Accepted(inbound) = node.receive(&peer(), &document, Some(ours.clone()), at(60))
    else {
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
        node.receive(&peer(), &document, Some(ours.clone()), at(60)),
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
    let verdict = node.receive(&peer(), &restated, Some(ours), at(60));
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
    let Verdict::Rejected { reasons } = node.receive(&peer(), &broken, None, at(60)) else {
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
    let Verdict::Disputed { key, reasons } =
        node.receive(&peer(), &document, Some(unverifiable), at(60))
    else {
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
        lenient.receive(&peer(), &document, None, at(60)),
        Verdict::Accepted(_)
    ));

    // …and one that does refuses the same document, before any conversion.
    let mut strict = node().requiring_signed_data();
    let Verdict::Rejected { reasons } = strict.receive(&peer(), &document, None, at(60)) else {
        panic!("a record with no signed data is not one this node may settle");
    };
    assert!(reasons.iter().any(|r| r.contains("signed")), "{reasons:?}");
    assert_eq!(strict.received().len(), 0);
}

/// **An acceptance is terminal, and a reversal is owed only where one happened.**
///
/// Two readings of one paragraph, each held by a different method.
/// `[OCPI 2.3.0 §mod_cdrs]` seals a record the moment the eMSP has it, and
/// `consign` enforced that by reading the delivery field — which `refused` would
/// overwrite, so one call after the fact re-opened the seal and the same record
/// could go to the same partner twice.
///
/// And the same paragraph's *other* half: a Credit CDR reverses a record the
/// eMSP **has**. Requiring one before every replacement deadlocked the
/// correction of a record the partner had refused, against `consign`'s own
/// documented rule that nothing refused is sealed (D297).
#[test]
fn an_acceptance_cannot_be_taken_back_and_a_refusal_does_not_need_reversing() {
    let mut node = node();
    let token = token();

    // ── The seal holds against a later refusal ─────────────────────────────
    let taken = issued("cdr-90", None);
    node.consign(&taken, &token).expect("it routes");
    node.accepted(&taken.key, at(32), None)
        .expect("the partner took it");
    let error = node
        .refused(&taken.key, at(33), "on reflection, no")
        .expect_err("a record the eMSP has cannot become one it refused");
    assert!(
        matches!(error, DispatchError::AlreadyAccepted { .. }),
        "{error}"
    );
    assert!(
        node.consign(&taken, &token).is_err(),
        "and the seal is still a seal"
    );
    // …while re-recording the same outcome is idempotent, because a location
    // that arrives late is a fact about one delivery rather than a second.
    node.accepted(
        &taken.key,
        at(32),
        Some("https://emsp.example/cdrs/9".to_owned()),
    )
    .expect("the same acceptance, with the URL the receiver returned");

    // ── …and a refused record is corrected without one ─────────────────────
    let refused = issued("cdr-91", None);
    node.consign(&refused, &token).expect("it routes");
    node.refused(
        &refused.key,
        at(33),
        "total_cost does not match the periods",
    )
    .expect("the partner said no");

    let replacement = issued("cdr-92", Some(refused.key.clone()));
    let consigned = node
        .consign(&replacement, &token)
        .expect("nothing was settled, so nothing is sealed and nothing is owed back");
    assert_eq!(consigned.supersedes.as_ref(), Some(&refused.key));
}

/// **M7b: a partner onboarded at runtime gets a policy an operator wrote.**
///
/// The typed authority model already takes its values at runtime — a
/// `Principal` is built from configuration, not from a `const` — so a policy
/// engine buys nothing until there is a question that model has no vocabulary
/// for. This is that question: *charge detail records, until the trial ends*.
/// A set of dotted capability patterns cannot say **until**.
///
/// Four things are asserted, and each is a way a policy engine goes wrong:
///
/// 1. it decides, and the answer is the operator's own name for the policy;
/// 2. it decides **inside** a ceiling no text can widen;
/// 3. an operator who wrote nothing gets nothing;
/// 4. a reload that does not validate leaves the running set standing, because
///    default deny makes an empty set a total outage rather than a degraded one.
#[test]
fn a_partner_onboarded_at_runtime_is_decided_by_a_policy_an_operator_wrote() {
    use emob_service::policy::{Gate, Policies};

    // What an operator writes when the partner arrives. `@id` is their own name
    // for it, and it is what a verdict says back.
    const TRIAL: &str = r#"
        @id("de-abc-trial")
        permit (
          principal == emob::Party::"DE*ABC",
          action == emob::Action::"emob.cdr.write",
          resource
        )
        when { resource.owner == principal }
        unless { context.at > datetime("2026-07-01T00:00:00Z") };
    "#;

    let fresh = node;
    let mut node = fresh().with_policies(Gate::new(
        Policies::load(TRIAL).expect("the trial policy holds against the derived schema"),
    ));
    let (document, ours) = crossing();

    // ── 1. Inside the trial, the record is taken ───────────────────────────
    let inside = node.receive(&peer(), &document, Some(ours.clone()), at(60));
    assert!(
        matches!(inside, Verdict::Accepted(_)),
        "the policy grants it: {inside:?}"
    );

    // ── 2. …and after it, the same document from the same partner is not ───
    //
    // Nothing about the record changed. The grant did, without a deploy, which
    // is the whole of what this milestone is about.
    let mut later = fresh().with_policies(Gate::new(Policies::load(TRIAL).expect("it holds")));
    let after = later.receive(
        &peer(),
        &document,
        Some(ours.clone()),
        datetime!(2026-09-01 10:00 +2),
    );
    let Verdict::Unauthorised { reasons } = after else {
        panic!("a trial that has ended is not a document fault: {after:?}");
    };
    assert!(!reasons.is_empty());

    // ── 3. A policy may grant, and it may never widen a delegation ─────────
    //
    // The credential reaches only its own party's records, and no text can turn
    // that off — which is the leak `emob_service::authority` exists to prevent
    // and the reason the typed model is asked first.
    let stranger = emob_service::Principal::peer(
        PartyId::new("NL", "TNM").expect("a valid party"),
        emob_core::Role::Emsp,
        emob_service::Capabilities::root(),
    );
    let mut node = fresh().with_policies(Gate::new(Policies::load(TRIAL).expect("it holds")));
    let pushed = node.receive(&stranger, &document, Some(ours.clone()), at(60));
    let Verdict::Unauthorised { reasons } = pushed else {
        panic!("a peer pushing another party's record is not a document fault: {pushed:?}");
    };
    assert!(
        reasons[0].contains("does not reach"),
        "the typed ceiling answers first, and says so: {reasons:?}"
    );

    // ── 4. An operator who writes nothing gets nothing ─────────────────────
    let mut silent =
        fresh().with_policies(Gate::new(Policies::load("").expect("no policies is a set")));
    assert!(matches!(
        silent.receive(&peer(), &document, Some(ours.clone()), at(60)),
        Verdict::Unauthorised { .. }
    ));

    // …and a reload that does not hold leaves the old set deciding, because an
    // empty policy set is not a degraded state — it refuses every request the
    // node was serving a second ago.
    let mut node = fresh().with_policies(Gate::new(Policies::load(TRIAL).expect("it holds")));
    assert!(
        node.reload_policies(r#"permit (principal, action == emob::Action::"nope", resource);"#)
            .is_err()
    );
    assert!(matches!(
        node.receive(&peer(), &document, Some(ours.clone()), at(60)),
        Verdict::Accepted(_)
    ));

    // …and one that does hold replaces it, while the node runs.
    let mut node = fresh().with_policies(Gate::new(Policies::load(TRIAL).expect("it holds")));
    assert_eq!(node.reload_policies("").expect("an empty set is valid"), 0);
    assert!(matches!(
        node.receive(&peer(), &document, Some(ours), at(60)),
        Verdict::Unauthorised { .. }
    ));

    // …and a node that was never given a gate refuses the load rather than
    // holding a grant nobody enforces.
    assert!(matches!(
        fresh().reload_policies(TRIAL),
        Err(DispatchError::NoPolicies)
    ));
}

// ── …and over a real socket, which is the leg that was described as untestable
//
// Four documents called the outbound push *"somebody else's server, behind
// credentials"*. That was true of Hubject and true of a peer's production
// endpoint, and it was never the reason this leg had no test: the daemon took
// `ocpi-kit` **without its transport feature**, so there was no client to give
// credentials to (D322).
//
// A peer is an HTTP server. One runs here, on a real port, and answers the way
// `[OCPI 2.3.0 §mod_cdrs]` says a receiver answers.

use emob_service::Actor;
use emob_service::journal::{Journal as _, Memory};
use ocpi_kit::VersionNumber;
use ocpi_kit::client::ClientConfig;
use roamd::{Answer, Dispatcher, Node};

/// The node's data key: the same bytes on every open, which is what a restart
/// under one `ROAMD_DATA_KEY` is.
fn data_key() -> emob_service::vault::DataKey {
    emob_service::vault::DataKey::from_bytes(&[5; 32])
}
use std::sync::{Arc, Mutex as StdMutex};

/// A receiver that records what it was given and answers as the specification
/// prescribes.
async fn receiver(
    status: u16,
    body: serde_json::Value,
    location: Option<&'static str>,
) -> (std::net::SocketAddr, Arc<StdMutex<Vec<serde_json::Value>>>) {
    let seen: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/ocpi/cdrs",
        axum::routing::post(move |axum::Json(payload): axum::Json<serde_json::Value>| {
            let recorded = Arc::clone(&recorded);
            let body = body.clone();
            async move {
                recorded.lock().expect("not poisoned").push(payload);
                let mut response = axum::response::Response::builder()
                    .status(status)
                    .header("content-type", "application/json");
                if let Some(location) = location {
                    response = response.header("location", location);
                }
                response
                    .body(axum::body::Body::from(body.to_string()))
                    .expect("a well-formed response")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("a bound address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, seen)
}

fn ok_envelope() -> serde_json::Value {
    serde_json::json!({ "status_code": 1000, "timestamp": "2026-06-01T10:31:00Z" })
}

fn dispatcher() -> Dispatcher {
    Dispatcher::new(
        &PartyId::new("DE", "ABC").expect("a valid party"),
        // Localhost and plain HTTP, which the default policy refuses on
        // purpose: a production client that would `POST` a charge detail record
        // over cleartext is a client sending a driver's movements in the open.
        ClientConfig::for_testing(),
    )
    .expect("a client")
}

/// What a completed handshake leaves behind, which is the only description of a
/// partner this node keeps.
fn registered_at(addr: std::net::SocketAddr) -> roamd::Registration {
    roamd::Registration {
        peer_id: "NL*TNM".to_owned(),
        incoming: data_key().digest(roamd::peering::INCOMING, b"TOKEN-C-THIS-NODE-ISSUED"),
        outgoing: data_key().seal_str(roamd::peering::OUTGOING, "a-test-credentials-token"),
        versions_url: format!("http://{addr}/ocpi/versions"),
        version: "2.3.0".to_owned(),
        endpoints: vec![roamd::peering::Endpoint {
            module: "cdrs".to_owned(),
            role: "RECEIVER".to_owned(),
            url: format!("http://{addr}/ocpi/cdrs"),
        }],
        parties: vec![("NL".to_owned(), "TNM".to_owned())],
    }
}

/// The consigned record, its rendered document, and the node holding both.
fn queued() -> (Node<Memory>, CdrKey, Outbound) {
    let mut node = Node::open(node(), Memory::new(), data_key()).expect("an empty journal folds");
    let cdr = issued("cdr-77", None);
    let token = token();
    let context = Context {
        token: &token,
        location: location(),
        signed: payloads(),
        public_key: None,
        last_updated: at(31),
    };
    let key = node
        .consign(&Actor::service("csmsd"), &cdr, &token, Some(&context))
        .expect("a routable record");
    let document = node
        .state()
        .document(&key)
        .expect("the outbox holds the document the account describes")
        .clone();
    (node, key, document)
}

#[tokio::test]
async fn a_record_reaches_a_partner_over_a_real_socket_and_is_sealed_after_the_answer() {
    let (addr, seen) = receiver(
        200,
        ok_envelope(),
        Some("https://emsp.example/ocpi/cdrs/77"),
    )
    .await;
    let (mut node, key, document) = queued();

    // Before the send, nothing is sealed.
    assert!(
        !node
            .state()
            .consignment(&key)
            .expect("queued")
            .delivery
            .is_accepted()
    );

    let answer = dispatcher()
        .deliver(&registered_at(addr), &data_key(), &document)
        .await;
    assert_eq!(
        answer,
        Answer::Accepted {
            location: Some("https://emsp.example/ocpi/cdrs/77".to_owned())
        },
        "the receiver's `Location` is the URL the paragraph makes it return so the sender can read \
         back what it sent"
    );

    node.answered(&Actor::Timer, &key, &answer, at(40))
        .expect("recorded");
    assert!(
        node.state()
            .consignment(&key)
            .expect("held")
            .delivery
            .is_accepted()
    );

    // …and the bytes that arrived are the bytes this workspace rendered — not a
    // second downgrade the kit would have done at the wire, which would leave
    // the crossing's account pointing into a copy nobody received.
    let arrived = seen.lock().expect("not poisoned");
    assert_eq!(arrived.len(), 1, "one record, one POST");
    let Outbound::V2_3_0(sent) = &document else {
        panic!("this partner speaks 2.3.0");
    };
    assert_eq!(
        arrived[0],
        serde_json::to_value(sent.as_ref()).expect("the document serialises"),
        "the wire must carry the document the account describes, byte for byte"
    );
}

#[tokio::test]
async fn a_receiver_that_refuses_is_a_verdict_and_is_recorded_as_one() {
    // The specification's own shape for a refusal: HTTP 200 with an OCPI status
    // code that is not 1000 is *also* a refusal, and so is a 4xx. This is the
    // second, which is what a peer sends for a malformed push.
    let (addr, _) = receiver(
        422,
        serde_json::json!({
            "status_code": 2001,
            "status_message": "the charging periods do not sum to the total",
            "timestamp": "2026-06-01T10:31:00Z"
        }),
        None,
    )
    .await;
    let (mut node, key, document) = queued();

    let answer = dispatcher()
        .deliver(&registered_at(addr), &data_key(), &document)
        .await;
    let Answer::Refused { reason } = &answer else {
        panic!("a receiver that answered has seen the record: {answer:?}");
    };
    assert!(
        reason.contains("2001") || reason.contains("periods"),
        "the reason a person has to read must survive the wire: {reason}"
    );

    node.answered(&Actor::Timer, &key, &answer, at(40))
        .expect("recorded");
    let held = node.state().consignment(&key).expect("held");
    assert!(!held.delivery.is_accepted());
    assert!(
        matches!(held.delivery, roamd::Delivery::Refused { .. }),
        "a refusal is a verdict, and a refused record is not waiting for a socket"
    );
}

#[tokio::test]
async fn nobody_answering_is_not_a_refusal_and_leaves_the_record_owed() {
    // A platform that registered and published no CDRs Receiver — which is the
    // same reading as a connection that times out: the partner may hold the
    // record and may not, and the only safe answer is that this node still owes
    // it.
    let (mut node, key, document) = queued();
    let mut silent = registered_at("127.0.0.1:1".parse().expect("an address"));
    silent.endpoints.clear();

    let answer = dispatcher().deliver(&silent, &data_key(), &document).await;
    assert!(matches!(answer, Answer::Unreached { .. }), "{answer:?}");

    node.answered(&Actor::Timer, &key, &answer, at(40))
        .expect("an unreachable partner records nothing");
    assert!(
        node.state()
            .pending()
            .any(|consignment| consignment.key == key),
        "a record nobody answered for is still owed, and the next drain sends it again"
    );
    assert_eq!(
        node.journal().records().expect("the chain holds").len(),
        1,
        "only the consignment is on the record: a timeout is not a fact about the partner"
    );
}

// ── …and the half that makes this an OCPI implementation ────────────────────
//
// `[OCPI 2.3.0 §credentials]`: *"The Credentials module is symmetric and has to
// be implemented by all OCPI implementations: all implementations need to be
// able to call this module on any other platform and handle receiving requests
// from another party."*
//
// A node that can push a document and cannot be **registered with** is not one.
// These run the handshake against this node over a real socket, from the other
// side, and then push a record through it.

use ocpi_kit::ModuleId;
use ocpi_kit::client::{ClientConfig as KitConfig, OcpiClient, OcpiRequest};
use ocpi_kit::server::InMemoryTokenStore;
use ocpi_kit::transport::CredentialsToken;
use ocpi_kit::types::{OcpiString, Url as KitUrl};
use ocpi_kit::v2_3_0::credentials::Credentials;
use roamd::inbound::{Handshake, router};
use roamd::peering::Registry;

/// The peer's own side: a versions endpoint this node can read back during the
/// `POST`, which `[OCPI 2.3.0 §credentials]` makes a precondition of the
/// registration rather than a courtesy.
async fn peer_platform() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("bound");
    let base = format!("http://{addr}/ocpi");
    // The peer accepts the token it hands over, which is the whole point of the
    // step this node takes before it answers: `[OCPI 2.3.0 §credentials]` makes
    // the receiver read the client's versions **with that token**, and a peer
    // that does not accept it has not registered.
    let tokens = Arc::new(InMemoryTokenStore::new());
    tokens.insert(
        CredentialsToken::new("TOKEN-B-FROM-THE-PEER").expect("a token"),
        ocpi_kit::testkit::registered_peer("NL*TNM", vec![ocpi_kit::testkit::test_cpo()]),
    );
    let inner =
        ocpi_kit::server::OcpiRouter::new(VersionNumber::V2_3_0, KitUrl::new_lenient(base), tokens)
            .build();
    let app = axum::Router::new().nest("/ocpi", inner);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// This node, listening, with the loopback allowlist the tests' peer needs.
async fn this_node() -> (std::net::SocketAddr, Arc<StdMutex<Node<Memory>>>) {
    this_node_reaching(roamd::reach::ReachBack::allowing(["127.0.0.1"])).await
}

/// This node, listening, behind `reach`.
async fn this_node_reaching(
    reach: roamd::reach::ReachBack,
) -> (std::net::SocketAddr, Arc<StdMutex<Node<Memory>>>) {
    let node = Arc::new(StdMutex::new(
        Node::open(node(), Memory::new(), data_key()).expect("an empty journal folds"),
    ));
    // The bootstrap token, handed to the platform out of band — which is what
    // `[OCPI 2.3.0 §credentials]` says happens before any of this.
    node.lock()
        .expect("not poisoned")
        .offer(&Actor::Estate, "NL*TNM", "TOKEN-A-HANDED-OVER-BY-EMAIL")
        .expect("recorded");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("bound");
    let base = KitUrl::new_lenient(format!("http://{addr}/ocpi"));

    let me = Credentials::builder()
        .token(OcpiString::<64>::new("unused-until-we-answer").expect("a token"))
        .url(base.clone())
        .roles(vec![
            ocpi_kit::v2_3_0::credentials::CredentialsRole::builder()
                .role(ocpi_kit::v2_3_0::types::Role::Cpo)
                .business_details(
                    ocpi_kit::v2_3_0::locations::BusinessDetails::builder()
                        .name("emob")
                        .build(),
                )
                .party_id(ocpi_kit::types::CiString::new("ABC").expect("a party"))
                .country_code(ocpi_kit::types::CiString::new("DE").expect("a country"))
                .build(),
        ])
        .build();

    // Deterministic on purpose: a handshake replayed in a test has to produce
    // the same bytes, and a token reached through ambient randomness is the one
    // thing in it that cannot be replayed.
    let mint: roamd::inbound::MintToken = Arc::new(|peer: &str| format!("TOKEN-C-FOR-{peer}"));
    let handshake = Handshake::new(me, Arc::clone(&node), mint, KitConfig::for_testing(), reach)
        .expect("a client");
    let app = router(
        base,
        "/ocpi",
        Arc::clone(&node),
        Registry::new(Arc::clone(&node)),
        handshake,
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, node)
}

#[tokio::test]
async fn a_peer_discovers_this_node_registers_with_it_and_then_pushes_a_record() {
    let peer = peer_platform().await;
    let (mine, node) = this_node().await;

    let client = OcpiClient::with_config(KitConfig::for_testing()).expect("a client");
    let token_a = CredentialsToken::new("TOKEN-A-HANDED-OVER-BY-EMAIL").expect("a token");

    // 1. The peer discovers this node with the bootstrap token.
    let discovered = ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        token_a.clone(),
    )
    .discover(client.transport())
    .await
    .expect("this node publishes its versions");
    assert!(
        discovered
            .versions()
            .iter()
            .any(|v| v.version == VersionNumber::V2_3_0),
        "a node that advertises no version cannot be registered with: {:?}",
        discovered.versions()
    );

    // 2. …and registers, through the kit's own client: it selects a version,
    //    reads the version details, and POSTs to the credentials URL **that
    //    document advertises**. A hand-written URL here would pass while every
    //    real peer read a document pointing at 404s.
    let selected = ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        token_a.clone(),
    )
    .discover(client.transport())
    .await
    .expect("versions")
    .select_best(client.transport())
    .await
    .expect("this node publishes details for a version it speaks");

    // The check `[OCPI 2.3.0 §credentials]` says a party makes before it
    // registers: *"a CPO could want to connect only when the CDRs endpoint is
    // available at an eMSP"*.
    selected
        .require(&[
            // Credentials is symmetric, so the kit advertises it as `Sender`
            // on both sides — there is no second role for a module every
            // implementation both calls and answers.
            (ModuleId::Credentials, ocpi_kit::InterfaceRole::Sender),
            (ModuleId::Cdrs, ocpi_kit::InterfaceRole::Receiver),
        ])
        .expect("this node advertises the modules it mounts");

    let theirs = ocpi_kit::testkit::sample::credentials(
        "TOKEN-B-FROM-THE-PEER",
        &format!("http://{peer}/ocpi/versions"),
    )
    .expect("a credentials object");

    let registered_peer = selected
        .register(client.transport(), &theirs)
        .await
        .expect("the handshake completes");
    // The peer now holds a token this node issued, and presents it on every
    // later request. What it is, is asserted against the node's own ledger
    // below rather than against this handle: `CredentialsToken` redacts itself.
    let _ = registered_peer.token();

    // 3. …and it is on the record, not only in memory.
    {
        let held = node.lock().expect("not poisoned");
        let registered: Vec<_> = held.state().peers().registered().collect();
        assert_eq!(registered.len(), 1);
        // SR-044: held as a digest and a box, never as the tokens themselves.
        let key = held.key();
        assert!(key.matches(
            &registered[0].incoming,
            roamd::peering::INCOMING,
            b"TOKEN-C-FOR-NL*TNM"
        ));
        assert_eq!(
            key.open_str(roamd::peering::OUTGOING, &registered[0].outgoing)
                .unwrap(),
            "TOKEN-B-FROM-THE-PEER"
        );
        let on_disk = serde_json::to_string(&held.journal().records().unwrap()).unwrap();
        for token in [
            "TOKEN-A-HANDED-OVER-BY-EMAIL",
            "TOKEN-B-FROM-THE-PEER",
            "TOKEN-C-FOR-NL*TNM",
        ] {
            assert!(
                !on_disk.contains(token),
                "SR-044: {token} is in the journal"
            );
        }
        assert!(
            held.journal()
                .records()
                .expect("the chain holds")
                .iter()
                .any(|r| r.kind == "registered"),
            "a registration is a fact about the past: a node that forgot it answers 401 to every \
             partner it has onboarded"
        );
    }

    // 4. The bootstrap token is spent. `[OCPI 2.3.0 §credentials]` makes
    //    `Token A` valid only until the exchange is finished.
    let repeat = OcpiRequest::new(
        http::Method::POST,
        KitUrl::new_lenient(format!("http://{mine}/ocpi/credentials")),
        ModuleId::Credentials,
    )
    .with_body(&theirs)
    .expect("encodes");
    let again = client
        .transport()
        .send_with_headers::<Credentials>(&repeat, &token_a, &Default::default())
        .await;
    assert!(
        again.is_err(),
        "a spent bootstrap token must not open a second registration"
    );
}

#[tokio::test]
async fn a_registration_survives_a_restart_because_the_token_is_a_fact() {
    let peer = peer_platform().await;
    let (mine, running) = this_node().await;
    let client = OcpiClient::with_config(KitConfig::for_testing()).expect("a client");
    let token_a = CredentialsToken::new("TOKEN-A-HANDED-OVER-BY-EMAIL").expect("a token");
    let theirs = ocpi_kit::testkit::sample::credentials(
        "TOKEN-B-FROM-THE-PEER",
        &format!("http://{peer}/ocpi/versions"),
    )
    .expect("a credentials object");
    ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        token_a.clone(),
    )
    .discover(client.transport())
    .await
    .expect("versions")
    .select_best(client.transport())
    .await
    .expect("details")
    .register(client.transport(), &theirs)
    .await
    .expect("the handshake completes");

    // The journal is the node's; fold it into a fresh one, which is what a
    // restart is.
    let records = running
        .lock()
        .expect("not poisoned")
        .journal()
        .records()
        .expect("the chain holds");
    let replay = Memory::new();
    for record in &records {
        replay
            .append(&record.kind, &record.actor, &record.body)
            .expect("the same facts");
    }
    let restarted = Node::open(node(), replay, data_key()).expect("the journal folds");

    let registered: Vec<_> = restarted.state().peers().registered().collect();
    assert_eq!(
        registered.len(),
        1,
        "a node that forgot its registrations answers 401 to every partner it has onboarded"
    );
    assert!(
        restarted.key().matches(
            &registered[0].incoming,
            roamd::peering::INCOMING,
            b"TOKEN-C-FOR-NL*TNM"
        ),
        "SR-044: after a restart Token C still authenticates"
    );
    assert!(
        restarted
            .state()
            .peers()
            .for_party(&PartyId::new("NL", "TNM").expect("a valid party"))
            .is_some(),
        "and it still knows which platform speaks for which party"
    );
}

/// The loop the two halves close: **registering is what makes a partner
/// reachable**.
///
/// Before this, a registration went into one map and the client read another,
/// so a partner could complete the handshake and still be unreachable — and the
/// endpoints the handshake fetched were thrown away, so the node knew where the
/// peer's directory was and not where to push (D330). One inventory, two
/// audiences.
#[tokio::test]
async fn a_partner_that_registers_is_reachable_from_that_moment() {
    let seen: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));

    // The peer's platform: a versions endpoint this node reads during the
    // handshake, and a CDRs receiver it advertises there.
    let recorded = Arc::clone(&seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let peer_addr = listener.local_addr().expect("bound");
    let tokens = Arc::new(InMemoryTokenStore::new());
    tokens.insert(
        CredentialsToken::new("TOKEN-B-FROM-THE-PEER").expect("a token"),
        ocpi_kit::testkit::registered_peer("NL*TNM", vec![ocpi_kit::testkit::test_cpo()]),
    );
    let inner = ocpi_kit::server::OcpiRouter::new(
        VersionNumber::V2_3_0,
        KitUrl::new_lenient(format!("http://{peer_addr}/ocpi")),
        tokens,
    )
    .cdrs_receiver(Collecting(recorded))
    .build();
    tokio::spawn(async move {
        let _ = axum::serve(listener, axum::Router::new().nest("/ocpi", inner)).await;
    });

    // This node, and the handshake.
    let (mine, node) = this_node().await;
    let client = OcpiClient::with_config(KitConfig::for_testing()).expect("a client");
    let theirs = ocpi_kit::testkit::sample::credentials(
        "TOKEN-B-FROM-THE-PEER",
        &format!("http://{peer_addr}/ocpi/versions"),
    )
    .expect("a credentials object");
    ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        CredentialsToken::new("TOKEN-A-HANDED-OVER-BY-EMAIL").expect("a token"),
    )
    .discover(client.transport())
    .await
    .expect("versions")
    .select_best(client.transport())
    .await
    .expect("details")
    .register(client.transport(), &theirs)
    .await
    .expect("the handshake completes");

    // The endpoints the handshake read are on the record, which is the half
    // that was being thrown away.
    let to = {
        let held = node.lock().expect("not poisoned");
        let registration = held
            .state()
            .peers()
            .for_party(&PartyId::new("NL", "TNM").expect("a valid party"))
            .expect("the platform that just registered speaks for this party")
            .clone();
        assert!(
            registration.endpoint("cdrs", "RECEIVER").is_some(),
            "a node that kept the versions URL and dropped the endpoints knows where a peer's \
             directory is and not where to push a record to it: {:?}",
            registration.endpoints
        );
        registration
    };

    // …and a record reaches it, with no peer configured anywhere.
    let (_, _, document) = queued();
    let answer = dispatcher().deliver(&to, &data_key(), &document).await;
    assert!(
        matches!(answer, Answer::Accepted { .. }),
        "registering is what makes a partner reachable: {answer:?}"
    );
    assert_eq!(
        seen.lock().expect("not poisoned").len(),
        1,
        "one record, at the endpoint the peer published when it registered"
    );
}

/// A CDRs receiver that keeps what it was given.
struct Collecting(Arc<StdMutex<Vec<serde_json::Value>>>);

impl ocpi_kit::server::CdrsReceiver for Collecting {
    async fn cdr(
        &self,
        cdr_id: String,
        _context: ocpi_kit::server::RequestContext,
    ) -> ocpi_kit::server::Handled<ocpi_kit::v2_3_0::cdrs::Cdr> {
        Err(ocpi_kit::transport::OcpiError::NotFound(cdr_id))
    }

    async fn post_cdr(
        &self,
        cdr: ocpi_kit::v2_3_0::cdrs::Cdr,
        _context: ocpi_kit::server::RequestContext,
    ) -> ocpi_kit::server::Handled<KitUrl> {
        let id = cdr.id.as_str().to_owned();
        self.0
            .lock()
            .expect("not poisoned")
            .push(serde_json::to_value(&cdr).expect("serialises"));
        Ok(KitUrl::new_lenient(format!(
            "https://emsp.example/cdrs/{id}"
        )))
    }
}

/// The test whose absence hid a defect that broke every inbound document.
///
/// Every other test here builds a `Cdr` in Rust and hands it to the node. None
/// of them **parsed** one, and a partner's record arrives as bytes — so a
/// workspace-wide `serde_json` feature could make every numeric field in every
/// OCPI document undeserialisable, and nothing would say so (D331). That is
/// rule 4 in the direction nobody was looking: not *a fixture this workspace
/// signed*, but *a fixture this workspace never parsed*.
#[test]
fn a_partners_document_parses_from_the_bytes_it_arrives_as() {
    // Fractional, because an integer would pass under the defect.
    let wire = r#"{
        "country_code": "NL", "party_id": "TNM", "id": "cdr-parse-1",
        "start_date_time": "2026-06-01T10:00:00Z", "end_date_time": "2026-06-01T10:31:00Z",
        "cdr_token": {"country_code":"NL","party_id":"TNM","uid":"045F2C9A","type":"RFID","contract_id":"NL-TNM-C00122045-K"},
        "auth_method": "AUTH_REQUEST",
        "cdr_location": {"id":"LOC1","address":"Hauptstraße 12","city":"Berlin","country":"DEU",
            "coordinates":{"latitude":"52.520008","longitude":"13.404954"},
            "evse_uid":"EVSE1","evse_id":"DE*AB7*E840*6487","connector_id":"1",
            "connector_standard":"IEC_62196_T2_COMBO","connector_format":"CABLE","connector_power_type":"DC"},
        "currency": "EUR",
        "charging_periods": [
            {"start_date_time":"2026-06-01T10:00:00Z",
             "dimensions":[{"type":"ENERGY","volume":18.512},{"type":"TIME","volume":0.5166}]}
        ],
        "total_cost": {"before_taxes": 8.82},
        "total_energy": 18.512,
        "total_time": 0.5166,
        "last_updated": "2026-06-01T10:31:00Z"
    }"#;

    let parsed: ocpi_kit::v2_3_0::cdrs::Cdr =
        serde_json::from_str(wire).expect("a partner's record has to parse from its own bytes");
    assert_eq!(parsed.id.as_str(), "cdr-parse-1");
    // Every figure at the scale an OCPI document carries — a meter's three
    // decimals, an hour's four, a currency's two — survives the parse exactly.
    assert_eq!(parsed.total_energy.to_string(), "18.512");
    assert_eq!(parsed.total_time.to_string(), "0.5166");
    assert_eq!(
        parsed.total_cost.before_taxes.to_string(),
        "8.82",
        "money above all: a cent read through a binary float is a cent two companies argue about"
    );
}

/// `[OCPI 2.3.0 §mod_cdrs]` makes a Receiver return a `Location` so the sender
/// can read back **what it sent** — which is the sender's document, not this
/// side's canonical model rebuilt.
#[tokio::test]
async fn a_pushed_record_can_be_read_back_as_the_document_that_arrived() {
    let (mine, node) = this_node().await;
    let client = OcpiClient::with_config(KitConfig::for_testing()).expect("a client");

    // Registered, so the push is authenticated the way a real one is.
    let peer = peer_platform().await;
    let theirs = ocpi_kit::testkit::sample::credentials(
        "TOKEN-B-FROM-THE-PEER",
        &format!("http://{peer}/ocpi/versions"),
    )
    .expect("a credentials object");
    ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        CredentialsToken::new("TOKEN-A-HANDED-OVER-BY-EMAIL").expect("a token"),
    )
    .discover(client.transport())
    .await
    .expect("versions")
    .select_best(client.transport())
    .await
    .expect("details")
    .register(client.transport(), &theirs)
    .await
    .expect("the handshake completes");

    let token_c = CredentialsToken::new("TOKEN-C-FOR-NL*TNM").expect("a token");

    // A document owned by the platform that is pushing it. Anything else is
    // refused a step earlier — see the test below.
    let document: ocpi_kit::v2_3_0::cdrs::Cdr = {
        let (_, _, rendered) = queued();
        let Outbound::V2_3_0(cdr) = rendered else {
            panic!("this partner speaks 2.3.0");
        };
        let mut cdr = *cdr;
        cdr.country_code = ocpi_kit::types::CiString::new("NL").expect("a country");
        cdr.party_id = ocpi_kit::types::CiString::new("TNM").expect("a party");
        cdr
    };
    let cdrs = advertised(
        &client,
        mine,
        &token_c,
        ModuleId::Cdrs,
        ocpi_kit::InterfaceRole::Receiver,
    )
    .await;
    let push = OcpiRequest::new(
        http::Method::POST,
        KitUrl::new_lenient(cdrs),
        ModuleId::Cdrs,
    )
    .with_body(&document)
    .expect("encodes");
    let (pushed, headers) = client
        .transport()
        .send_with_headers::<serde_json::Value>(&push, &token_c, &Default::default())
        .await
        .expect("the push reaches this node");
    assert!(pushed.is_success(), "{pushed:?}");
    let location =
        ocpi_kit::transport::header_str(&headers, &ocpi_kit::transport::headers::LOCATION)
            .expect("the paragraph makes the Location mandatory on the response")
            .to_owned();

    // …and reading that URL back gives the sender its own document.
    let back = OcpiRequest::new(
        http::Method::GET,
        KitUrl::new_lenient(location),
        ModuleId::Cdrs,
    );
    let read: ocpi_kit::v2_3_0::cdrs::Cdr = client
        .transport()
        .send(&back, &token_c, &Default::default())
        .await
        .expect("the URL the Location named answers");
    assert_eq!(
        serde_json::to_value(&read).expect("serialises"),
        serde_json::to_value(&document).expect("serialises"),
        "a sender reading back what it sent must get what it sent, not a rebuild of it"
    );

    // …and it is a fact on the record, so a restart answers the same.
    assert!(
        node.lock()
            .expect("not poisoned")
            .journal()
            .records()
            .expect("the chain holds")
            .iter()
            .any(|r| r.kind == "received")
    );
}

/// The leak the authority model exists to prevent, at the wire.
///
/// A platform speaks for the parties its credentials listed. A record naming a
/// different one is refused — and with a **404**, because
/// `[OCPI 2.3.0 §transport_and_format]` says a receiver may block access this
/// way rather than reveal whether the object exists.
///
/// The check that matters is the inner one: `Inbox::post_cdr` builds its
/// principal from the **credential**, never from the document. Built from the
/// document, `Roamd::receive` would compare a claim with itself and every push
/// would self-authorise — a partner settling records in any operator's name by
/// writing that name in the payload (D332).
#[tokio::test]
async fn a_platform_cannot_push_a_record_that_names_another_party() {
    let (mine, _node) = this_node().await;
    let client = OcpiClient::with_config(KitConfig::for_testing()).expect("a client");
    let peer = peer_platform().await;
    let theirs = ocpi_kit::testkit::sample::credentials(
        "TOKEN-B-FROM-THE-PEER",
        &format!("http://{peer}/ocpi/versions"),
    )
    .expect("a credentials object");
    ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        CredentialsToken::new("TOKEN-A-HANDED-OVER-BY-EMAIL").expect("a token"),
    )
    .discover(client.transport())
    .await
    .expect("versions")
    .select_best(client.transport())
    .await
    .expect("details")
    .register(client.transport(), &theirs)
    .await
    .expect("the handshake completes");

    // The platform registered for NL*TNM. This record says DE*ABC.
    let (_, _, rendered) = queued();
    let Outbound::V2_3_0(document) = rendered else {
        panic!("this partner speaks 2.3.0");
    };
    assert_eq!(document.country_code.as_str(), "DE");

    let token_c = CredentialsToken::new("TOKEN-C-FOR-NL*TNM").expect("a token");
    let cdrs = advertised(
        &client,
        mine,
        &token_c,
        ModuleId::Cdrs,
        ocpi_kit::InterfaceRole::Receiver,
    )
    .await;
    let push = OcpiRequest::new(
        http::Method::POST,
        KitUrl::new_lenient(cdrs),
        ModuleId::Cdrs,
    )
    .with_body(document.as_ref())
    .expect("encodes");
    let refused = client
        .transport()
        .send_with_headers::<serde_json::Value>(&push, &token_c, &Default::default())
        .await
        .expect_err("a platform may not write another party's records");
    // The **authority model** answers, not the transport: the grant is judged
    // against the credential and the record's owner, and it says so by name.
    // That is the check D332 is about — built from the document instead, it
    // would have compared a claim with itself and passed.
    let said = refused.to_string();
    assert!(
        said.contains("emob.cdr.write") && said.contains("DE*ABC"),
        "the refusal has to name the grant and the owner it does not reach: {said}"
    );
}

/// The URL this node **advertises** for a module, read out of its own
/// version-details document.
///
/// Never a hand-written path. The kit mounts Receiver routes under a prefix and
/// advertises them with it, so a test that guessed `/ocpi/cdrs` would fail
/// against a node that is behaving correctly — which is the failure
/// `inbound::router` exists to make impossible and which this helper is how a
/// peer avoids.
async fn advertised(
    client: &OcpiClient,
    mine: std::net::SocketAddr,
    token: &CredentialsToken,
    module: ModuleId,
    role: ocpi_kit::InterfaceRole,
) -> String {
    let selected = ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
        token.clone(),
    )
    .discover(client.transport())
    .await
    .expect("versions")
    .select_best(client.transport())
    .await
    .expect("details");
    selected
        .details()
        .endpoints
        .iter()
        .find(|endpoint| endpoint.identifier == module && endpoint.role == role)
        .unwrap_or_else(|| panic!("this node advertises no {module}/{role}"))
        .url
        .as_str()
        .to_owned()
}

/// SR-045 (baseline SR-010): a registrant who names an internal address gets a
/// refusal, and the address gets no request.
#[tokio::test]
async fn sr_045_a_handshake_pointed_inward_is_refused_before_any_request() {
    // A listener that counts connections: the "internal service".
    let inside = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let inside_addr = inside.local_addr().expect("bound");
    let reached = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&reached);
    tokio::spawn(async move {
        while inside.accept().await.is_ok() {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });

    let (mine, node) = this_node_reaching(roamd::reach::ReachBack::strict()).await;
    let client = OcpiClient::with_config(KitConfig::for_testing()).expect("a client");
    let token_a = CredentialsToken::new("TOKEN-A-HANDED-OVER-BY-EMAIL").expect("a token");

    for url in [
        "http://169.254.169.254/latest/meta-data".to_owned(),
        format!("https://{inside_addr}/ocpi/versions"),
        format!("http://{inside_addr}/ocpi/versions"),
    ] {
        let theirs = ocpi_kit::testkit::sample::credentials("TOKEN-B-FROM-THE-PEER", &url)
            .expect("a credentials object");
        let selected = ocpi_kit::client::Registration::new(
            KitUrl::new_lenient(format!("http://{mine}/ocpi/versions")),
            token_a.clone(),
        )
        .discover(client.transport())
        .await
        .expect("versions")
        .select_best(client.transport())
        .await
        .expect("details");
        let refused = selected.register(client.transport(), &theirs).await;
        assert!(refused.is_err(), "SR-045: {url} was registered");
    }
    assert_eq!(
        reached.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "SR-045: no request reached the internal address"
    );
    assert_eq!(
        node.lock()
            .expect("not poisoned")
            .state()
            .peers()
            .registered()
            .count(),
        0,
        "and nothing was registered"
    );
}

/// SC-001, SR-096: the cut the chain cannot see — the **last** record removed —
/// is refused at start, because the head signed beside the journal still names
/// the position it was cut from.
#[test]
fn sr_096_a_journal_cut_by_one_record_does_not_start() {
    let dir = std::env::temp_dir().join(format!("emob-roamd-{}-sr096", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal = dir.join("roamd.ndjson");
    let estate: roamd::daemon::Estate = emob_service::estate::from_str(
        r#"
        stations = []

        [parties]
        operator = "DE*ABC"
        provider = "DE*EMP"

        [services]
        billd = "http://billd"
        roamd = "http://roamd"
        empd = "http://empd"
        webhook_secret_env = "WEBHOOK_SECRET"
        service_token_env = "SERVICE_TOKEN"

        [roaming]
        public_url = "http://roamd/ocpi"
        "#,
    )
    .expect("an estate");
    let data = emob_service::vault::DataKey::generate();
    let signing = emob_service::head::SigningKey::generate();
    let env: emob_service::estate::Env = Arc::new(move |name: &str| match name {
        roamd::daemon::DATA_KEY => Some(data.clone()),
        "ROAMD_SIGNING_KEY" => Some(signing.clone()),
        _ => None,
    });

    {
        let mut node = roamd::daemon::open(&estate, &journal, &env).expect("an empty journal");
        node.offer(&Actor::Estate, "NL*TNM", "TOKEN-A-ONE")
            .expect("recorded");
        node.offer(&Actor::Estate, "BE*XYZ", "TOKEN-A-TWO")
            .expect("recorded");
        let partner = Actor::partner(&PartyId::new("NL", "TNM").expect("a party"));
        node.unregister(&partner, "NL*TNM").expect("recorded");
    }
    // The node restarts over its own history.
    drop(roamd::daemon::open(&estate, &journal, &env).expect("the journal it signed"));

    let text = std::fs::read_to_string(&journal).expect("read");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "three facts");
    std::fs::write(&journal, format!("{}\n", lines[..2].join("\n"))).expect("written");

    let refused = roamd::daemon::open(&estate, &journal, &env)
        .expect_err("SR-096: a journal one record short of its signed head does not start");
    let said = format!("{refused:#}");
    assert!(
        said.contains("holds 2 records") && said.contains("signed a head at 3"),
        "the refusal names both positions: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
