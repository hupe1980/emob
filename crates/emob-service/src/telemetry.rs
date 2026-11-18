//! Structured logging, and the one field every line in this workspace carries.
//!
//! # Why a daemon's logs are a compliance artefact here
//!
//! `[MessEG §33]` gives a customer years to ask how a kilowatt-hour was billed,
//! and the answer is a replay — but the *question* usually arrives as "which
//! session was this", and finding it starts in a log. A log line that cannot be
//! joined to a session id, a charge point and a party is a line that answers
//! nothing.
//!
//! So the format is JSON by default in a deployment and human-readable on a
//! terminal, and the daemon's own name and version are on every line — because
//! a fleet runs several daemons into one collector and "which one said this" is
//! the first thing anybody asks.

use crate::health::Identity;

/// The variable that picks the human-readable format: `EMOB_LOG_FORMAT=text`.
pub const FORMAT_VARIABLE: &str = "EMOB_LOG_FORMAT";

/// Start logging.
///
/// The filter comes from `RUST_LOG` when it is set and from `default` when it is
/// not — the usual convention, stated because a daemon that logged nothing until
/// somebody found the variable would be a daemon nobody could debug.
///
/// JSON lines unless `EMOB_LOG_FORMAT=text`: a deployment feeds a collector,
/// and a log line an incident report has to cite carries its request id and
/// actor as fields (`span.id`, `span.actor`), not as words in a sentence. The
/// human format is one variable away for a terminal. It is not a probe of
/// whether stdout is a tty, because a container's stdout is not one.
///
/// Calling this twice is a no-op rather than a panic: a test binary that starts
/// two daemons must not fall over on the second.
pub fn init(identity: Identity, default: &str) {
    let json = std::env::var(FORMAT_VARIABLE).map_or(true, |format| format != "text");
    if start(default, json, std::io::stdout) {
        tracing::info!(
            daemon = identity.name,
            version = identity.version,
            "logging started"
        );
    }
}

fn start<W>(default: &str, json: bool, writer: W) -> bool
where
    W: for<'w> tracing_subscriber::fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_writer(writer),
            )
            .try_init()
            .is_ok()
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_writer(writer))
            .try_init()
            .is_ok()
    }
}

/// Every log line of this process, as JSON, kept in memory — what
/// `emob demo --check` searches for a secret, a token or a card UID (SR-102).
#[derive(Clone, Default)]
pub struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl core::fmt::Debug for Captured {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Captured(…)")
    }
}

impl Captured {
    /// Start capturing at `filter`, or `None` when this process already logs.
    #[must_use]
    pub fn start(filter: &str) -> Option<Self> {
        let captured = Self::default();
        let writer = captured.clone();
        start(filter, true, move || writer.clone()).then_some(captured)
    }

    /// What was logged so far.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(
            &self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_twice_is_a_no_op_rather_than_a_panic() {
        // A test binary that starts two daemons must not fall over on the
        // second, and neither must a process that embeds one.
        let identity = crate::identity!();
        init(identity, "info");
        init(identity, "debug");
    }
}
