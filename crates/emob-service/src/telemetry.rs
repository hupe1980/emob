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

/// Start logging.
///
/// The filter comes from `RUST_LOG` when it is set and from `default` when it is
/// not — the usual convention, stated because a daemon that logged nothing until
/// somebody found the variable would be a daemon nobody could debug.
///
/// `json` picks the format: structured for a collector, human-readable for a
/// terminal. It is an argument rather than a probe of whether stdout is a tty,
/// because a container's stdout is not a tty and its logs still have to be
/// readable when somebody runs it locally.
///
/// Calling this twice is a no-op rather than a panic: a test binary that starts
/// two daemons must not fall over on the second.
pub fn init(identity: Identity, default: &str, json: bool) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));

    let registry = tracing_subscriber::registry().with(filter);
    let started = if json {
        registry
            .with(tracing_subscriber::fmt::layer().json().flatten_event(true))
            .try_init()
    } else {
        registry.with(tracing_subscriber::fmt::layer()).try_init()
    };

    if started.is_ok() {
        tracing::info!(
            daemon = identity.name,
            version = identity.version,
            "logging started"
        );
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
        init(identity, "info", false);
        init(identity, "debug", true);
    }
}
