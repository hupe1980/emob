//! Where a daemon's settings come from, and what they may not print.

use core::fmt;
use std::path::Path;

use serde::de::DeserializeOwned;

/// A configured value that must not reach a log line.
///
/// Every daemon here holds at least one: an OCPI credentials token, a webhook
/// secret, a database URL with a password in it. The commonest way each of them
/// escapes is not an attack — it is a `tracing::info!(?settings)` written while
/// debugging something else and never taken out.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Secret(String);

impl Secret {
    /// A secret from its configured form.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, at the one call site that needs it.
    ///
    /// Deliberately verbose: a reader scanning for where a secret leaves the
    /// type finds every place by grepping one name.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl fmt::Display for Secret {
    /// Also redacted. A `Display` that printed the value would make
    /// `format!("{secret}")` the hole `Debug` was closed to prevent.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("…")
    }
}

/// Why a daemon could not read its settings.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file is not there.
    #[error("no configuration at {path}: a daemon with no settings has nothing to bind to")]
    Missing {
        /// Where it was looked for.
        path: String,
    },
    /// The file could not be read.
    #[error("{path} could not be read: {source}")]
    Unreadable {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The file is not the shape the daemon expects.
    #[error("{path} is not valid configuration: {detail}")]
    Malformed {
        /// Which file.
        path: String,
        /// What the parser said.
        detail: String,
    },
    /// An environment override is not the shape the field expects.
    #[error("the environment variable {key} is not valid for that setting: {detail}")]
    BadOverride {
        /// Which variable.
        key: String,
        /// What the parser said.
        detail: String,
    },
}

/// Read a daemon's settings from a TOML file, then let the environment override
/// them.
///
/// # The layering, and why it is this way round
///
/// The file is the reviewed artefact: it is in the repository, it is in the
/// image, and a change to it is a change somebody approved. The environment is
/// the deployment's, and it wins — because the one thing a deployment always
/// has to change is the thing the file cannot know, which is where it is
/// running.
///
/// An override is `<PREFIX>_<FIELD>` in upper case, with a `__` for nesting:
/// `CSMSD_BIND`, `CSMSD_WEBHOOK__SECRET`. A key that matches no field is
/// **ignored**, deliberately — a process inherits an environment it does not
/// own, and a daemon that refused to start because the orchestrator set
/// `CSMSD_HOME` would be a daemon nobody could deploy.
///
/// # Errors
///
/// [`ConfigError`] when the file is absent, unreadable, malformed, or an
/// override does not parse into the field it names.
pub fn load<T: DeserializeOwned>(path: &Path, prefix: &str) -> Result<T, ConfigError> {
    let display = path.display().to_string();
    if !path.exists() {
        return Err(ConfigError::Missing { path: display });
    }
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Unreadable {
        path: display.clone(),
        source,
    })?;
    let mut value: toml::Value = toml::from_str(&text).map_err(|error| ConfigError::Malformed {
        path: display.clone(),
        detail: error.to_string(),
    })?;

    apply_environment(&mut value, prefix, std::env::vars())?;

    value
        .try_into()
        .map_err(|error: toml::de::Error| ConfigError::Malformed {
            path: display,
            detail: error.to_string(),
        })
}

/// The same, from settings and variables a caller supplies.
///
/// What the tests use, and what a daemon embedding another one uses. `load`
/// is this function with the filesystem and the process environment in front of
/// it — so the layering is testable without either.
///
/// # Errors
///
/// [`ConfigError::Malformed`] when the text is not TOML or does not fit `T`,
/// and [`ConfigError::BadOverride`] when an override does not parse.
pub fn load_from<T, I>(text: &str, prefix: &str, environment: I) -> Result<T, ConfigError>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = (String, String)>,
{
    let mut value: toml::Value = toml::from_str(text).map_err(|error| ConfigError::Malformed {
        path: "<in memory>".to_owned(),
        detail: error.to_string(),
    })?;
    apply_environment(&mut value, prefix, environment)?;
    value
        .try_into()
        .map_err(|error: toml::de::Error| ConfigError::Malformed {
            path: "<in memory>".to_owned(),
            detail: error.to_string(),
        })
}

/// Fold every matching variable into the parsed document.
fn apply_environment<I>(
    value: &mut toml::Value,
    prefix: &str,
    environment: I,
) -> Result<(), ConfigError>
where
    I: IntoIterator<Item = (String, String)>,
{
    let prefix = format!("{}_", prefix.to_ascii_uppercase());
    for (key, raw) in environment {
        let Some(path) = key.strip_prefix(&prefix) else {
            continue;
        };
        let segments: Vec<String> = path.split("__").map(str::to_ascii_lowercase).collect();
        if segments.iter().any(String::is_empty) {
            continue;
        }
        set(value, &segments, &raw, &key)?;
    }
    Ok(())
}

/// Set one dotted path, creating tables on the way.
fn set(
    value: &mut toml::Value,
    segments: &[String],
    raw: &str,
    key: &str,
) -> Result<(), ConfigError> {
    let Some((last, parents)) = segments.split_last() else {
        return Ok(());
    };
    let mut cursor = value;
    for segment in parents {
        // A path through a non-table is a variable that does not name a field.
        // Ignored, for the reason `load` documents: a process inherits an
        // environment it does not own.
        if !cursor.is_table() {
            return Ok(());
        }
        cursor = cursor
            .as_table_mut()
            .expect("checked")
            .entry(segment.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    }
    let Some(table) = cursor.as_table_mut() else {
        return Ok(());
    };

    // The variable is text and the field has a type. `"9000"` has to reach a
    // `u16` field as a number, and `"true"` a `bool` — so the value is parsed
    // as TOML first and falls back to a string, which is what makes
    // `CSMSD_BIND=0.0.0.0:9000` and `CSMSD_WORKERS=4` both work.
    let parsed = raw
        .parse::<toml::Value>()
        .unwrap_or_else(|_| toml::Value::String(raw.to_owned()));

    // A field that exists keeps its type: an override that would change one is
    // the deployment saying something the daemon cannot mean, and guessing is
    // how a port becomes a string.
    if let Some(existing) = table.get(last)
        && !same_shape(existing, &parsed)
    {
        return Err(ConfigError::BadOverride {
            key: key.to_owned(),
            detail: format!(
                "the setting is {} and `{raw}` is {}",
                kind(existing),
                kind(&parsed)
            ),
        });
    }
    table.insert(last.clone(), parsed);
    Ok(())
}

const fn same_shape(a: &toml::Value, b: &toml::Value) -> bool {
    // An integer where a float is expected is the one widening that is always
    // safe and always meant.
    matches!(
        (a, b),
        (toml::Value::Integer(_), toml::Value::Integer(_))
            | (toml::Value::String(_), toml::Value::String(_))
            | (toml::Value::Boolean(_), toml::Value::Boolean(_))
            | (toml::Value::Array(_), toml::Value::Array(_))
            | (toml::Value::Table(_), toml::Value::Table(_))
            | (
                toml::Value::Float(_),
                toml::Value::Float(_) | toml::Value::Integer(_)
            )
    )
}

const fn kind(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "text",
        toml::Value::Integer(_) => "a number",
        toml::Value::Float(_) => "a decimal",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "an instant",
        toml::Value::Array(_) => "a list",
        toml::Value::Table(_) => "a table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Webhook {
        secret: String,
        retries: i64,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Settings {
        bind: String,
        workers: i64,
        webhook: Webhook,
    }

    const TOML: &str = r#"
bind = "127.0.0.1:9000"
workers = 2
[webhook]
secret = "whsec_from-the-file"
retries = 3
"#;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_file_is_the_default_and_the_deployment_wins() {
        let settings: Settings = load_from(
            TOML,
            "csmsd",
            env(&[
                ("CSMSD_BIND", "0.0.0.0:9000"),
                ("CSMSD_WEBHOOK__SECRET", "whsec_from-the-orchestrator"),
            ]),
        )
        .unwrap();

        assert_eq!(settings.bind, "0.0.0.0:9000");
        assert_eq!(settings.webhook.secret, "whsec_from-the-orchestrator");
        // …and everything nobody overrode is still the reviewed value.
        assert_eq!(settings.workers, 2);
        assert_eq!(settings.webhook.retries, 3);
    }

    #[test]
    fn a_variable_is_text_and_a_field_has_a_type() {
        // `CSMSD_WORKERS=4` has to reach an integer field as an integer.
        let settings: Settings = load_from(TOML, "csmsd", env(&[("CSMSD_WORKERS", "4")])).unwrap();
        assert_eq!(settings.workers, 4);
    }

    #[test]
    fn a_variable_that_names_no_field_is_ignored_rather_than_fatal() {
        // A process inherits an environment it does not own. A daemon that
        // refused to start because the orchestrator set `CSMSD_HOME` would be a
        // daemon nobody could deploy.
        let settings: Settings = load_from(
            TOML,
            "csmsd",
            env(&[
                ("CSMSD_HOME", "/var/lib/csmsd"),
                ("PATH", "/usr/bin"),
                ("OTHERD_BIND", "0.0.0.0:1"),
            ]),
        )
        .unwrap();
        assert_eq!(settings.bind, "127.0.0.1:9000");
    }

    #[test]
    fn an_override_that_would_change_a_fields_type_is_refused() {
        // Guessing is how a port becomes a string. The deployment is told which
        // setting it disagrees with rather than finding out from a parse error
        // three layers down.
        let error = load_from::<Settings, _>(TOML, "csmsd", env(&[("CSMSD_WORKERS", "many")]))
            .expect_err("a type change");
        assert!(matches!(error, ConfigError::BadOverride { .. }), "{error}");
        assert!(error.to_string().contains("CSMSD_WORKERS"), "{error}");
        assert!(error.to_string().contains("a number"), "{error}");
    }

    #[test]
    fn a_secret_prints_as_nothing_in_either_formatter() {
        // `Debug` is the hole; `Display` would be the same hole, reopened.
        let secret = Secret::new("whsec_AAAA");
        assert_eq!(format!("{secret:?}"), "Secret(…)");
        assert_eq!(format!("{secret}"), "…");
        assert_eq!(secret.expose(), "whsec_AAAA");
    }

    #[test]
    fn a_missing_file_says_so_rather_than_starting_on_defaults() {
        let error =
            load::<Settings>(Path::new("/nonexistent/csmsd.toml"), "csmsd").expect_err("absent");
        assert!(matches!(error, ConfigError::Missing { .. }), "{error}");
    }
}
