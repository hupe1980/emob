//! `emob verify <file>` over a transparency file `emob-eichrecht` wrote — the
//! one a receipt page hands out — and over the same file with one digit
//! changed (SR-068).

use std::path::PathBuf;
use std::process::Command;

use emob_eichrecht::registry::{ComponentRef, KeyRegistry, RegisteredKey};
use emob_eichrecht::{Evidence, PublicKey};
use p256::ecdsa::signature::hazmat::PrehashSigner as _;

/// A record signed here, by a real key, so the file is one a key produced.
fn signed(pagination: &str, marker: &str, value: &str) -> (String, PublicKey) {
    use p256::ecdsa::SigningKey;
    use sha2::{Digest as _, Sha256};

    let signing = SigningKey::from_bytes(&[7u8; 32].into()).expect("a valid scalar");
    let payload = format!(
        r#"{{"FV":"1.0","PG":"{pagination}","MS":"M-1","IS":true,"IL":"VERIFIED","IF":[],"IT":"NONE","RD":[{{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"{marker}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}"#
    );
    let digest: [u8; 32] = Sha256::digest(payload.as_bytes()).into();
    let signature: p256::ecdsa::DerSignature = signing.sign_prehash(&digest).expect("a digest");
    let text = format!(
        "OCMF|{payload}|{{\"SA\":\"ECDSA-secp256r1-SHA256\",\"SD\":\"{}\"}}",
        hex::encode(signature.as_bytes())
    );
    let key = PublicKey::from_sec1(
        ocmf::Curve::Secp256r1,
        p256::ecdsa::VerifyingKey::from(&signing)
            .to_sec1_point(false)
            .as_bytes(),
    )
    .expect("a valid key");
    (text, key)
}

fn transparency_file() -> String {
    let (begin, key) = signed("T1", "B", "10.0");
    let (end, _) = signed("T2", "E", "25.0");
    let mut registry = KeyRegistry::new();
    registry
        .insert(
            ComponentRef::Meter {
                serial: "M-1".to_owned(),
            },
            RegisteredKey::unbounded(key, "test"),
        )
        .expect("one key");
    let records = [
        ocmf::Record::parse(&begin).unwrap(),
        ocmf::Record::parse(&end).unwrap(),
    ];
    let evidence = Evidence::assemble(&records, &registry, time::OffsetDateTime::UNIX_EPOCH);
    assert!(evidence.is_billable(), "{:?}", evidence.problems);
    emob_eichrecht::transparency::to_xml(&evidence).expect("a container")
}

fn write(name: &str, text: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("emob-cli-{}-{name}", std::process::id()));
    std::fs::write(&path, text).unwrap();
    path
}

fn emob(args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_emob"))
        .args(args)
        .output()
        .expect("the binary runs");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr),
    )
}

#[test]
fn sr_068_a_file_the_receipt_hands_out_verifies_offline() {
    let path = write("good.xml", &transparency_file());
    let (code, out) = emob(&["verify", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("2 verified, 0 not verified"), "{out}");
    assert!(
        out.contains("B 10.0 kWh") && out.contains("E 25.0 kWh"),
        "{out}"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn sr_068_one_changed_digit_names_the_value_and_fails() {
    let forged = transparency_file().replace("\"RV\":25.0", "\"RV\":29.0");
    let path = write("forged.xml", &forged);
    let (code, out) = emob(&["verify", path.to_str().unwrap()]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("1 verified, 1 not verified"), "{out}");
    let failed = out
        .lines()
        .position(|line| line.contains("E 29.0 kWh"))
        .expect("the changed value is named");
    assert!(out.lines().nth(failed).unwrap().contains('✗'), "{out}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn what_is_not_a_transparency_file_is_named_as_such() {
    let path = write("not.xml", "<html/>");
    assert_eq!(emob(&["verify", path.to_str().unwrap()]).0, 66);
    assert_eq!(emob(&["verify", "/nonexistent/emob.xml"]).0, 66);
    assert_eq!(emob(&[]).0, 64);
    assert_eq!(emob(&["--help"]).0, 0);
    let _ = std::fs::remove_file(path);
}
