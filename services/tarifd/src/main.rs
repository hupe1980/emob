//! `tarifd` — the daemon that publishes a tariff version.
//!
//! Sockets, a clock and a schedule. What a price *says* is decided by
//! [`emob_tariff`] and carried by three crossings that already exist; what this
//! process adds is *when* — see [`tarifd`] for why that split is the whole
//! design rather than a convenience.
//!
//! # The loop is the product
//!
//! Every tick: ask the service which versions take effect within the lead,
//! prepare each one (all three audiences or none), send, and record only what
//! came back accepted. Then ask the sharper question — which versions are in
//! force that somebody was never told about — and log each one with the article
//! it breaches. That second list is what an operator is paged for: it is not
//! work outstanding, it is a price the estate is charging that a driver was
//! never shown.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use emob_core::PartyId;
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use tarifd::{Audience, Tarifd};

/// What the daemon waits for before it takes traffic.
const TARIFFS: &str = "tariffs";

/// How far ahead of a version taking effect it is published.
///
/// `[AFIR Art. 5(4)]` requires the price to be known to the driver **before**
/// they initiate a session, so publishing when a version takes effect is
/// already late for everybody at a point at that instant. An hour is the
/// operator's call; being ahead of it is the duty.
const LEAD: time::Duration = time::Duration::hours(1);

/// How often the schedule is asked.
const TICK: std::time::Duration = std::time::Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,tarifd=debug", false);

    let http: SocketAddr = std::env::var("TARIFD_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9581".to_owned())
        .parse()
        .context("TARIFD_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new().expecting(TARIFFS);
    let shutdown = Shutdown::new();

    // A real deployment loads these from the store `tarifd` writes to. The
    // service holds no tariff it was not given, and refuses to publish one.
    let service = Arc::new(Mutex::new(Tarifd::new()));
    let party = PartyId::new("DE", "ABC")?;
    readiness.up(TARIFFS);

    let schedule = Arc::clone(&service);
    let stopping = shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if stopping.is_stopping() {
                return;
            }
            publish_due(&schedule, &party);
        }
    });

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}

/// One tick of the schedule.
///
/// Split out because it is the whole of what the daemon does, and because the
/// only thing that must never happen here is a `confirm` before a send.
fn publish_due(service: &Mutex<Tarifd>, party: &PartyId) {
    let now = time::OffsetDateTime::now_utc();

    // Prepared under the lock and sent outside it: a push is a socket, and a
    // socket held under a lock is a daemon that stops answering its own probe.
    let prepared = {
        let held = lock(service);
        held.due(now, LEAD)
            .into_iter()
            .filter_map(|version| match held.prepare(version, party, now) {
                Ok(publication) => Some(publication),
                Err(error) => {
                    // A version no audience can be given is not a version to
                    // publish partially. It stays due, and it will be reported
                    // late the moment it takes effect.
                    tracing::error!(%error, "a version could not be prepared for any audience");
                    None
                }
            })
            .collect::<Vec<_>>()
    };

    for publication in prepared {
        for note in &publication.notes {
            tracing::info!(pointer = %note.pointer, reason = %note.reason, "the crossing cost this");
        }
        for audience in Audience::ALL {
            // A real deployment puts the payload on a socket here —
            // `SetDefaultTariff` to every 2.1 station, the `snapshotPush` to
            // the national access point, a `PUT` to the partner — and only a
            // delivery that came back accepted is recorded.
            tracing::info!(
                tariff = %publication.tariff_id,
                version = %publication.fingerprint.short(),
                %audience,
                "would publish"
            );
            let _ = audience;
        }
        let _ = &publication;
    }

    for late in lock(service).late(now) {
        // Not a backlog item: a price the estate is charging that one of the
        // three audiences has never seen.
        tracing::error!(breach = %late, "a version in force was never published");
    }
}

/// A lock that survives a panic elsewhere.
///
/// The guarded value is a plain map, and a daemon that answers a poisoned lock
/// by refusing to publish a price is a worse failure than the one it is
/// avoiding — the same reasoning `csmsd` states for its ledgers.
fn lock(service: &Mutex<Tarifd>) -> std::sync::MutexGuard<'_, Tarifd> {
    service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
