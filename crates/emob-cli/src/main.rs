//! `emob` — the command line.
//!
//! `emob check <estate.toml>` refuses an estate before a daemon reads it — see
//! [`check`].
//!
//! `emob subscribers test <name>` sends a subscriber one signed sample of
//! each type it takes — see [`subscribers`].
//!
//! `emob demo` runs the whole estate on loopback — see [`demo`].
//!
//! `emob witness <url>...` reads each daemon's signed journal head and alarms
//! on one that went backwards — see [`witness`].
//!
//! `emob verify <transparency-file>` reads the S.A.F.E.
//! container a receipt page hands out (`[MessEG §33]`: the driver must be able
//! to check the value) and checks every value against the key it carries,
//! offline, with [`emob_verify::check`] — the same function the receipt page
//! runs in the browser, so the two cannot say different things about one file.
//!
//! Exit status: `0` every value verified · `1` at least one did not ·
//! `2` none failed but one could not be checked · `64` usage · `66` the file is
//! unreadable or not a transparency file.

use std::io::Write as _;
use std::process::ExitCode;

use emob_verify::Verdict;

mod check;
mod demo;
mod subscribers;
mod witness;

const USAGE: &str = "usage: emob check [--secrets] [--schema] <estate.toml>\n       emob subscribers test <name> [--estate PATH]\n       emob verify <transparency-file>\n       emob demo [--check] [--seed N] [--clock RFC3339] [--speed N|max] [--listen IP] [--tls DIR] [--links FILE]\n       emob witness [--file PATH] <url>...\n\n\
    check   Checks an estate before a daemon reads it: every section, every\n\
            daemon's own checks, and with --secrets every *_env is set.\n\
            Errors point at the file, the line and the key.\n\
    subscribers test\n\
            Sends one signed sample of every event type a [[subscribers]]\n\
            entry takes, marked emobtest, and prints each answer.\n\
    verify  Checks every signed meter value in a S.A.F.E. transparency file against\n\
            the key it carries, offline. Exit 0 = all verified, 1 = one did not\n\
            verify, 2 = one could not be checked here.\n\
    demo    Runs the whole estate on loopback: three simulated stations, a roaming\n\
            partner, a seeded morning. --check plays it at full speed and asserts\n\
            what it prints.\n\
    witness Reads each daemon's signed journal head (EMOB_OPERATOR_TOKEN) and\n\
            alarms when one went backwards, forked or changed key.\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let code = match args.as_slice() {
        [command, path] if command == "verify" => verify(path, &mut out, &mut err),
        [command, rest @ ..] if command == "witness" => {
            drop((out, err));
            return witness::main(rest);
        }
        [command, rest @ ..] if command == "check" => check::main(rest, &mut out, &mut err),
        [command, rest @ ..] if command == "subscribers" => {
            subscribers::main(rest, &mut out, &mut err)
        }
        [command, rest @ ..] if command == "demo" => {
            drop((out, err));
            return demo::main(rest);
        }
        [flag] if flag == "-h" || flag == "--help" => {
            let _ = out.write_all(USAGE.as_bytes());
            0
        }
        _ => {
            let _ = err.write_all(USAGE.as_bytes());
            64
        }
    };
    ExitCode::from(code)
}

/// One value's outcome, before it is printed.
enum Outcome {
    Checked(emob_verify::Check),
    NotOcmf(String),
    NoKey,
}

fn verify(path: &str, out: &mut impl std::io::Write, err: &mut impl std::io::Write) -> u8 {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(err, "emob verify: {path}: {error}");
            return 66;
        }
    };
    let values = match emob_eichrecht::transparency::from_xml(&text) {
        Ok(values) => values,
        Err(error) => {
            let _ = writeln!(
                err,
                "emob verify: {path} is not a transparency file: {error}"
            );
            return 66;
        }
    };
    if values.entries.is_empty() {
        let _ = writeln!(err, "emob verify: {path} holds no value");
        return 66;
    }

    let (mut verified, mut failed, mut unchecked) = (0usize, 0usize, 0usize);
    let _ = writeln!(out, "{path}: {} signed value(s)", values.entries.len());
    for (index, entry) in values.entries.iter().enumerate() {
        let outcome = if !entry.is_ocmf() {
            Outcome::NotOcmf(entry.format.clone())
        } else if let Some(key) = &entry.public_key {
            Outcome::Checked(emob_verify::check(&entry.signed_data, key))
        } else {
            Outcome::NoKey
        };
        let (mark, what, sentence) = match &outcome {
            Outcome::Checked(check) => {
                let what = check
                    .readings
                    .iter()
                    .map(|r| format!("{} {} {} {}", r.marker, r.value, r.unit, r.time))
                    .collect::<Vec<_>>()
                    .join(" · ");
                match check.verdict {
                    Verdict::Verified => verified += 1,
                    Verdict::NotVerified | Verdict::KeyMismatch => failed += 1,
                    _ => unchecked += 1,
                }
                let mark = match check.verdict {
                    Verdict::Verified => "✓",
                    Verdict::NotVerified | Verdict::KeyMismatch => "✗",
                    _ => "?",
                };
                (mark, what, check.verdict.sentence().to_owned())
            }
            Outcome::NotOcmf(format) => {
                unchecked += 1;
                (
                    "?",
                    String::new(),
                    format!("not an OCMF value (format {format:?})"),
                )
            }
            Outcome::NoKey => {
                unchecked += 1;
                (
                    "?",
                    String::new(),
                    "the file carries no key for this value".to_owned(),
                )
            }
        };
        // The sentence sits under the value, indented past the mark and number.
        let indent = " ".repeat(8);
        let _ = writeln!(out, "  {mark} {:>2}  {what}", index + 1);
        let _ = writeln!(out, "{indent}{sentence}");
    }
    let _ = writeln!(
        out,
        "{verified} verified, {failed} not verified, {unchecked} not checkable here"
    );
    if failed > 0 {
        1
    } else if unchecked > 0 {
        2
    } else {
        0
    }
}
