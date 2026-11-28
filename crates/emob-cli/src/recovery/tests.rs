use super::*;
use emob_service::Actor;
use std::fs;

struct Exercise {
    root: PathBuf,
    paths: Paths,
}
impl Exercise {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("emob-cold-cut-{:016x}", getrandom::u64().unwrap()));
        fixture::create(&root).expect("complete synthetic cut");
        let paths = Paths {
            copy: root.join("copy"),
            evidence: root.join("evidence"),
            keys: root.join("keys"),
            scratch: root.join("scratch"),
        };
        Self { root, paths }
    }
    fn report(&self) -> Report {
        let mut report = Report::new("unit_test_disposable_memory");
        if let Err(problem) = inspect(&self.paths, &mut report) {
            report.reject(problem);
        }
        report
    }
    fn change_manifest(&self, change: impl FnOnce(&mut Manifest)) {
        let path = self.paths.evidence.join("cut.json");
        let mut m: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        change(&mut m);
        fs::write(path, serde_json::to_vec(&m).unwrap()).unwrap();
    }
    fn snapshot(&self) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        for dir in [&self.paths.copy, &self.paths.evidence, &self.paths.keys] {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                out.insert(path.clone(), digest(&fs::read(path).unwrap()));
            }
        }
        out
    }
}
impl Drop for Exercise {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// receivables-allocation/SR-355: both independently witnessed replays include the cash ledger.
#[test]
fn sr_221_sr_222_sr_223_sr_224_two_restores_repeat_one_receipt_without_touching_the_sources() {
    let drill = Exercise::new();
    let before = drill.snapshot();
    let mut first = drill.report();
    let mut second = drill.report();
    assert_eq!(first.exit(), 0, "{:?}", first.reasons);
    assert_eq!(second.exit(), 0, "{:?}", second.reasons);
    assert_eq!(first.retry, "one_effect");
    assert!(first.business.values().all(|v| *v == "match"));
    assert_eq!(first.owed_before["billd"], 1);
    assert_eq!(first.owed_after["invoice_subscriber"], 1);
    assert_eq!(first.owed_after["billd"], 0);
    assert!(
        first
            .services
            .values()
            .all(|v| v.replay == "folded" && v.witness == "exact")
    );
    assert_ne!(first.attempt_id, second.attempt_id);
    first.attempt_id.clear();
    second.attempt_id.clear();
    let exported = serde_json::to_string(&first).unwrap();
    assert_eq!(exported, serde_json::to_string(&second).unwrap());
    assert_eq!(before, drill.snapshot());
    let keyring: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(drill.paths.keys.join("data.json")).unwrap()).unwrap();
    for secret in keyring.values() {
        assert!(!exported.contains(secret));
    }
    for private in [
        "04DEADBEEF0001",
        "04A1B2C3D4E5F6",
        "DE-MSP-C00000001-W",
        "DE*MSP",
        "http://",
        "R-2026",
        "pilot-EVSE",
    ] {
        assert!(!exported.contains(private));
    }
}

#[test]
fn sr_220_sr_226_refuse_bad_signer_chain_and_partial_tail_before_folding() {
    let drill = Exercise::new();
    let path = drill.paths.copy.join("csmsd.ndjson");
    let original = fs::read(&path).unwrap();
    for replacement in [original[..original.len() - 1].to_vec(), b"{}\n".to_vec()] {
        fs::write(&path, replacement).unwrap();
        let report = drill.report();
        assert_eq!(report.exit(), 1);
        assert!(report.services.values().all(|r| r.replay == "unchecked"));
    }
    fs::write(&path, original).unwrap();
    drill.change_manifest(|m| {
        let foreign = m.services["billd"].signer.clone();
        m.services.get_mut("csmsd").unwrap().signer = foreign;
    });
    assert!(drill.report().reasons.contains(&"wrong_signer"));
}

#[test]
fn sr_220_an_older_cut_ahead_of_witness_missing_history_and_sticky_alarm_differ() {
    use emob_service::head::{SigningKey, VerifierKey};
    use emob_service::journal::Head;
    use emob_service::witness::Witness;
    let drill = Exercise::new();
    let journal = drill.paths.copy.join("poid.ndjson");
    let head = emob_service::head::beside(&journal);
    let raw = fs::read(&journal).unwrap();
    let held = Memory::from_cold_cut(&raw).unwrap().head().unwrap();
    let signer = SigningKey::from_seed(&[7; 32], "poid");
    let note = signer.sign(&held);
    fs::write(&head, &note).unwrap();
    let witness = drill.paths.evidence.join("witness.ndjson");
    let original = fs::read(&witness).unwrap();
    let obs = Witness::read_cold_cut(&original).unwrap();
    let pins = SERVICES
        .into_iter()
        .map(|s| {
            let key = if s == "poid" {
                signer.verifier().to_string()
            } else {
                obs.iter().find(|v| v.daemon == s).unwrap().key.clone()
            };
            (s.to_owned(), key)
        })
        .collect();
    let fresh = |pos: u64, digest: &str, alarm: bool, omit: bool| {
        fs::remove_file(&witness).unwrap();
        let w = Witness::open(&witness, &pins).unwrap();
        for s in SERVICES {
            if omit && s == "poid" {
                continue;
            }
            let at = obs.iter().find(|entry| entry.daemon == s).unwrap().at;
            let note = if s == "poid" {
                signer.sign(&Head {
                    position: pos,
                    hash: digest.into(),
                })
            } else {
                fs::read_to_string(drill.paths.copy.join(format!("{s}.ndjson.head"))).unwrap()
            };
            let key = pins.get(s).unwrap();
            assert!(key.parse::<VerifierKey>().unwrap().open(&note).is_ok());
            w.observe(s, &note, Some(key), at, &Actor::Estate).unwrap();
            if s == "poid" && alarm {
                w.observe(
                    s,
                    &signer.sign(&Head::genesis()),
                    Some(key),
                    at,
                    &Actor::Estate,
                )
                .unwrap();
            }
        }
        let hash = super::digest(&fs::read(&witness).unwrap());
        drill.change_manifest(|m| {
            m.witness_sha256 = hash;
            let e = m.services.get_mut("poid").unwrap();
            e.signer = signer.verifier().to_string();
            e.head_sha256 = super::digest(&fs::read(&head).unwrap());
        });
        drill.report()
    };
    let r = fresh(held.position + 1, &held.hash, false, false);
    assert_eq!(r.exit(), 1);
    assert!(r.reasons.contains(&"regressed_head"));
    let r = fresh(0, &"0".repeat(64), false, false);
    assert_eq!(r.exit(), 2);
    assert!(r.reasons.contains(&"ahead_of_witness"));
    let r = fresh(held.position, &"a".repeat(64), false, false);
    assert_eq!(r.exit(), 1);
    assert!(r.reasons.contains(&"forked_head"));
    let r = fresh(held.position, &held.hash, true, false);
    assert_eq!(r.exit(), 1);
    assert!(r.reasons.contains(&"witness_alarm"));
    let r = fresh(held.position, &held.hash, false, true);
    assert_eq!(r.exit(), 2);
    assert!(r.reasons.contains(&"independent_head_missing"));
}

#[test]
fn sr_223_sr_224_a_wrong_data_key_and_a_contradicted_oracle_prevent_a_pass() {
    let drill = Exercise::new();
    let keys = drill.paths.keys.join("data.json");
    let original = fs::read(&keys).unwrap();
    let mut ring: BTreeMap<String, String> = serde_json::from_slice(&original).unwrap();
    let foreign = ring["billd"].clone();
    ring.insert("empd".into(), foreign);
    fs::write(&keys, serde_json::to_vec(&ring).unwrap()).unwrap();
    assert!(drill.report().reasons.contains(&"wrong_data_key"));
    let initial_manifest = fs::read(drill.paths.evidence.join("cut.json")).unwrap();
    drill.change_manifest(|m| {
        m.services.get_mut("empd").unwrap().key_sha256 = Some(digest(ring["empd"].as_bytes()))
    });
    let foreign = drill.report();
    assert_eq!(foreign.services["empd"].replay, "folded");
    assert_eq!(foreign.business["cards"], "mismatch");
    fs::write(drill.paths.evidence.join("cut.json"), initial_manifest).unwrap();
    fs::write(&keys, &original).unwrap();
    let path = drill.paths.evidence.join("outcomes.json");
    let mut e: replay::Expected = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    e.energy_kwh.push('0');
    e.invoice_total = "123456".into();
    let changed = serde_json::to_vec(&e).unwrap();
    fs::write(path, &changed).unwrap();
    let untrusted = drill.report();
    assert!(untrusted.reasons.contains(&"cut_input_mismatch"));
    assert!(
        untrusted
            .services
            .values()
            .all(|row| row.replay == "unchecked")
    );
    drill.change_manifest(|m| m.outcomes_sha256 = digest(&changed));
    let report = drill.report();
    assert_eq!(report.exit(), 1);
    assert_eq!(report.business["settled"], "mismatch");
    assert_eq!(report.business["invoice"], "mismatch");
    assert_eq!(report.retry, "unchecked");
}

#[cfg(unix)]
#[test]
fn sr_221_sr_225_missing_aliased_and_oversized_inputs_have_no_business_effect() {
    let drill = Exercise::new();
    let src = drill.paths.copy.join("billd.ndjson");
    let alternate = drill.root.join("original");
    fs::rename(&src, &alternate).unwrap();
    assert_eq!(drill.report().exit(), 2);
    std::os::unix::fs::symlink(&alternate, &src).unwrap();
    assert!(drill.report().reasons.contains(&"unsafe_input_type"));
    fs::remove_file(&src).unwrap();
    fs::hard_link(&alternate, &src).unwrap();
    assert!(drill.report().reasons.contains(&"aliased_input"));
    fs::remove_file(&src).unwrap();
    std::fs::File::create_new(&src)
        .unwrap()
        .set_len(GIB + 1)
        .unwrap();
    let report = drill.report();
    assert!(report.reasons.contains(&"resource_limit"));
    assert!(report.services.values().all(|r| r.replay == "unchecked"));
}

#[test]
fn sr_220_sr_225_missing_evidence_stays_incomplete_and_a_known_contradiction_wins() {
    let drill = Exercise::new();
    let keys = drill.paths.keys.join("data.json");
    let mut ring: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(&keys).unwrap()).unwrap();
    ring.remove("empd");
    fs::write(&keys, serde_json::to_vec(&ring).unwrap()).unwrap();
    let incomplete = drill.report();
    assert_eq!(incomplete.exit(), 2);
    assert_eq!(incomplete.services["empd"].witness, "key_missing");
    assert!(
        incomplete
            .services
            .values()
            .all(|row| row.replay == "unchecked")
    );
    fs::write(drill.paths.copy.join("poid.ndjson"), b"broken\n").unwrap();
    let failed = drill.report();
    assert_eq!(failed.exit(), 1);
    assert_eq!(failed.services["empd"].witness, "key_missing");
    assert_eq!(failed.services["poid"].witness, "broken_chain");
    drill.change_manifest(|manifest| manifest.version += 1);
    assert!(drill.report().reasons.contains(&"unsupported_version"));
}

#[test]
fn sr_222_only_loopback_can_be_up_in_the_isolated_namespace() {
    let drill = Exercise::new();
    let sysfs = drill.root.join("interfaces");
    for (iface, flags) in [("lo", "0x9"), ("tunl0", "0x80")] {
        fs::create_dir_all(sysfs.join(iface)).unwrap();
        fs::write(sysfs.join(iface).join("flags"), flags).unwrap();
    }
    let devices = "header one\nheader two\n lo: 0\n tunl0: 0\n";
    assert!(isolated_links(devices, &sysfs));
    fs::write(sysfs.join("tunl0/flags"), "0x81").unwrap();
    assert!(!isolated_links(devices, &sysfs));
    assert!(!isolated_links("header one\nheader two\n", &sysfs));
}
