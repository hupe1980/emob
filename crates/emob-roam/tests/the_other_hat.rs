//! The EMP half: a partner's record, re-rated at this side's retail price and
//! invoiced to a driver.
//!
//! # Why this test spans four crates
//!
//! [OVERVIEW.md] claims the two roles are one platform: the CPO that sells a
//! session to an eMSP and the eMSP that sells the same session to its driver.
//! The CPO half is proven by `the_same_session.rs` — one signed session settles
//! at the same money over three wires. **This is the other half**, and until it
//! existed the claim rested on a path with no composition test and, underneath
//! that, on an API that did not exist: `from_ocpi` lands a partner's record
//! unpriced and nothing re-rated it.
//!
//! Doing it by hand — `rate(&retail, &cdr.chargeable()?)` — is the obvious way
//! and it silently skips every gate the issuing side applies. So the path is
//! `Cdr::rerated_with`, the same door the CPO's own builder uses, and this
//! asserts the whole chain: partner document → canonical record → this side's
//! price → the driver's invoice → the books.
//!
//! [OVERVIEW.md]: https://github.com/hupe1980/emob

use emob_billing::invoice::{Counterparty, InvoiceBuilder};
use emob_billing::tax::TaxStatus;
use emob_cdr::{CdrBuilder, EvidenceRef};
use emob_core::{ClockResolution, Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_poi::site::{
    Address, ChargingPoint, Connector, ConnectorType, Coordinates, Facility, Site,
};
use emob_roam::ocpi::cdr::{Context, SignedPayload, to_ocpi};
use emob_roam::ocpi::inbound::from_ocpi;
use emob_roam::ocpi::location::cdr_location;
use emob_roam::{Partner, RoamingToken, TokenType};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::{Curve, PublicKey};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::{date, datetime};

const METER_SERIAL: &str = "BQ27400330016";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(dec(s)).unwrap()
}

fn at(minute: i64) -> time::OffsetDateTime {
    datetime!(2026-06-02 10:00 +2) + time::Duration::minutes(minute)
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
}

/// A genuinely signed record, so the far end verifies rather than trusts.
fn signed_record(pagination: u64, marker: &str, register: &str, minute: i64) -> String {
    let payload = format!(
        r#"{{"FV":"1.4","GI":"ACME CS-1","GS":"GW-1","PG":"T{pagination}","MV":"Phoenix Contact","MM":"EEM-350-D-MCB","MS":"{METER_SERIAL}","IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","RD":[{{"TM":"2026-06-02T{:02}:{:02}:00,000+0200 S","TX":"{marker}","RV":{register},"RI":"01-00:B2.08.00*FF","RU":"kWh","RT":"AC","EF":"","ST":"G"}}]}}"#,
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

fn raw_records() -> Vec<String> {
    vec![
        signed_record(1, "B", "100.000", 0),
        signed_record(2, "E", "129.500", 30),
    ]
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

fn session() -> Session {
    let mut session = Session::open(
        "s-emp".parse().unwrap(),
        "DE*AB7*E840*6487".parse().unwrap(),
        Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: "c-1".parse().unwrap(),
                emaid: Some("NL-TNM-C00122045-K".parse().unwrap()),
            },
            token_ref: Some(emob_session::TokenRef::new("a".repeat(64)).unwrap()),
            authorization_reference: Some("auth-77".into()),
        },
        at(0),
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
                        at(30),
                        kwh("129.500"),
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

/// What the **CPO** charges the provider: 0.49 gross per kWh.
fn wholesale() -> Tariff {
    Tariff::simple(
        "cpo-ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

/// What the **eMSP** charges its own driver: 0.59 gross per kWh, under a
/// contract rather than ad hoc.
fn retail() -> Tariff {
    Tariff::simple(
        "emsp-retail".parse().unwrap(),
        Currency::EUR,
        TariffKind::Contract,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec("0.59")).with_vat(dec("19"))],
    )
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

#[test]
fn a_partners_record_becomes_this_sides_invoice() {
    // ── 1. The CPO's side, as `the_same_session.rs` already proves ──────────
    let texts = raw_records();
    let records: Vec<_> = texts
        .iter()
        .map(|r| ocmf::Record::parse(r).expect("well-formed OCMF"))
        .collect();
    let theirs = Evidence::assemble(&records, &registry(), at(0));
    let binding = wholesale();
    let issued = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(
            PartyId::new("DE", "ABC").unwrap(),
            "cdr-77".parse().unwrap(),
        )
        .evidence(EvidenceRef::from_evidence(&theirs, "OCMF"))
        .rated_with(&binding)
        .build()
        .expect("a genuine session builds a CDR");

    let token = RoamingToken::new(
        PartyId::new("NL", "TNM").unwrap(),
        "045F2C9A",
        TokenType::Rfid,
        "NL-TNM-C00122045-K".parse().unwrap(),
    )
    .unwrap();
    let point = point();
    let context = Context {
        token: &token,
        location: cdr_location(&site(), &point, &point.connectors[0], 0).unwrap(),
        signed: texts
            .iter()
            .zip(["Start", "End"])
            .map(|(record, nature)| SignedPayload {
                nature: (*nature).to_owned(),
                plain_data: record.clone(),
                signed_data: record.clone(),
            })
            .collect(),
        public_key: None,
        last_updated: at(31),
    };
    let partner = Partner::emsp(PartyId::new("NL", "TNM").unwrap());
    let wire = to_ocpi(&issued, &partner, &context)
        .expect("a rated import CDR crosses")
        .into_value_discarding_notes();

    // ── 2. …and this side is the eMSP receiving it ──────────────────────────
    // The payloads are verified against **our** registry, never the key the
    // document carries.
    let payloads = emob_roam::ocpi::cdr::inbound_payloads(&wire);
    let received: Vec<_> = payloads
        .iter()
        .map(|p| ocmf::Record::parse(&p.signed_data).expect("verbatim OCMF"))
        .collect();
    let verified = Evidence::assemble(&received, &registry(), at(0));
    assert_eq!(
        verified.billable_energy().unwrap().to_string(),
        "29.500 kWh",
        "the far end re-verifies and gets the same energy"
    );

    let inbound = from_ocpi(&wire, Some(EvidenceRef::from_evidence(&verified, "OCMF")))
        .expect("the document is one this side can read")
        .into_value_discarding_notes();
    let theirs_says = inbound.stated_total;
    assert!(
        inbound.cdr.cost.is_none(),
        "a partner's record arrives unpriced: rebuilding a `Rated` from totals \
         would make the pre-flight check our own arithmetic"
    );
    // The provider's own handle on the authorisation it granted came back, so
    // this record can be tied to the `Authorize` this side answered.
    assert_eq!(
        inbound.cdr.authorization_reference.as_deref(),
        Some("auth-77")
    );

    // ── 3. The eMSP prices it with its **own** retail tariff ───────────────
    // Through the same door the issuer used, so every gate applies: a retail
    // tariff not in force, a version the meter says was superseded mid-session,
    // a duration the signed records do not vouch for, and the clock resolution
    // under a per-minute fee.
    let retail = retail();
    let mine = inbound
        .cdr
        .rerated_with(&retail, ClockResolution::conforming())
        .expect("this side's own tariff prices the record");

    // The two numbers are about the same session by construction: same periods,
    // same energy, same evidence — only the price differs.
    assert_eq!(mine.total_energy, inbound.cdr.total_energy);
    assert_eq!(mine.periods, inbound.cdr.periods);
    assert_eq!(mine.evidence, inbound.cdr.evidence);
    assert!(mine.was_priced_with(&retail), "…and it names which tariff");

    // 29.500 kWh at 0.59 gross to the driver, against 0.49 to the provider.
    assert_eq!(mine.total_cost().unwrap().to_string(), "17.41 EUR");
    assert_eq!(theirs_says.to_string(), "14.46 EUR");
    let margin = (mine.total_cost().unwrap() - theirs_says).unwrap();
    assert_eq!(
        margin.to_string(),
        "2.95 EUR",
        "the comparison a settlement conversation is about"
    );

    // ── 4. …and the driver's invoice comes out of it ────────────────────────
    // `[UStG §3g]` does **not** move this leg: the eMSP supplies its own
    // customer, who consumes what they buy, so the supply is taxed where the
    // electricity was drawn.
    let crossing = InvoiceBuilder::new(
        "EMP-2026-0001",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        Counterparty::new(
            "Nederlandse Mobiliteit",
            "Amsterdam",
            TaxStatus::reseller("NL", "NL001234567B01"),
        )
        .at("Keizersgracht 1", "1015"),
        Counterparty::new("Fahrerin", "Berlin", TaxStatus::consumer("DE")).at("Weg 2", "10119"),
    )
    .supplied_from("DE", dec("19"))
    .record(&mine)
    .due_on(date!(2026 - 07 - 15))
    .build()
    .expect("the driver's invoice");
    let invoice = crossing.value;

    assert_eq!(invoice.treatment.place_of_supply, "DE");
    assert_eq!(
        invoice.treatment.category,
        emob_billing::VatCategory::Standard,
        "a driver is not a reseller, so [UStG §3g] never engages"
    );
    assert_eq!(invoice.gross_total().to_string(), "17.41 EUR");
    assert!(invoice.reconciles());
    assert!(
        invoice
            .lines
            .iter()
            .all(emob_billing::InvoiceLine::reconciles)
    );

    // …and the books balance on the eMSP's own side.
    let books = emob_billing::postings::postings_for(&invoice);
    assert!(books.balances(), "{books:?}");
    assert_eq!(books.debits().to_string(), "17.41 EUR");

    // The document validates as the European e-invoice it claims to be.
    let crossed = emob_billing::en16931::to_en16931(&invoice, emob_billing::en16931::CEN_CORE)
        .expect("the crossing");
    assert!(
        crossed.value.is_valid(),
        "{:?}",
        crossed.value.reasons().collect::<Vec<_>>()
    );
}

#[test]
fn re_rating_a_partners_record_applies_the_gates_the_issuer_applied() {
    // The reason `rerated_with` exists rather than leaving the composition to a
    // caller. `rate(&retail, &cdr.chargeable()?)` is the obvious way to do it
    // and it skips all four gates — silently, on every record.
    let texts = raw_records();
    let records: Vec<_> = texts
        .iter()
        .map(|r| ocmf::Record::parse(r).unwrap())
        .collect();
    let evidence = Evidence::assemble(&records, &registry(), at(0));
    let binding = wholesale();
    let issued = CdrBuilder::from_session(&session(), Direction::Import)
        .unwrap()
        .key(
            PartyId::new("DE", "ABC").unwrap(),
            "cdr-78".parse().unwrap(),
        )
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&binding)
        .build()
        .unwrap();

    // A retail tariff that was not in force when the session ran did not price
    // it, whatever its numbers say `[AFIR Art. 5(4)]`.
    let future = retail().valid_between(Some(at(45)), None);
    assert!(matches!(
        issued.rerated_with(&future, ClockResolution::conforming()),
        Err(emob_cdr::CdrError::TariffNotInForce { .. })
    ));

    // A duration the signed records do not vouch for is not one this side may
    // bill either, however the CPO priced it.
    let mut undefendable = issued.clone();
    undefendable.evidence.as_mut().unwrap().duration_billable = false;
    let by_the_minute = Tariff::simple(
        "emsp-minute".parse().unwrap(),
        Currency::EUR,
        TariffKind::Contract,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Time, dec("6.00")).with_vat(dec("19"))],
    );
    assert!(matches!(
        undefendable.rerated_with(&by_the_minute, ClockResolution::conforming()),
        Err(emob_cdr::CdrError::DurationNotBillable { .. })
    ));

    // …and a tariff change the meter signed inside the session contradicts any
    // single price, including this side's.
    let mut changed = issued.clone();
    changed.evidence.as_mut().unwrap().tariff_changes = vec![at(15)];
    assert!(matches!(
        changed.rerated_with(&retail(), ClockResolution::conforming()),
        Err(emob_cdr::CdrError::SignedTariffChangeInsideSession { .. })
    ));
}
