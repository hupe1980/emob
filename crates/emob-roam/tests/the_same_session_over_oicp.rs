//! One session, out over Hubject, and the same energy on the far side.
//!
//! The property M4b exists to prove, and the shape of it is **not** the OCPI
//! one. `the_same_session.rs` asserts that a session settles at the same
//! *money* over three OCPI paths, because an OCPI CDR carries `total_cost`.
//! An OICP charge detail record carries no cost field at all: the provider
//! re-derives what it owes from a pricing product the two parties agreed on
//! beforehand `[OICP 2.3 §PricingProductData]`.
//!
//! So what has to hold here is what the wire actually carries — the **energy**,
//! the **evidence** and the **identity** — plus the two things that follow from
//! it: that the price this side computed is reported as not having crossed, and
//! that the product it must be re-derived from is a document this crate can
//! produce.
//!
//! Everything is signed for real: the OCMF records are produced with a private
//! key at test time and verified through the same code path a station would go
//! through. And the broker is real enough to refuse: `MockHubject` is
//! `oicp-kit`'s in-process Hubject, which validates a CDR the way the live one
//! does and rejects a record for a session it never opened.

use emob_cdr::{CdrBuilder, EvidenceRef};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_roam::oicp::cdr::{Context, MeterWindow, check_conserves, inbound_payloads, to_oicp};
use emob_roam::oicp::pricing::to_oicp_product;
use emob_roam::{Partner, RoamError, RoamingToken, SignedPayload, TokenType};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Restrictions, Tariff, TariffElement, TariffKind};
use ocmf::{Curve, PublicKey};
use oicp_kit::testkit::{MockEmp, MockHubject, samples};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::datetime;

const METER_SERIAL: &str = "BQ27400330016";
const EVSE: &str = "DE*AB7*E840*6487";

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

/// The **receiver's** registry: the key arrives out of band, never beside the
/// record it signs.
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

/// A 30-minute session on a contract, 18.000 kWh, ending at 10:30 — and the
/// vehicle sits connected for five minutes before the charge begins, which is
/// the span OICP can state and OCPI cannot.
fn session() -> Session {
    let mut session = Session::open(
        "s-1".parse().unwrap(),
        EVSE.parse().unwrap(),
        Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: "NL-TNM-C00122045-K".parse().unwrap(),
                emaid: Some("NL-TNM-C00122045-K".parse().unwrap()),
            },
            token_ref: Some(emob_session::TokenRef::new("a".repeat(64)).unwrap()),
            authorization_reference: Some("auth-9".into()),
        },
        at(-5),
    );
    session
        .transition_to(SessionState::Charging, at(0))
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
    let texts = raw_records();
    let records = texts
        .iter()
        .map(|r| ocmf::Record::parse(r))
        .collect::<Result<Vec<_>, _>>()
        .expect("well-formed OCMF");
    Evidence::assemble(&records, &registry(), at(-5))
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

fn token() -> RoamingToken {
    RoamingToken::new(
        PartyId::new("NL", "TNM").unwrap(),
        "045F2C9A",
        TokenType::Rfid,
        "NL-TNM-C00122045-K".parse().unwrap(),
    )
    .expect("the reference contract id checks out")
}

fn payloads() -> Vec<SignedPayload> {
    raw_records()
        .iter()
        .zip(["Start", "Progress", "End"])
        .map(|(record, nature)| SignedPayload {
            nature: (*nature).to_owned(),
            plain_data: record.clone(),
            signed_data: record.clone(),
        })
        .collect()
}

fn partner() -> Partner {
    Partner::emsp(PartyId::new("NL", "TNM").unwrap()).on_signed_data()
}

fn context<'a>(token: &'a RoamingToken, session_id: String) -> Context<'a> {
    Context {
        token,
        session_id,
        meter: Some(MeterWindow {
            start: kwh("100.000"),
            end: kwh("118.000"),
        }),
        signed: payloads(),
        public_key: Some(hex::encode(
            signing_key()
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        )),
        product_id: Some("AC1".to_owned()),
        calibration_certificate: Some("PTB - 8.51-2020-01 : V1 : 01Jan2020".to_owned()),
        verification_url: Some("https://cpo.example/transparency/cdr-1".to_owned()),
    }
}

/// A broker that knows this charging point and one provider that says yes.
fn broker() -> (MockHubject, String) {
    let mut hubject = MockHubject::new();
    hubject.register_emp(MockEmp::permissive("NL-TNM".parse().unwrap()));
    hubject
        .push_evse_data(&samples::evse_data_record(EVSE).into())
        .expect("the sample record is conformant");
    let response = hubject.authorize_start(&samples::authorize_start_request(EVSE));
    let session_id = response
        .session_id
        .clone()
        .expect("the broker opened a session")
        .as_str()
        .to_owned();
    (hubject, session_id)
}

#[test]
fn one_signed_session_crosses_hubject_and_arrives_with_its_energy_and_its_evidence() {
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let (hubject, session_id) = broker();

    // What the session is worth on this side, computed once from its own
    // quarter hours.
    let priced = cdr.cost.as_ref().expect("rated").rated.total();
    assert_eq!(priced.to_string(), "8.82 EUR");
    assert_eq!(cdr.total_energy, kwh("18.000"));

    let crossing = to_oicp(&cdr, &partner(), &context(&token, session_id.clone()))
        .expect("a genuine record crosses");
    let record = &crossing.value;

    // ── The broker accepts it, which is the half a schema check cannot prove ─
    let ack = hubject
        .submit_cdr(record)
        .expect("Hubject accepts a conformant record for a session it opened");
    assert!(ack.result);

    // …and the same record a second time is refused, because Hubject accepts
    // one CDR per session. A retry is not a second sale.
    assert!(hubject.submit_cdr(record).is_err());

    // ── What the provider pulls back ────────────────────────────────────────
    let settled = hubject.cdrs();
    assert_eq!(settled.len(), 1);
    let arrived = &settled[0];

    // The energy survived, and OICP's own defining equation holds on it:
    // `ConsumedEnergy` is `MeterValueEnd - MeterValueStart`, exactly.
    assert_eq!(
        check_conserves(arrived).expect("the register and the total agree"),
        kwh("18.000")
    );
    assert_eq!(
        arrived.metered_energy().map(|n| n.get()),
        Some(dec("18.000"))
    );

    // ── And the evidence re-verifies at the far end ─────────────────────────
    //
    // Against the **receiver's** registry, from the bytes that arrived, which is
    // the whole point of carrying them verbatim.
    let texts: Vec<String> = inbound_payloads(arrived)
        .into_iter()
        .map(|payload| payload.signed_data)
        .collect();
    assert_eq!(texts.len(), 3);
    let records = texts
        .iter()
        .map(|text| ocmf::Record::parse(text))
        .collect::<Result<Vec<_>, _>>()
        .expect("the records crossed verbatim");
    let far_side = Evidence::assemble(&records, &registry(), at(-5));
    assert_eq!(
        far_side.billable_energy(),
        Some(kwh("18.000")),
        "the same kilowatt-hours, re-derived from the signatures on the other side"
    );
}

#[test]
fn the_money_does_not_cross_and_the_record_says_so() {
    // The headline difference from OCPI, and the reason this test file exists
    // rather than a fourth case in `the_same_session.rs`. An OICP CDR has no
    // cost field of any kind: the provider re-derives what it owes from the
    // pricing product. A crossing that said nothing about that would leave two
    // companies to discover it from a reconciliation report.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let (_, session_id) = broker();

    let crossing = to_oicp(&cdr, &partner(), &context(&token, session_id)).unwrap();

    let account: Vec<String> = crossing.reasons().collect();
    assert!(
        account.iter().any(|reason| reason.contains("8.82 EUR")
            && reason.contains("no cost field")
            && reason.contains("PartnerProductID")),
        "{account:#?}"
    );

    // …and the record really does carry no amount anywhere.
    let json = serde_json::to_string(&crossing.value).unwrap();
    assert!(!json.contains("8.82"), "{json}");
    assert!(json.contains("\"PartnerProductID\":\"AC1\""), "{json}");
}

#[test]
fn the_span_ocpi_has_to_guess_at_is_stated_here() {
    // A translation is not only a loss. OICP has four timestamps where OCPI has
    // two, so the five minutes the vehicle sat connected before its charge
    // began — which an OCPI reader attributes to whichever measured period
    // precedes it — is a fact on this document rather than an inference.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let (_, session_id) = broker();

    let crossing = to_oicp(&cdr, &partner(), &context(&token, session_id)).unwrap();
    let record = &crossing.value;

    assert_eq!(record.session_start.as_offset(), Some(at(-5)));
    assert_eq!(record.charging_start.as_offset(), Some(at(0)));
    assert_eq!(record.charging_end.as_offset(), Some(at(30)));
    assert_eq!(record.session_end.as_offset(), Some(at(30)));
    assert_eq!(record.session_duration_seconds(), Some(35 * 60));
    assert_eq!(record.charging_duration_seconds(), Some(30 * 60));

    assert!(
        crossing
            .reasons()
            .any(|reason| reason.contains("300 s") && reason.contains("connected time")),
        "{:#?}",
        crossing.reasons().collect::<Vec<_>>()
    );
}

#[test]
fn a_mac_address_session_does_not_become_a_plug_and_charge_contract() {
    // OCPI collapses Plug & Charge and AutoCharge into one `AUTH_REQUEST` that
    // names neither, and the crossing there reports the lost distinction. OICP's
    // `PlugAndChargeIdentification` **names** ISO 15118 — so putting a MAC
    // address in it asserts a contract certificate that was never presented,
    // which is a lie rather than a loss (D233).
    let evidence = evidence();
    let mut cdr = cdr(&evidence);
    cdr.auth_path = emob_session::AuthPath::AutoCharge;
    let token = token();
    let (_, session_id) = broker();

    let err = to_oicp(&cdr, &partner(), &context(&token, session_id)).unwrap_err();
    assert!(matches!(err, RoamError::AutoChargeNotExpressible), "{err}");
    assert!(err.to_string().contains("ISO 15118"), "{err}");
}

#[test]
fn a_discharge_does_not_cross_here_either() {
    // `ConsumedEnergy` has no sign, exactly as OCPI's `total_energy` has none,
    // so a V2G discharge would settle backwards on this wire too.
    let evidence = evidence();
    let mut cdr = cdr(&evidence);
    cdr.direction = Direction::Export;
    let token = token();
    let (_, session_id) = broker();

    assert!(matches!(
        to_oicp(&cdr, &partner(), &context(&token, session_id)),
        Err(RoamError::ExportNotExpressible { .. })
    ));
}

#[test]
fn a_register_that_disagrees_with_the_record_is_caught_before_the_broker_does() {
    // OICP *defines* `ConsumedEnergy` as `MeterValueEnd - MeterValueStart`, and
    // Hubject validates it. Finding the disagreement here is the difference
    // between a note to an operator and a rejected record after the driver has
    // gone.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let (_, session_id) = broker();

    let mut context = context(&token, session_id);
    context.meter = Some(MeterWindow {
        start: kwh("100.000"),
        end: kwh("117.000"),
    });

    let err = to_oicp(&cdr, &partner(), &context).unwrap_err();
    assert!(matches!(err, RoamError::RegisterDisagrees { .. }), "{err}");
}

#[test]
fn a_record_for_a_session_the_broker_never_opened_is_refused() {
    // The most common integration failure, and the one a schema check cannot
    // find: a CPO that invents its own session ids has every CDR rejected. The
    // id is the broker's, from the authorisation that started the session.
    let evidence = evidence();
    let cdr = cdr(&evidence);
    let token = token();
    let (hubject, _) = broker();

    let crossing = to_oicp(
        &cdr,
        &partner(),
        &context(&token, samples::session_id().as_str().to_owned()),
    )
    .expect("it is a conformant document; it is simply not this broker's session");

    let refusal = hubject
        .submit_cdr(&crossing.value)
        .expect_err("the broker refuses a session it never opened");
    assert!(!refusal.result);
}

#[test]
fn the_price_crosses_as_a_product_and_a_tier_does_not_cross_at_all() {
    // The other half of "settles identically": the amount is re-derived from a
    // pricing product, so the product has to be a document this crate can
    // produce — and where the tariff is a shape OICP cannot state, the crossing
    // refuses rather than flattening it onto its first band.
    let crossing = to_oicp_product(&tariff(), "AC1", dec("150")).expect("a flat tariff crosses");
    let product = &crossing.value;

    assert_eq!(product.product_id.as_str(), "AC1");
    assert_eq!(product.price_per_reference_unit.get(), dec("0.49"));
    assert_eq!(product.product_price_currency.as_str(), "EUR");
    assert!(product.is_valid_24hours);

    // The tax does not cross, and a provider reading a gross price as net is
    // out by the whole VAT rate.
    assert!(
        crossing
            .reasons()
            .any(|reason| reason.contains("gross") && reason.contains("no tax flag")),
        "{:#?}",
        crossing.reasons().collect::<Vec<_>>()
    );

    // …and a tiered tariff has no spelling here. A product has one base price.
    let tiered = Tariff {
        elements: vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("4")),
                    ..Restrictions::default()
                },
            },
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.50"))],
                restrictions: Restrictions::default(),
            },
        ],
        ..tariff()
    };
    let err = to_oicp_product(&tiered, "AC1", dec("150")).unwrap_err();
    assert!(
        matches!(
            err,
            RoamError::RestrictionNotExpressible {
                field: "min_kwh/max_kwh",
                ..
            }
        ),
        "{err}"
    );
}

/// What is *inside* the energy that settles, and what the record actually cost.
///
/// Two facts the account owed a partner and did not give. `[REA 6-A §3.2]` makes
/// telling the affected party what a measured value contains a duty, and a
/// settlement partner is one — the OCPI crossing had said it since D198 and this
/// wire had not. And the headline note read `cost.rated.total()`, which is the
/// session alone: a record carrying a reservation was reported to the partner at
/// a price the record itself contradicts (D250, D253).
#[test]
fn the_account_names_what_is_inside_the_energy_and_what_the_record_cost() {
    let evidence = evidence();
    let mut cdr = cdr(&evidence);
    let token = token();
    let (_hubject, session_id) = broker();

    // The meter compensated 150 Wh of cable, and the session was preceded by a
    // half-hour reservation this side priced.
    cdr.evidence.as_mut().unwrap().compensated_loss = Some(kwh("0.150"));
    let held = emob_tariff::Reservation::honoured(at(-30), at(0));
    cdr.reservation = Some(held);
    let binding = tariff();
    cdr.cost.as_mut().unwrap().reservation = Some(emob_tariff::rate_reservation(&binding, &held));

    let crossing =
        to_oicp(&cdr, &partner(), &context(&token, session_id)).expect("a lawful record crosses");
    let account: Vec<String> = crossing.reasons().collect();

    assert!(
        account
            .iter()
            .any(|note| note.contains("0.150") && note.contains("REA 6-A")),
        "the compensated loss is named: {account:#?}"
    );
    assert_eq!(
        cdr.total_cost().unwrap(),
        cdr.cost.as_ref().unwrap().gross(),
        "the record's own total is both parts"
    );
    let stated = cdr.total_cost().unwrap().to_string();
    assert!(
        account.iter().any(|note| note.contains(&stated)),
        "the account states what the record cost ({stated}), not the session alone: {account:#?}"
    );
}
