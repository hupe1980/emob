//! SR-068: the driver's check says what the billing check says, on records
//! somebody else wrote.
//!
//! `tests/corpus/records.json` is every OCMF record in the S.A.F.E.
//! Transparenzsoftware's test data (Apache-2.0, see `corpus/NOTICE`), each with
//! the key it ships with and **OpenSSL's verdict** on it — vendored from `ocmf`,
//! which measures itself against the same file. A check that signed its own
//! fixtures would only prove that it agrees with itself.
//!
//! Three verdicts are compared for every record that carries a key:
//!
//! 1. OpenSSL's (where the corpus states one);
//! 2. `emob-eichrecht`'s — the record assembled into [`Evidence`] against a
//!    registry holding that key for the component the record names, which is
//!    the path a session takes before it may bill;
//! 3. [`emob_verify::check`]'s — what the receipt page and `emob verify` say.

use emob_eichrecht::registry::{ComponentRef, KeyRegistry, RegisteredKey};
use emob_eichrecht::{EichrechtError, Evidence, EvidenceProblem, PublicKey, Record};
use emob_verify::{Verdict, check};

struct Entry {
    source: String,
    record: String,
    key: Option<String>,
    openssl: Option<bool>,
}

fn corpus() -> Vec<Entry> {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("corpus/records.json")).expect("the corpus is JSON");
    json["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| Entry {
            source: e["source"].as_str().unwrap_or_default().to_owned(),
            record: e["record"].as_str().expect("a record").to_owned(),
            key: e["key"].as_str().map(str::to_owned),
            openssl: e["openssl_verified"].as_bool(),
        })
        .collect()
}

/// The component a record names, most specific first — what
/// `KeyRegistry::key_for_record` looks up.
fn component_of(record: &Record<'_>) -> Option<ComponentRef> {
    let payload = record.payload();
    match (payload.gateway_serial(), payload.meter_serial()) {
        (Some(gateway), Some(meter)) => Some(ComponentRef::GatewayAndMeter {
            gateway: gateway.to_owned(),
            meter: meter.to_owned(),
        }),
        (None, Some(meter)) => Some(ComponentRef::Meter {
            serial: meter.to_owned(),
        }),
        (Some(gateway), None) => Some(ComponentRef::Gateway {
            serial: gateway.to_owned(),
        }),
        (None, None) => match (payload.charge_point_id_type(), payload.charge_point_id()) {
            (Some(id_type), Some(id)) => Some(ComponentRef::ChargePoint {
                id_type: id_type.as_str().to_owned(),
                id: id.to_owned(),
            }),
            _ => None,
        },
    }
}

/// `emob-eichrecht`'s verdict on one record, through `Evidence::assemble`.
fn eichrecht(record: &Record<'_>, key: PublicKey) -> Option<Verdict> {
    let component = component_of(record)?;
    let mut registry = KeyRegistry::new();
    registry
        .insert(component, RegisteredKey::unbounded(key, "S.A.F.E. corpus"))
        .ok()?;
    let evidence = Evidence::assemble(
        std::slice::from_ref(record),
        &registry,
        time::OffsetDateTime::UNIX_EPOCH,
    );
    let signature = evidence.problems.iter().find_map(|problem| match problem {
        EvidenceProblem::Signature { error, .. } => Some(error),
        _ => None,
    });
    Some(match signature {
        None => Verdict::Verified,
        Some(EichrechtError::Signature(ocmf::VerifyError::Unsupported { .. })) => {
            Verdict::Unavailable
        }
        Some(EichrechtError::Signature(ocmf::VerifyError::AlgorithmKeyMismatch { .. })) => {
            Verdict::KeyMismatch
        }
        Some(EichrechtError::Signature(_)) => Verdict::NotVerified,
        // A lookup or parse failure is not a verdict about the signature.
        Some(_) => return None,
    })
}

#[test]
fn sr_068_the_drivers_check_agrees_with_openssl_and_with_the_billing_check() {
    let corpus = corpus();
    assert_eq!(corpus.len(), 256, "the whole S.A.F.E. corpus");

    let (mut with_key, mut against_openssl, mut against_eichrecht, mut unavailable) = (0, 0, 0, 0);
    let mut disagreements = Vec::new();
    for entry in &corpus {
        let Some(key_text) = &entry.key else {
            // No key shipped: the driver's check has nothing to check against,
            // and says the key is unreadable rather than guessing one.
            assert_eq!(
                check(&entry.record, "").verdict,
                Verdict::UnreadableKey,
                "{}",
                entry.source
            );
            continue;
        };
        with_key += 1;
        let ours = check(&entry.record, key_text).verdict;
        if ours == Verdict::Unavailable {
            unavailable += 1;
        }

        if let Some(expected) = entry.openssl {
            against_openssl += 1;
            let agrees = match ours {
                Verdict::Verified => expected,
                // This build cannot tell; that is reported, not guessed.
                Verdict::Unavailable => true,
                _ => !expected,
            };
            if !agrees {
                disagreements.push(format!(
                    "{}: OpenSSL said {expected}, the check said {ours:?}",
                    entry.source
                ));
            }
        }

        let record = Record::parse(&entry.record).expect("every corpus record parses");
        let Ok(key) = PublicKey::from_text(key_text, record.signature().curve()) else {
            // OpenSSL read every key it ruled on; a key it never saw that we
            // cannot read is reported as unreadable, never as a verdict.
            assert!(
                entry.openssl.is_none(),
                "{}: OpenSSL read this key",
                entry.source
            );
            assert_eq!(ours, Verdict::UnreadableKey, "{}", entry.source);
            continue;
        };
        if let Some(theirs) = eichrecht(&record, key) {
            against_eichrecht += 1;
            if theirs != ours {
                disagreements.push(format!(
                    "{}: emob-eichrecht said {theirs:?}, the check said {ours:?}",
                    entry.source
                ));
            }
        }
    }

    assert!(
        disagreements.is_empty(),
        "{} disagreement(s):\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
    assert_eq!(with_key, 254, "records that ship a key");
    assert_eq!(against_openssl, 251, "every record OpenSSL ruled on");
    assert!(
        against_eichrecht >= 250,
        "{against_eichrecht} records went through Evidence::assemble"
    );
    assert!(
        unavailable == 4,
        "brainpool and secp192k1 are reported as not checkable here, not as failures"
    );
}

#[test]
fn sr_068_one_changed_digit_is_caught_and_the_reading_is_the_signed_one() {
    let entry = corpus()
        .into_iter()
        .find(|e| e.openssl == Some(true) && e.record.contains("\"RV\":0.2596"))
        .expect("the KEBA record from ocmf's own example");
    let key = entry.key.expect("it ships a key");

    let held = check(&entry.record, &key);
    assert_eq!(held.verdict, Verdict::Verified);
    assert_eq!(held.readings[0].value, "0.2596");
    assert_eq!(held.readings[0].marker, 'B');
    assert_eq!(held.readings[0].unit, "kWh");
    assert!(held.summary().starts_with("B\t0.2596\tkWh\t"));

    let edited = entry.record.replace("0.2596", "0.2597");
    let forged = check(&edited, &key);
    assert_eq!(forged.verdict, Verdict::NotVerified);
    assert_eq!(forged.verdict.code(), 1);
    assert_eq!(
        forged.readings[0].value, "0.2597",
        "the device shows what it checked"
    );
}

#[test]
fn what_is_not_a_record_or_not_a_key_is_named_as_such() {
    assert_eq!(check("not ocmf", "04ab").verdict, Verdict::UnreadableRecord);
    let entry = corpus()
        .into_iter()
        .find(|e| e.openssl == Some(true))
        .expect("an authentic record");
    assert_eq!(check(&entry.record, "zz").verdict, Verdict::UnreadableKey);
    assert_eq!(Verdict::Unavailable.code(), 2);
    assert!(!Verdict::Unavailable.holds());
}
