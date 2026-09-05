//! `roamd` — the daemon both roaming wires wait on.
//!
//! Sockets, a clock and two queues. What a record *says* is decided by
//! [`emob_cdr`] and carried by crossings that already exist; what this process
//! adds is *who*, *when*, and *whether it arrived* — see [`roamd`] for why that
//! split is the whole design rather than a convenience.
//!
//! # The two loops are the product
//!
//! **Outbound**, every tick: prepare each pending consignment in the version its
//! partner speaks, send, and record only what came back accepted. Then ask the
//! sharper question — which records their partner agreed to have by now and does
//! not — and log each one against *that partner's own* window. That second list
//! is what an operator is paged for: it is not work outstanding, it is a
//! delivered session that nobody has been billed for.
//!
//! **Inbound** is not a loop. A partner pushes, and the answer is a verdict:
//! accepted, disputed, a duplicate, a restatement, or a refusal — see
//! [`roamd::Verdict`]. The transport is the one leg CI cannot run, because a
//! peer is somebody else's server behind a credentials exchange, exactly as
//! `csmsd`'s WebSocket and the Mobilithek subscription are.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use emob_core::PartyId;
use emob_roam::{Partner, PartnerRegistry};
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use roamd::Roamd;

/// What the daemon waits for before it takes traffic.
///
/// A roaming node with no partners cannot route anything, and answering a
/// readiness probe before the registry is loaded is a node that accepts a push
/// it has nowhere to put.
const PARTNERS: &str = "partners";

/// How often the outbound queue is drained.
const TICK: std::time::Duration = std::time::Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,roamd=debug", false);

    let http: SocketAddr = std::env::var("ROAMD_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9583".to_owned())
        .parse()
        .context("ROAMD_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new().expecting(PARTNERS);
    let shutdown = Shutdown::new();

    // A real deployment loads the registry from the store the credentials
    // exchange writes to. The service routes to nobody it was not given, and
    // refuses rather than guessing.
    let registry = PartnerRegistry::new(PartyId::new("DE", "ABC")?)
        .with(Partner::emsp(PartyId::new("NL", "TNM")?).on_signed_data());
    let service = Arc::new(Mutex::new(Roamd::new(registry)));
    readiness.up(PARTNERS);

    let queue = Arc::clone(&service);
    let stopping = shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if stopping.is_stopping() {
                return;
            }
            drain(&queue);
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

/// One tick of the outbound queue.
///
/// Split out because it is the whole of what the daemon does, and because the
/// only thing that must never happen here is an `accepted` before a send.
fn drain(service: &Mutex<Roamd>) {
    let now = time::OffsetDateTime::now_utc();

    // Read under the lock and sent outside it: a push is a socket, and a socket
    // held under a lock is a daemon that stops answering its own probe.
    let pending: Vec<_> = lock(service).pending().cloned().collect();

    for consignment in pending {
        // A real deployment builds the document here — `Roamd::prepare` for a
        // peer, `Roamd::prepare_for_broker` for Hubject, chosen by the wire the
        // consignment records rather than by the caller — `POST`s it, and calls
        // `accepted` with the `Location` the receiver returned. Only a delivery
        // that came back accepted is recorded.
        tracing::info!(
            key = %consignment.key,
            recipient = %consignment.recipient(),
            wire = %consignment.wire,
            "would send"
        );
    }

    for late in lock(service).unsettled(now) {
        // Not a backlog item: a session that has been delivered, settled on
        // this side, and never billed to anybody.
        tracing::error!(unsettled = %late, "a record is past its partner's own window");
    }
}

/// A lock that survives a panic elsewhere.
///
/// The guarded value is a plain map, and a roaming node that answers a poisoned
/// lock by refusing to send a record it has already settled is a worse failure
/// than the one it is avoiding — the same reasoning `csmsd` states for its
/// ledgers.
fn lock(service: &Mutex<Roamd>) -> std::sync::MutexGuard<'_, Roamd> {
    service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
