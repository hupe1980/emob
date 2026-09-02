//! A year of charging, filed: signed records → eligible points → a
//! `[38k §8(1)]` notification, and the four refusals that keep it honest.
//!
//! The property is not that the arithmetic runs. It is that **only energy a
//! meter signed at a point the Verordnung admits reaches the file**, and that
//! everything excluded says why — because a kilowatt-hour missing from a
//! notification is a kilowatt-hour nobody notices until the year is closed.

use emob_cdr::{CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::station::{
    Accessibility, ChargePointProfile, QuotaPosture, RegisterPublication, Registration,
};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::ocmf::KeyType;
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry, PublicKey, ocmf};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use emob_thg::{
    Attribution, ClaimBuilder, DirectSupply, DriveEfficiency, EmissionsBasis, RenewableSource,
    ThgError,
};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::{date, datetime};

const METER_SERIAL: &str = "BQ27400330016";
const EVSE: &str = "DE*AB7*E840*6487";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(dec(s)).unwrap()
}

fn day(n: i64) -> time::OffsetDateTime {
    datetime!(2026-06-01 10:00 +2) + time::Duration::days(n)
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
}

fn signed_record(
    session: u64,
    pagination: u64,
    marker: &str,
    register: &str,
    at: time::OffsetDateTime,
) -> String {
    let payload = format!(
        r#"{{"FV":"1.4","GI":"ACME CS-1","GS":"GW-1","PG":"T{pagination}","MV":"Phoenix Contact","MM":"EEM-350-D-MCB","MS":"{METER_SERIAL}","IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","ID":"s{session}","RD":[{{"TM":"{}-{:02}-{:02}T{:02}:{:02}:00,000+0200 S","TX":"{marker}","RV":{register},"RI":"01-00:B2.08.00*FF","RU":"kWh","RT":"AC","EF":"","ST":"G"}}]}}"#,
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
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
        )
        .unwrap();
    registry
}

fn session(n: i64, id: &str, from: &str, to: &str) -> (Session, Evidence) {
    let start = day(n);
    let end = start + time::Duration::minutes(30);
    let counter = u64::try_from(n).unwrap() * 2;

    let raw = [
        signed_record(counter, counter + 1, "B", from, start),
        signed_record(counter, counter + 2, "E", to, end),
    ];
    let records: Vec<_> = raw.iter().map(|r| ocmf::parse(r).unwrap()).collect();
    let evidence = Evidence::assemble(&records, &registry(), end);

    let mut session = Session::open(
        id.parse().unwrap(),
        EVSE.parse().unwrap(),
        Authorization::ad_hoc(),
        start,
    );
    session
        .transition_to(SessionState::Charging, start)
        .unwrap();
    session
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        start,
                        kwh(from),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    )
                    .signed(),
                    MeterReading::new(
                        end,
                        kwh(to),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    )
                    .signed(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    session.end(end, EndReason::Local).unwrap();
    (session, evidence)
}

fn tariff() -> Tariff {
    Tariff::simple(
        "ad-hoc-2026".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

/// Three days of charging, all of it signed.
fn year() -> CdrLedger {
    let mut ledger = CdrLedger::new();
    let party = PartyId::new("DE", "ABC").unwrap();
    let tariff = tariff();

    for (n, id, from, to) in [
        (0_i64, "s-1", "100.000", "129.500"),
        (1, "s-2", "129.500", "150.000"),
        (2, "s-3", "150.000", "163.333"),
    ] {
        let (session, evidence) = session(n, id, from, to);
        let cdr = CdrBuilder::from_session(&session, Direction::Import)
            .unwrap()
            .key(party.clone(), format!("cdr-{id}").parse().unwrap())
            .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
            .rated_with(&tariff)
            .build()
            .unwrap();
        assert!(ledger.accept(cdr).is_stored());
    }
    ledger
}

/// A point that meets every one of `[38k §6(3)]`'s four conditions.
fn eligible_point() -> ChargePointProfile {
    let mut point = ChargePointProfile::bare(EVSE.parse().unwrap(), date!(2025 - 03 - 01));
    point.registration = Registration::notified_on(date!(2025 - 03 - 10));
    point.quota = QuotaPosture {
        publication: RegisterPublication::Published,
        conformity_declared: true,
        operator_code_assigned: true,
        ..QuotaPosture::default()
    };
    point
}

fn basis() -> EmissionsBasis {
    EmissionsBasis::grid_average(dec("96"), "BAnz AT 31.10.2025 B5").unwrap()
}

fn builder() -> ClaimBuilder {
    ClaimBuilder::new(
        2026,
        Attribution::own("AB7"),
        basis(),
        DriveEfficiency::BatteryElectric,
    )
    .unwrap()
}

#[test]
fn a_signed_year_reaches_the_notification() {
    let ledger = year();
    let mut claim = builder();
    claim
        .point(&eligible_point(), "Musterstraße 1, 10115 Berlin", &ledger)
        .unwrap();
    let filed = claim.build().unwrap();

    assert!(
        filed.is_lossless(),
        "{:?}",
        filed.reasons().collect::<Vec<_>>()
    );
    let claim = &filed.value;

    // One line, five fields, in `[38k §6(1) S. 2]`'s order.
    assert_eq!(claim.records.len(), 1);
    let record = &claim.records[0];
    assert_eq!(record.evse_id.canonical(), "DEAB7E8406487");
    assert_eq!(record.location, "Musterstraße 1, 10115 Berlin");
    assert_eq!(record.sessions, 3);

    // 29.500 + 20.500 + 13.333, to the watt-hour the meter stated.
    assert_eq!(record.energy.kwh(), dec("63.333"));
    assert_eq!(record.megawatt_hours(), dec("0.063333"));

    // Nr. 5: the withdrawal did not span the year, so the window is stated.
    let window = record.window.expect("three days is not a year");
    assert_eq!(window.from, date!(2026 - 06 - 01));
    assert_eq!(window.to, date!(2026 - 06 - 03));

    // `[38k §5(3)]`: × 3 for 2026, × 96 g/MJ, × 0.4 from Anlage 3.
    assert_eq!(claim.counted_megawatt_hours(), dec("0.189999"));
    assert_eq!(claim.emissions_kg_co2e(), dec("26.26546176"));
}

#[test]
fn a_year_the_verordnung_states_no_factor_for_is_refused() {
    let err = ClaimBuilder::new(
        2023,
        Attribution::own("AB7"),
        basis(),
        DriveEfficiency::BatteryElectric,
    )
    .unwrap_err();
    assert!(matches!(err, ThgError::YearNotCounted { year: 2023, .. }));
}

#[test]
fn a_point_that_fails_one_of_the_four_conditions_is_refused_by_remedy() {
    let ledger = year();

    for break_it in [
        |p: &mut ChargePointProfile| p.quota.publication = RegisterPublication::Withheld,
        |p: &mut ChargePointProfile| p.quota.conformity_declared = false,
        |p: &mut ChargePointProfile| p.quota.operator_code_assigned = false,
        |p: &mut ChargePointProfile| {
            p.quota.further_identifiers = emob_core::station::FurtherIdentifiers::Missing;
        },
    ] {
        let mut point = eligible_point();
        break_it(&mut point);
        let err = builder().point(&point, "anywhere", &ledger).unwrap_err();
        let ThgError::NotEligible { remedy, .. } = &err else {
            panic!("expected NotEligible, got {err}");
        };
        assert!(remedy.contains("consent to its publication"), "{remedy}");
    }
}

#[test]
fn consent_to_publication_is_enough_but_the_anzeige_is_still_presupposed() {
    let ledger = year();

    // `[38k §6(3) Nr. 1]` is a disjunction: consent alone satisfies it.
    let mut consented = eligible_point();
    consented.quota.publication = RegisterPublication::ConsentGiven;
    assert!(builder().point(&consented, "here", &ledger).is_ok());

    // But there is nothing to publish without the `[LSV26 §4(1) Nr. 1]` notice.
    let mut unnotified = eligible_point();
    unnotified.registration = Registration::default();
    assert!(matches!(
        builder().point(&unnotified, "here", &ledger).unwrap_err(),
        ThgError::NotEligible { .. }
    ));
}

#[test]
fn a_private_point_is_out_of_scope_rather_than_failing() {
    let ledger = year();
    let mut point = eligible_point();
    point.accessibility = Accessibility::Private;
    assert!(matches!(
        builder().point(&point, "a depot", &ledger).unwrap_err(),
        ThgError::NotPublic { .. }
    ));
}

#[test]
fn a_point_outside_the_electricity_tax_territory_is_refused() {
    let ledger = year();
    let mut point = eligible_point();
    point.evse_id = "FR*AB7*E840*6487".parse().unwrap();
    assert!(matches!(
        builder().point(&point, "Strasbourg", &ledger).unwrap_err(),
        ThgError::OutsideTaxTerritory { .. }
    ));
}

#[test]
fn a_filer_without_the_operators_designation_cannot_claim_its_points() {
    let ledger = year();
    let mut claim = ClaimBuilder::new(
        2026,
        Attribution::designated("QUOTA-BROKER", ["XYZ"]),
        basis(),
        DriveEfficiency::BatteryElectric,
    )
    .unwrap();
    let err = claim
        .point(&eligible_point(), "Musterstraße 1", &ledger)
        .unwrap_err();
    let ThgError::NoAgreement { operator, .. } = &err else {
        panic!("expected NoAgreement, got {err}");
    };
    assert_eq!(operator, "AB7");
}

#[test]
fn energy_no_meter_signed_cannot_enter_a_notification() {
    let mut ledger = CdrLedger::new();
    let (session, _) = session(0, "s-unsigned", "100.000", "129.500");
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(
            PartyId::new("DE", "ABC").unwrap(),
            "cdr-unsigned".parse().unwrap(),
        )
        .rated_with(&tariff())
        .build()
        .unwrap();
    assert!(ledger.accept(cdr).is_stored());

    assert!(matches!(
        builder()
            .point(&eligible_point(), "Musterstraße 1", &ledger)
            .unwrap_err(),
        ThgError::Unmeasured { .. }
    ));
}

#[test]
fn a_claim_on_a_source_that_does_not_count_yet_is_refused() {
    let biomass = EmissionsBasis::renewable(
        RenewableSource::Biomass,
        dec("20"),
        "BAnz AT 31.10.2025 B6",
        DirectSupply::complete(),
    )
    .unwrap();
    let err = ClaimBuilder::new(
        2026,
        Attribution::own("AB7"),
        biomass.clone(),
        DriveEfficiency::BatteryElectric,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ThgError::SourceNotYetCountable { from: 2028, .. }
    ));
    assert!(
        ClaimBuilder::new(
            2028,
            Attribution::own("AB7"),
            biomass,
            DriveEfficiency::BatteryElectric,
        )
        .is_ok()
    );
}
#[test]
fn a_notification_with_nothing_in_it_is_refused_rather_than_filed() {
    let empty = CdrLedger::new();
    let mut claim = builder();
    claim
        .point(&eligible_point(), "Musterstraße 1", &empty)
        .unwrap();
    let err = claim.build().unwrap_err();
    assert!(matches!(err, ThgError::NothingToReport));
}

#[test]
fn two_runs_of_one_year_are_one_file() {
    let ledger = year();
    let build = || {
        let mut claim = builder();
        claim
            .point(&eligible_point(), "Musterstraße 1", &ledger)
            .unwrap();
        claim.build().unwrap().value
    };
    assert_eq!(build(), build());
}
