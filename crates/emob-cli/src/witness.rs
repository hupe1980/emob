//! `emob witness <url>...` — a witness on the operator's own machine.
//!
//! Reads each daemon's signed journal head (`GET /v1/<name>/head`, with
//! `EMOB_OPERATOR_TOKEN`), checks it against the key pinned at first sight,
//! and records it in a chained file of its own — so a journal cut short, or
//! restored from an older backup, is caught by a copy no daemon's volume holds
//! ([`emob_service::witness`]).
//!
//! Exit status: `0` every head witnessed · `1` an alarm, now or recorded
//! earlier · `2` a daemon could not be read · `64` usage.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use emob_service::witness::{Verdict, Witness};

const USAGE: &str = "usage: emob witness [--file PATH] <url>...\n\n\
    Reads each daemon's signed journal head with EMOB_OPERATOR_TOKEN and records\n\
    it in PATH (default ./witness.ndjson). Exit 0 = all witnessed, 1 = a head went\n\
    backwards, forked or changed key (now or earlier), 2 = a daemon could not be read.\n";

pub fn main(args: &[String]) -> ExitCode {
    let mut file = PathBuf::from("witness.ndjson");
    let mut urls = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--file" => match args.next() {
                Some(path) => file = PathBuf::from(path),
                None => return usage(),
            },
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            flag if flag.starts_with('-') => return usage(),
            url => urls.push(url.to_owned()),
        }
    }
    if urls.is_empty() {
        return usage();
    }
    let Some(token) = std::env::var("EMOB_OPERATOR_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
    else {
        eprintln!("emob witness: EMOB_OPERATOR_TOKEN is not set — a head is an operator's read");
        return ExitCode::from(64);
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("emob witness: {error}");
            return ExitCode::from(2);
        }
    };
    ExitCode::from(runtime.block_on(run(&file, &urls, token)))
}

fn usage() -> ExitCode {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
    ExitCode::from(64)
}

async fn run(file: &std::path::Path, urls: &[String], token: String) -> u8 {
    let witness = match Witness::open(file, &std::collections::BTreeMap::new()) {
        Ok(witness) => witness,
        Err(error) => {
            eprintln!("emob witness: {}: {error}", file.display());
            return 2;
        }
    };
    let reader = match deskd::fetch::Reader::new(emob_service::config::Secret::new(token)) {
        Ok(reader) => reader,
        Err(error) => {
            eprintln!("emob witness: {error}");
            return 2;
        }
    };
    let by = emob_service::Actor::operator("cli")
        .unwrap_or_else(|_| unreachable!("`cli` is in the actor alphabet"));
    let (mut alarm, mut unread) = (false, false);
    for url in urls {
        let seen = async {
            let name = reader.name(url).await.map_err(|why| format!("{why:?}"))?;
            let (note, key) = reader
                .head(url, &name)
                .await
                .map_err(|why| format!("{why:?}"))?;
            let verdict = witness
                .observe(
                    &name,
                    &note,
                    key.as_deref(),
                    time::OffsetDateTime::now_utc(),
                    &by,
                )
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((name, verdict))
        }
        .await;
        match seen {
            Ok((name, verdict)) => {
                alarm |= verdict.is_alarm();
                let held = witness.view().into_iter().find(|seen| seen.daemon == name);
                let position = held.as_ref().map_or(0, |seen| seen.last);
                let line = match &verdict {
                    Verdict::First => format!(
                        "✓ {name:<6} {position:>6}  first sight — key {} pinned",
                        held.as_ref().map_or("", |seen| short(&seen.key))
                    ),
                    Verdict::Advanced { from } => {
                        format!("✓ {name:<6} {position:>6}  (was {from})")
                    }
                    Verdict::Unchanged => format!("✓ {name:<6} {position:>6}  unchanged"),
                    Verdict::Regressed { witnessed } => format!(
                        "✕ {name:<6} {position:>6}  went backwards: witnessed {witnessed} — records were removed or an older copy restored"
                    ),
                    Verdict::Forked => format!(
                        "✕ {name:<6} {position:>6}  forked: another record than the one witnessed here"
                    ),
                    Verdict::Rekeyed { pinned } => format!(
                        "✕ {name:<6} {position:>6}  signed by another key than {}",
                        short(pinned)
                    ),
                };
                println!("{line}");
                if let Some(earlier) = held
                    .and_then(|seen| seen.alarm)
                    .filter(|_| !verdict.is_alarm())
                {
                    alarm = true;
                    println!("  ↳ an alarm is on record for {name}: {earlier:?}");
                }
            }
            Err(why) => {
                unread = true;
                println!("? {url}  not witnessed: {why}");
            }
        }
    }
    if alarm {
        1
    } else if unread {
        2
    } else {
        0
    }
}

/// `emob/csmsd+1a2b3c4d` of a verifier key — enough to tell two apart.
fn short(key: &str) -> &str {
    key.rsplit_once('+').map_or(key, |(head, _)| head)
}
