//! `emob check` — an estate refused before a daemon reads it (spec 012).
//!
//! First the whole file against [`Estate`], which knows every section and
//! refuses one no daemon reads; a refusal is reported the way `rustc` reports
//! one — what, where, the line, and what would be valid. Then each daemon's
//! own checks, the functions it runs at start, on the same text. Secrets are
//! named, never printed (SR-141): without `--secrets` every `*_env` reads as a
//! placeholder so the checks that need a value still run; with it, each
//! variable must be set in this environment.
//!
//! Exit status: `0` every daemon would start · `1` refused · `2` unreadable or
//! usage.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use emob_service::estate::{self, Env, Estate, EstateError};

/// A secret that satisfies every format check and opens nothing.
const PLACEHOLDER: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// `emob check [--secrets] [--schema] <path>`.
pub fn main(args: &[String], out: &mut impl Write, err: &mut impl Write) -> u8 {
    let mut secrets = false;
    let mut path = None;
    for arg in args {
        match arg.as_str() {
            "--secrets" => secrets = true,
            "--schema" => {
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&estate::json_schema()).unwrap_or_default()
                );
                return 0;
            }
            other if path.is_none() && !other.starts_with('-') => path = Some(other.to_owned()),
            _ => {
                let _ = writeln!(
                    err,
                    "usage: emob check [--secrets] [--schema] <estate.toml>"
                );
                return 2;
            }
        }
    }
    let Some(path) = path else {
        let _ = writeln!(
            err,
            "usage: emob check [--secrets] [--schema] <estate.toml>"
        );
        return 2;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(err, "error: {path} could not be read ({error})");
            return 2;
        }
    };
    let env: Env = if secrets {
        estate::process_env()
    } else {
        placeholders()
    };
    match check(&text, &path, &env) {
        Ok(whole) => {
            let _ = writeln!(
                out,
                "✓ {path}: {stations} {s_word}, {tariffs} {t_word}, {partners} {p_word}, {operators} {o_word} — every daemon would start{secrets}",
                stations = whole.stations.len(),
                s_word = plural(whole.stations.len(), "station", "stations"),
                tariffs = whole.tariffs.len(),
                t_word = plural(whole.tariffs.len(), "tariff", "tariffs"),
                partners = whole.partners.len(),
                p_word = plural(whole.partners.len(), "partner", "partners"),
                operators = whole.operators.len(),
                o_word = plural(whole.operators.len(), "operator", "operators"),
                secrets = if secrets {
                    ", and every secret is set"
                } else {
                    ""
                },
            );
            0
        }
        Err(error) => {
            let _ = write!(err, "{}", report(&error, &path));
            1
        }
    }
}

/// Every variable reads as [`PLACEHOLDER`].
fn placeholders() -> Env {
    Arc::new(|_: &str| Some(PLACEHOLDER.to_owned()))
}

const fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// The checks, in the order a refusal is most useful in.
///
/// # Errors
///
/// The first refusal.
pub fn check(text: &str, path: &str, env: &Env) -> Result<Estate, EstateError> {
    let whole: Estate = estate::from_str_at(text, path)?;

    // csmsd: the fleet, the keys, the zone, the displays.
    if !whole.stations.is_empty() {
        let view: csmsd::estate::Estate = estate::from_str_at(text, path)?;
        csmsd::estate::provisioning(&view)?;
        csmsd::estate::registry(&view.meter_keys)?;
        csmsd::estate::zone(&view)?;
        csmsd::display::Display::from_estate(&view.stations)?;
        for (index, station) in view.stations.iter().enumerate() {
            if let Some(day) = &station.commissioned {
                time::Date::parse(day, &time::format_description::well_known::Iso8601::DATE)
                    .map_err(|_| EstateError::Invalid {
                        key: format!("stations[{index}].commissioned"),
                        reason: format!("{day} is not a date like 2024-05-02"),
                    })?;
            }
        }
        // roamd: every location a CDR will carry; poid: every site's zone.
        for (index, station) in view.stations.iter().enumerate() {
            roamd::daemon::location(index, station)?;
        }
        poid::http::sites(&estate::from_str_at(text, path)?)?;
    }
    // tarifd: every tariff builds and its history holds.
    if !whole.tariffs.is_empty() {
        tarifd::http::published(&estate::from_str_at(text, path)?)?;
    }
    // Everyone: the operators are actors, the subscribers are reachable and
    // subscribe to something that is emitted, the secrets are named.
    estate::operator_tokens(&whole.operators, env)?;
    estate::subscriptions(&whole.subscribers, env)?;
    if let Some(services) = &whole.services {
        estate::webhook_secret(services, env)?;
        services
            .service_token_env
            .resolve_in("services.service_token_env", env)?;
    }
    for (index, partner) in whole.partners.iter().enumerate() {
        partner
            .token_a_env
            .resolve_in(&format!("partners[{index}].token_a_env"), env)?;
    }
    if let Some(delivery) = whole.billing.as_ref().and_then(|b| b.delivery.as_ref()) {
        delivery
            .secret_env
            .resolve_in("billing.delivery.secret_env", env)?;
    }
    if let Some(desk) = &whole.desk {
        desk.token_env.resolve_in("desk.token_env", env)?;
    }
    Ok(whole)
}

/// A refusal as `rustc` would print it. A parse error already is one; a value
/// refused by a daemon's check names its key.
fn report(error: &EstateError, path: &str) -> String {
    match error {
        EstateError::Malformed { detail, .. } => detail.clone(),
        EstateError::Invalid { key, reason } => {
            format!("error: {reason}\n --> {path}: {key}\n")
        }
        EstateError::Unset { variable, key } => format!(
            "error: {variable} is not set\n --> {path}: {key}\n  = help: a secret is never written into the estate; export it, or `just dev-keys > .env` for this machine\n"
        ),
        other => format!("error: {other}\n"),
    }
}

/// Whether `path` names an estate this checks cleanly — for the demo's own
/// tests.
#[must_use]
pub fn passes(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|text| check(&text, &path.display().to_string(), &placeholders()).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/estate.example.toml"
        ))
        .unwrap()
    }

    fn run(text: &str, secrets: bool) -> (u8, String, String) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("emob-check-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("estate.toml");
        std::fs::write(&path, text).unwrap();
        let mut args = vec![path.display().to_string()];
        if secrets {
            args.insert(0, "--secrets".to_owned());
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = main(&args, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn the_example_estate_passes() {
        let (code, out, err) = run(&example(), false);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("every daemon would start"), "{out}");
    }

    #[test]
    fn a_misspelt_key_is_shown_on_its_line_with_the_key_it_meant() {
        let (code, _, err) = run(&example().replacen("evse = ", "evse_id = ", 1), false);
        assert_eq!(code, 1);
        assert!(err.starts_with("error: unknown field `evse_id`"), "{err}");
        assert!(err.contains("= help: did you mean `evse`?"), "{err}");
        assert!(err.contains("^^^^^^^"), "{err}");
    }

    #[test]
    fn a_section_nobody_reads_is_refused() {
        let (code, _, err) = run(
            &format!("{}\n[[subscriber]]\nname = \"x\"\n", example()),
            false,
        );
        assert_eq!(code, 1);
        assert!(err.contains("subscriber"), "{err}");
    }

    #[test]
    fn sr_141_a_secret_is_named_and_never_printed() {
        // Only the operator token is set, to a value the output must not hold.
        let text = example();
        let dir = std::env::temp_dir().join(format!("emob-check-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("estate.toml");
        std::fs::write(&path, &text).unwrap();
        let value = "a-very-secret-operator-token-0123456789";
        let env: Env =
            Arc::new(move |name| (name == "EMOB_OPERATOR_TOKEN").then(|| value.to_owned()));
        let error = check(&text, &path.display().to_string(), &env).unwrap_err();
        let printed = report(&error, "estate.toml");
        assert!(!printed.contains(value), "{printed}");
        assert!(printed.contains("is not set"), "{printed}");
    }
}
