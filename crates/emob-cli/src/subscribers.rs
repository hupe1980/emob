//! `emob subscribers test <name>` — the "send test webhook" button, as a
//! command (spec 012).
//!
//! One signed sample of every type the subscriber takes, sent exactly the way
//! a daemon sends the real one — structured-mode CloudEvent, Standard Webhooks
//! signature under the subscriber's own secret — so a receiver is proven before
//! the first real record reaches it. A sample is marked `emobtest = true` and
//! its id starts `test-`; a receiver must not book it, and nothing is
//! journalled (SR-143). The URL is checked by the estate's own rules first,
//! and the secret is never printed.
//!
//! Exit status: `0` every sample answered `2xx` · `1` one did not · `2` usage
//! or an estate that names no such subscriber.

use std::io::Write;
use std::time::{Duration, Instant};

use emob_service::cloudevent::CloudEvent;
use emob_service::estate::{self, Estate};
use emob_service::events;
use emob_service::webhook::transport;

/// How far the receiver's own answer is indented under its line.
const INDENT: &str = "    ";

const USAGE: &str = "usage: emob subscribers test <name> [--estate PATH]";

/// `emob subscribers test <name> [--estate PATH]`.
pub fn main(args: &[String], out: &mut impl Write, err: &mut impl Write) -> u8 {
    let (name, path) = match args {
        [verb, name] if verb == "test" => (name.clone(), estate::path()),
        [verb, name, flag, path] if verb == "test" && flag == "--estate" => {
            (name.clone(), std::path::PathBuf::from(path))
        }
        _ => {
            let _ = writeln!(err, "{USAGE}");
            return 2;
        }
    };
    let whole: Estate = match estate::load(&path) {
        Ok(whole) => whole,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return 2;
        }
    };
    let Some(subscriber) = whole.subscribers.iter().find(|s| s.name == name).cloned() else {
        let names: Vec<&str> = whole.subscribers.iter().map(|s| s.name.as_str()).collect();
        let _ = writeln!(
            err,
            "error: the estate names no subscriber `{name}`\n  = help: its [[subscribers]] are: {}",
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(", ")
            }
        );
        return 2;
    };
    let subscription = match estate::subscriptions(&[subscriber], &estate::process_env()) {
        Ok(mut list) => list.remove(0),
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return 2;
        }
    };
    let mut failed = false;
    for kind in events::ALL
        .iter()
        .copied()
        .filter(|kind| subscription.takes(kind))
    {
        let mut nonce = [0_u8; 16];
        if let Err(error) = getrandom::fill(&mut nonce) {
            let _ = writeln!(err, "could not generate a test event id: {error}");
            return 2;
        }
        let id = format!("test-{}", hex::encode(nonce));
        let body = sample(kind, &id);
        let at = time::OffsetDateTime::now_utc();
        let started = Instant::now();
        let answer = runtime.block_on(async {
            let client = transport::Client::new(Duration::from_secs(10))?;
            client
                .post(&subscription.url, &subscription.secret, &id, body, at)
                .await
        });
        let took = started.elapsed().as_millis();
        match answer {
            Ok(transport::Reply { status, text }) => {
                let ok = status.is_success();
                failed |= !ok;
                let said: String = text.chars().take(200).collect();
                let _ = writeln!(
                    out,
                    "{mark} → {url} {kind} … {status} ({took} ms){said}",
                    mark = if ok { "✓" } else { "✗" },
                    url = subscription.url,
                    said = if said.is_empty() {
                        String::new()
                    } else {
                        format!("\n{INDENT}{said}")
                    },
                );
            }
            Err(error) => {
                failed = true;
                let _ = writeln!(out, "✗ → {} {kind} … no answer ({error})", subscription.url);
            }
        }
    }
    u8::from(failed)
}

/// A sample of `kind`, as a daemon would send it, marked as a test.
fn sample(kind: &'static str, id: &str) -> String {
    let mut event = if kind == events::invoice::ISSUED {
        CloudEvent::new(
            id,
            "billd",
            kind,
            serde_json::Value::String(
                "<?xml version=\"1.0\"?><Invoice><!-- emob subscribers test: not an invoice --></Invoice>"
                    .to_owned(),
            ),
        )
        .about(format!("invoice/{id}"))
        .content_type("application/xml")
    } else {
        CloudEvent::new(
            id,
            "csmsd",
            kind,
            serde_json::json!({
                "cdr": { "key": id, "note": "emob subscribers test: not a record" },
                "token": {},
                "signed": [],
                "station": "TEST"
            }),
        )
        .about(format!("cdr/{id}"))
    }
    .at(time::OffsetDateTime::now_utc());
    event
        .extensions
        .insert("emobtest".to_owned(), serde_json::Value::Bool(true));
    serde_json::to_string(&event).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sr_143_a_sample_is_marked_and_matches_its_published_schema() {
        for (kind, schema) in [
            (
                events::cdr::ISSUED,
                include_str!("../../../schemas/events/de.emob.cdr.issued.json"),
            ),
            (
                events::invoice::ISSUED,
                include_str!("../../../schemas/events/de.emob.invoice.issued.json"),
            ),
        ] {
            let body: serde_json::Value =
                serde_json::from_str(&sample(kind, "test-example")).unwrap();
            assert_eq!(body["emobtest"], true, "{kind}");
            assert!(body["id"].as_str().unwrap().starts_with("test-"));
            let schema: serde_json::Value = serde_json::from_str(schema).unwrap();
            let validator = jsonschema::validator_for(&schema).unwrap();
            let errors: Vec<String> = validator
                .iter_errors(&body)
                .map(|e| e.to_string())
                .collect();
            assert!(errors.is_empty(), "{kind}: {errors:?}");
        }
    }
}
