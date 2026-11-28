//! Cold-cut verification for the six durable services. All replayed appends
//! target memory. Only the private launcher may start the command-line worker.
pub(crate) mod fixture;
mod replay;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use emob_service::head::VerifierKey;
use emob_service::journal::{Journal, Memory};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SERVICES: [&str; 6] = ["csmsd", "billd", "roamd", "empd", "poid", "accountingd"];
const CHECKS: [&str; 7] = [
    "settled",
    "refused",
    "invoice",
    "cards",
    "roaming",
    "publication",
    "receivables",
];
const GIB: u64 = 1024 * 1024 * 1024;
const SMALL: u64 = 1024 * 1024;
const VERSION: u32 = 2;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    bytes: u64,
    journal_sha256: String,
    head_sha256: String,
    signer: String,
    key_sha256: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    synthetic: bool,
    build_sha256: String,
    config_sha256: String,
    witness_sha256: String,
    outcomes_sha256: String,
    #[serde(with = "time::serde::rfc3339")]
    captured: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    assessed: time::OffsetDateTime,
    services: BTreeMap<String, Entry>,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Pass,
    Incomplete,
    Fail,
}

#[derive(Clone, Copy)]
struct Problem(Verdict, &'static str);
type Result<T> = std::result::Result<T, Problem>;
fn failed(code: &'static str) -> Problem {
    Problem(Verdict::Fail, code)
}
fn missing(code: &'static str) -> Problem {
    Problem(Verdict::Incomplete, code)
}

#[derive(Default, Serialize)]
struct ServiceRow {
    position: Option<u64>,
    digest: Option<String>,
    witness: &'static str,
    replay: &'static str,
}

#[derive(Serialize)]
struct Report {
    version: u32,
    verdict: Verdict,
    reasons: Vec<&'static str>,
    attempt_id: String,
    cut_sha256: Option<String>,
    build_sha256: Option<String>,
    config_sha256: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    captured: Option<time::OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    assessed: Option<time::OffsetDateTime>,
    after_cut: &'static str,
    elapsed_ms: u64,
    containment: &'static str,
    services: BTreeMap<&'static str, ServiceRow>,
    business: BTreeMap<&'static str, &'static str>,
    owed_before: BTreeMap<&'static str, usize>,
    owed_after: BTreeMap<&'static str, usize>,
    retry: &'static str,
}

impl Report {
    fn new(containment: &'static str) -> Self {
        let mut id = [0u8; 16];
        let has_id = getrandom::fill(&mut id).is_ok();
        Self {
            version: VERSION,
            verdict: if has_id {
                Verdict::Pass
            } else {
                Verdict::Incomplete
            },
            reasons: if has_id {
                vec![]
            } else {
                vec!["attempt_id_unavailable"]
            },
            attempt_id: hex::encode(id),
            cut_sha256: None,
            build_sha256: None,
            config_sha256: None,
            captured: None,
            assessed: None,
            after_cut: "unknown",
            elapsed_ms: 0,
            containment,
            services: SERVICES
                .into_iter()
                .map(|s| {
                    (
                        s,
                        ServiceRow {
                            witness: "unchecked",
                            replay: "unchecked",
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            business: CHECKS.into_iter().map(|s| (s, "unchecked")).collect(),
            owed_before: BTreeMap::new(),
            owed_after: BTreeMap::new(),
            retry: "unchecked",
        }
    }
    fn reject(&mut self, problem: Problem) {
        self.verdict = self.verdict.max(problem.0);
        if !self.reasons.contains(&problem.1) {
            self.reasons.push(problem.1);
        }
    }
    fn compare(&mut self, name: &'static str, matches: bool) {
        self.business
            .insert(name, if matches { "match" } else { "mismatch" });
        if !matches {
            self.reject(failed("business_mismatch"));
        }
    }
    fn exit(&self) -> u8 {
        match self.verdict {
            Verdict::Pass => 0,
            Verdict::Fail => 1,
            Verdict::Incomplete => 2,
        }
    }
}

pub(super) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn build_digest() -> Result<String> {
    static BUILD: OnceLock<String> = OnceLock::new();
    if let Some(build) = BUILD.get() {
        return Ok(build.clone());
    }
    let mut binary =
        std::fs::File::open(std::env::current_exe().map_err(|_| missing("build_unavailable"))?)
            .map_err(|_| missing("build_unavailable"))?;
    let mut hash = Sha256::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = binary
            .read(&mut chunk)
            .map_err(|_| missing("build_unavailable"))?;
        if count == 0 {
            break;
        }
        hash.update(&chunk[..count]);
    }
    let result = hex::encode(hash.finalize());
    Ok(BUILD.get_or_init(|| result).clone())
}

/// Only fixed filenames are read, from immutable input mounts. Recheck the
/// opened object as well as its name, and bound the read even for a growing file.
fn read(path: &Path, limit: u64, budget: &mut u64) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| missing("input_unavailable"))?;
    if !meta.is_file() {
        return Err(failed("unsafe_input_type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(failed("aliased_input"));
        }
    }
    if meta.len() > limit.min(*budget) {
        return Err(missing("resource_limit"));
    }
    let file = std::fs::File::open(path).map_err(|_| missing("input_unavailable"))?;
    let opened = file.metadata().map_err(|_| missing("input_unavailable"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (opened.dev(), opened.ino(), opened.nlink()) != (meta.dev(), meta.ino(), 1) {
            return Err(failed("input_changed"));
        }
    }
    if !opened.is_file() {
        return Err(failed("unsafe_input_type"));
    }
    let mut data = Vec::new();
    file.take(meta.len().saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|_| missing("input_unavailable"))?;
    let size = u64::try_from(data.len()).map_err(|_| missing("resource_limit"))?;
    if size != meta.len() {
        return Err(failed("input_changed"));
    }
    *budget -= size;
    Ok(data)
}

struct Paths {
    copy: PathBuf,
    evidence: PathBuf,
    keys: PathBuf,
    scratch: PathBuf,
}
struct Recovery {
    journals: BTreeMap<&'static str, Memory>,
    keys: BTreeMap<String, String>,
}
impl Recovery {
    fn take(&mut self, service: &'static str) -> Result<Memory> {
        self.journals
            .remove(service)
            .ok_or_else(|| missing("journal_missing"))
    }
}

fn inspect(paths: &Paths, report: &mut Report) -> Result<()> {
    let mut budget = 2 * GIB;
    let raw = read(&paths.evidence.join("cut.json"), SMALL, &mut budget)?;
    report.cut_sha256 = Some(digest(&raw));
    let value: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| failed("invalid_manifest"))?;
    if value["version"].as_u64() != Some(u64::from(VERSION)) {
        return Err(missing("unsupported_version"));
    }
    let manifest: Manifest =
        serde_json::from_value(value).map_err(|_| failed("invalid_manifest"))?;
    if !manifest.synthetic {
        return Err(failed("synthetic_scope_required"));
    }
    if manifest
        .services
        .keys()
        .any(|name| !SERVICES.contains(&name.as_str()))
    {
        return Err(failed("unexpected_service"));
    }
    let build = build_digest()?;
    if manifest.build_sha256 != build {
        return Err(missing("unsupported_build"));
    }
    report.build_sha256 = Some(build);
    report.captured = Some(manifest.captured);
    report.assessed = Some(manifest.assessed);

    let config = read(&paths.copy.join("estate.toml"), SMALL, &mut budget)?;
    let outcomes = read(&paths.evidence.join("outcomes.json"), SMALL, &mut budget)?;
    let witness = read(
        &paths.evidence.join("witness.ndjson"),
        8 * SMALL,
        &mut budget,
    )?;
    if digest(&config) != manifest.config_sha256
        || digest(&outcomes) != manifest.outcomes_sha256
        || digest(&witness) != manifest.witness_sha256
    {
        return Err(failed("cut_input_mismatch"));
    }
    report.config_sha256 = Some(manifest.config_sha256.clone());
    let config = std::str::from_utf8(&config).map_err(|_| failed("invalid_config"))?;
    let outcomes = serde_json::from_slice(&outcomes).map_err(|_| failed("invalid_expectations"))?;
    let seen = emob_service::witness::Witness::read_cold_cut(&witness)
        .map_err(|_| failed("damaged_witness"))?;
    let key_bytes = read(&paths.keys.join("data.json"), SMALL, &mut budget)?;
    let keys = serde_json::from_slice(&key_bytes).map_err(|_| failed("invalid_keyring"))?;
    let mut state = Recovery {
        journals: BTreeMap::new(),
        keys,
    };
    for name in SERVICES {
        let checked = (|| {
            let entry = manifest
                .services
                .get(name)
                .ok_or_else(|| missing("service_absent"))?;
            let bytes = read(&paths.copy.join(format!("{name}.ndjson")), GIB, &mut budget)?;
            let note = read(
                &paths.copy.join(format!("{name}.ndjson.head")),
                SMALL,
                &mut budget,
            )?;
            let journal = Memory::from_cold_cut(&bytes).map_err(|_| failed("broken_chain"))?;
            let key: VerifierKey = entry.signer.parse().map_err(|_| failed("invalid_signer"))?;
            if key.name() != format!("emob/{name}") {
                return Err(failed("wrong_signer"));
            }
            let signed = key
                .open(std::str::from_utf8(&note).map_err(|_| failed("invalid_head"))?)
                .map_err(|_| failed("invalid_signature"))?;
            let held = journal.head().map_err(|_| failed("broken_chain"))?;
            if held.position == 0 {
                return Err(missing("empty_service"));
            }
            if held != signed {
                return Err(failed("local_head_mismatch"));
            }
            let row = report.services.get_mut(name).expect("known service");
            row.position = Some(held.position);
            row.digest = Some(held.hash.clone());
            let obs = seen
                .iter()
                .find(|obs| obs.daemon == name)
                .ok_or_else(|| missing("independent_head_missing"))?;
            if obs.alarm.is_some() {
                return Err(failed("witness_alarm"));
            }
            if key.to_string() != obs.key {
                return Err(failed("witness_rekey"));
            }
            if obs.at != manifest.captured {
                return Err(missing("observation_outside_cut"));
            }
            if held.position < obs.position {
                return Err(failed("regressed_head"));
            }
            if held.position > obs.position {
                return Err(missing("ahead_of_witness"));
            }
            if held.hash != obs.hash {
                return Err(failed("forked_head"));
            }
            if entry.bytes != bytes.len() as u64
                || digest(&bytes) != entry.journal_sha256
                || digest(&note) != entry.head_sha256
            {
                return Err(failed("file_digest_mismatch"));
            }
            if name != "poid" {
                let secret = state.keys.get(name).ok_or_else(|| missing("key_missing"))?;
                if Some(digest(secret.as_bytes())) != entry.key_sha256 {
                    return Err(failed("wrong_data_key"));
                }
            }
            Ok(journal)
        })();
        match checked {
            Ok(journal) => {
                report
                    .services
                    .get_mut(name)
                    .expect("known service")
                    .witness = "exact";
                state.journals.insert(name, journal);
            }
            Err(problem) => {
                report
                    .services
                    .get_mut(name)
                    .expect("known service")
                    .witness = problem.1;
                report.reject(problem);
            }
        }
    }
    if report.verdict == Verdict::Pass {
        replay::run(state, config, manifest.assessed, &outcomes, report)?;
    }
    Ok(())
}

/// Docker's offline namespace can still list dormant kernel tunnel devices.
/// Dropping all capabilities prevents the worker from bringing those links up.
fn isolated_links(devices: &str, network_class: &Path) -> bool {
    let mut loopback = false;
    for line in devices.lines().skip(2) {
        let Some((name, _)) = line.split_once(':') else {
            return false;
        };
        let name = name.trim();
        let Ok(flags) = std::fs::read_to_string(network_class.join(name).join("flags")) else {
            return false;
        };
        let Ok(flags) = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16) else {
            return false;
        };
        if name == "lo" {
            // IFF_UP and IFF_LOOPBACK, from Linux netdevice(7).
            loopback = flags & 0x9 == 0x9;
        } else if flags & 1 != 0 {
            return false;
        }
    }
    loopback
}

fn contained() -> bool {
    // The immutable image and launcher form the worker's trust boundary.
    // Confirm the kernel's view before reading the restricted custody mount.
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let mounted = |path: &str, flag: &str| {
        text.lines().any(|line| {
            let mut fields = line.split_ascii_whitespace().skip(4);
            fields.next() == Some(path)
                && fields
                    .next()
                    .is_some_and(|s| s.split(',').any(|v| v == flag))
        })
    };
    let Ok(interfaces) = std::fs::read_to_string("/proc/net/dev") else {
        return false;
    };
    isolated_links(&interfaces, Path::new("/sys/class/net"))
        && ["/", "/cut", "/evidence", "/keys"]
            .into_iter()
            .all(|p| mounted(p, "ro"))
        && mounted("/scratch", "rw")
}

pub(crate) fn main(args: &[String], stdout: &mut impl Write) -> u8 {
    if args != ["--isolated-worker"] {
        // Ship the same host-side launcher with installed CLIs as with a
        // checkout. Python isolated mode excludes user packages and PYTHONPATH.
        return std::process::Command::new("python3")
            .args([
                "-I",
                "-c",
                include_str!("../../../deploy/recovery-drill.py"),
            ])
            .args(args)
            .status()
            .map_or_else(
                |_| {
                    let _ = writeln!(stdout, "Recovery needs Python 3 and a local Docker engine.");
                    2
                },
                |status| {
                    status
                        .code()
                        .and_then(|n| u8::try_from(n).ok())
                        .unwrap_or(2)
                },
            );
    }
    let start = Instant::now();
    let mut report = Report::new("unverified");
    if contained() {
        report.containment = "isolated";
        // This limit survives loss of the external launcher. A killed worker
        // cannot create a final pass, and the host reserved an incomplete report.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(900));
            std::process::exit(2);
        });
        let paths = Paths {
            copy: "/cut".into(),
            evidence: "/evidence".into(),
            keys: "/keys".into(),
            scratch: "/scratch".into(),
        };
        if !std::fs::read_dir(&paths.scratch).is_ok_and(|mut d| d.next().is_none()) {
            report.reject(failed("scratch_not_fresh"));
        } else if let Err(problem) = inspect(&paths, &mut report) {
            report.reject(problem);
        }
    } else {
        report.reject(missing("isolation_unavailable"));
    }
    report.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let code = report.exit();
    if serde_json::to_writer(stdout, &report).is_err() {
        return 2;
    }
    code
}
