//! The seam, end to end, on a message this workspace did not write.
//!
//! The Open Charge Alliance publishes an example `StopTransaction.req` in its
//! *Signed Meter Values in OCPP* application note `[OCA SMV §5.2]`: a DZG
//! GSH01.1K2L, an OCMF data set base64'd into a `SignedMeterValueType`, that
//! object serialised into a `SampledValue.value` string, and the meter's public
//! key beside it.
//!
//! It is the whole seam in one artefact, and it carries the argument for the
//! seam rule inside itself: the message's own `meterStop` is **108814** — the
//! meter's *lifetime* register, in watt-hours — while the transaction's signed
//! difference is **0.636 kWh**. A CSMS that billed the protocol's number would
//! bill a figure nothing signed, from a register that is not the session's, and
//! be out by a factor of a hundred and seventy.

use emob_cdr::{CdrBuilder, EvidenceRef, validate};
use emob_core::{Currency, Direction, PartyId};
use emob_eichrecht::ocmf::{KeyType, PublicKey};
use emob_eichrecht::registry::{ComponentRef, KeyRegistry, RegisteredKey};
use emob_eichrecht::{Evidence, transparency};
use emob_ocpp::fixtures::{OCA_1_6_SAMPLED_VALUE, OCA_KEY_HEX};
use emob_ocpp::{SignedMeterValue, SignedReading, Transaction, TransactionEvent};
use emob_session::{Authorization, EndReason};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::datetime;

/// What the OCPP message says the meter's lifetime register read at the stop —
/// the number a CSMS billing the protocol would have used.
const OCPP_METER_STOP_WH: i64 = 108_814;

fn started() -> time::OffsetDateTime {
    datetime!(2023-05-19 15:52:39 +2)
}

fn ended() -> time::OffsetDateTime {
    datetime!(2023-05-19 15:53:58 +2)
}

/// The registry a provisioning run would have written — the key out of band,
/// never the one the station sent beside the record.
fn registry() -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    registry
        .insert(
            ComponentRef::Meter {
                serial: "1DZG0028225179".into(),
            },
            RegisteredKey::unbounded(
                PublicKey::from_hex(KeyType::Secp256k1, OCA_KEY_HEX).unwrap(),
                "type approval — DZG GSH01.1K2L",
            ),
        )
        .unwrap();
    registry
}

/// The transaction exactly as the whitepaper's message describes it: the signed
/// data arrives on the closing event, in one data set holding both readings.
fn oca_transaction() -> Transaction {
    Transaction::new(
        "t-96".parse().unwrap(),
        "DE*SIM*E00001".parse().unwrap(),
        Authorization::ad_hoc(),
    )
    .with(TransactionEvent::started(started(), vec![]))
    .with(TransactionEvent::ended(
        ended(),
        vec![SignedReading::new(
            SignedMeterValue::from_signed_data(OCA_1_6_SAMPLED_VALUE).unwrap(),
            Some("Transaction.End".to_owned()),
        )],
        EndReason::Local,
    ))
}

#[test]
fn a_real_ocpp_message_reaches_a_settleable_priced_record() {
    let assembled = oca_transaction().assemble(Direction::Import).unwrap();

    // 1. The signed records came out of the transport intact, and verify
    //    against the registry — not against the key the station sent.
    let evidence = Evidence::assemble(&assembled.records, &registry(), started());
    assert!(
        evidence.problems.iter().all(|p| {
            matches!(p, emob_eichrecht::EvidenceProblem::Chain(f)
                if matches!(f, emob_eichrecht::ChainFinding::ClockNotBillable { .. }))
        }),
        "{:?}",
        evidence.reasons().collect::<Vec<_>>()
    );
    assert_eq!(
        evidence.billable_energy().unwrap().to_string(),
        "0.636 kWh",
        "the billable quantity is the signed transaction difference"
    );

    // 2. …and it is not the number the protocol carried. `meterStop` is the
    //    lifetime register in watt-hours; billing it would be out by a factor
    //    of a hundred and seventy, off a register that is not the session's.
    let signed_wh = evidence.billable_energy().unwrap().wh();
    assert_ne!(signed_wh, Decimal::from(OCPP_METER_STOP_WH));
    assert!(signed_wh < Decimal::from(OCPP_METER_STOP_WH));

    // 3. The clock is informative `[OCMF Tab. 19]`, so the energy bills and the
    //    duration does not — the distinction survives the seam.
    assert!(!evidence.is_billable_for_time());

    // 4. A CDR built from it prices per kWh and settles.
    let tariff = Tariff::simple(
        "ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![
            PriceComponent::new(Dimension::Energy, Decimal::from_str("0.49").unwrap())
                .with_vat(Decimal::from(19)),
        ],
    );
    let cdr = CdrBuilder::from_session(&assembled.session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&tariff)
        .build()
        .unwrap();

    assert_eq!(cdr.total_energy.to_string(), "0.636 kWh");
    assert!(cdr.conserves());
    assert_eq!(cdr.total_cost().unwrap().to_string(), "0.31 EUR");
    assert!(validate(&cdr).is_settleable());

    // 5. And the driver gets a file their own verifier reads, holding the
    //    record the station signed — byte for byte, through the whole seam.
    let xml = transparency::to_xml(&evidence);
    let back = transparency::from_xml(&xml).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(
        back[0].record.signed_bytes(),
        assembled.records[0].signed_bytes()
    );
}

#[test]
fn a_per_minute_tariff_on_this_session_is_refused_by_name() {
    // The clock status is `I`. The energy is perfectly good and the duration is
    // not, so an occupancy fee has nothing it may be charged against — and the
    // refusal names the fix rather than blocking the session.
    let assembled = oca_transaction().assemble(Direction::Import).unwrap();
    let evidence = Evidence::assemble(&assembled.records, &registry(), started());

    let occupancy = Tariff::simple(
        "ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![
            PriceComponent::new(Dimension::Energy, Decimal::from_str("0.49").unwrap()),
            PriceComponent::new(Dimension::ParkingTime, Decimal::from_str("6.00").unwrap()),
        ],
    );
    let error = CdrBuilder::from_session(&assembled.session, Direction::Import)
        .unwrap()
        .key(PartyId::new("DE", "ABC").unwrap(), "cdr-1".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&occupancy)
        .build()
        .unwrap_err();

    assert!(
        error.to_string().contains("price this session per kWh"),
        "{error}"
    );
}

#[test]
fn a_tampered_record_from_the_same_transport_reaches_no_price() {
    // The seam does not verify — it carries bytes. The chain is what refuses,
    // and it still refuses when the bytes arrived over OCPP rather than out of
    // a file.
    let tampered_value = OCA_1_6_SAMPLED_VALUE.replace(
        // The base64 of the OCMF record, one character changed inside the
        // payload rather than the signature.
        "T0NNRnx7", "T0NNRnx8",
    );
    let transaction = Transaction::new(
        "t-96".parse().unwrap(),
        "DE*SIM*E00001".parse().unwrap(),
        Authorization::ad_hoc(),
    )
    .with(TransactionEvent::started(started(), vec![]))
    .with(TransactionEvent::ended(
        ended(),
        vec![SignedReading::new(
            SignedMeterValue::from_signed_data(&tampered_value).unwrap(),
            Some("Transaction.End".to_owned()),
        )],
        EndReason::Local,
    ));

    // Either the record no longer parses, or it parses and does not verify.
    // Both are refusals; neither is a price.
    match transaction.assemble(Direction::Import) {
        Err(_) => {}
        Ok(assembled) => {
            let evidence = Evidence::assemble(&assembled.records, &registry(), started());
            assert_eq!(evidence.billable_energy(), None);
        }
    }
}
