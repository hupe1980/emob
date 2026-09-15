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
use emob_roam::Wire;
use emob_roam::{Partner, PartnerRegistry};
use emob_service::journal::Journal as _;
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use roamd::{Answer, Dispatcher, Node, Roamd};

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

    let http: SocketAddr = emob_service::serve::bind_address("roamd", "127.0.0.1:9583")?;

    let readiness = Readiness::new().expecting(PARTNERS);
    let shutdown = Shutdown::new();

    // The registry is configuration and the ledger is a fact, and they are
    // loaded from different places for that reason: a node that replayed an old
    // registry would route today's records by yesterday's contracts, while a
    // node that forgot its ledger would re-send records the partner has already
    // billed. So the registry comes from the credentials exchange and the
    // consignments come from the journal.
    let me_party = PartyId::new("DE", "ABC")?;
    let registry = PartnerRegistry::new(me_party.clone())
        .with(Partner::emsp(PartyId::new("NL", "TNM")?).on_signed_data());
    let journal = emob_service::journal::for_daemon("roamd")
        .context("opening roamd's journal — see EMOB_JOURNAL_DIR")?;
    tracing::info!(path = %journal.path().display(), "folding the journal");
    let node = Node::open(Roamd::new(registry), journal).context("folding roamd's journal")?;
    tracing::info!(
        head = %node.journal().head().unwrap_or_default(),
        "the consignment ledger is what the journal says it is"
    );
    let service = Arc::new(Mutex::new(node));
    readiness.up(PARTNERS);

    // The outbound half. It holds no peer list: who this node can reach is the
    // set of platforms that have **registered** with it, which is the node's own
    // journalled state — so a partner that completes the handshake is reachable
    // from that moment, and there is no second map to keep in step (D330).
    let dispatcher = Dispatcher::new(&me_party, ocpi_kit::client::ClientConfig::default())
        .context("building the OCPI client")?;
    tracing::info!(
        registered = service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state()
            .peers()
            .registered()
            .count(),
        "outbound"
    );

    let queue = Arc::clone(&service);
    let stopping = shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if stopping.is_stopping() {
                return;
            }
            drain(&queue, &dispatcher).await;
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
///
/// # Three rules, and the middle one is the reason this is `async`
///
/// **Read under the lock, send outside it.** A push is a socket, and a socket
/// held under a lock is a daemon that stops answering its own probe. The lock
/// is taken to read the queue, dropped, and taken again to record each answer —
/// which is also why `Node` records one delivery at a time rather than a batch.
///
/// **Render what is sent, and send what was rendered.**
/// `emob_roam::ocpi::cdr::for_partner` produces the partner's copy *and* the
/// account of what carrying it there cost, by JSON Pointer into that copy. The
/// bytes that go on the wire are those bytes.
///
/// **Only an answer reaches the ledger.** A refusal is a verdict; a timeout is
/// not. See [`roamd::Answer`].
async fn drain(service: &Mutex<Node<emob_service::journal::File>>, dispatcher: &Dispatcher) {
    let now = time::OffsetDateTime::now_utc();

    let pending: Vec<_> = lock(service).state().pending().cloned().collect();

    for consignment in pending {
        if consignment.wire != Wire::Ocpi {
            // The second wire is a broker over mutual TLS and its client is not
            // here yet. The **document** for it is built already —
            // `Roamd::prepare_for_broker` is the OICP crossing, and it refuses
            // what that wire cannot carry rather than flattening it — so what
            // is missing is the socket and the credentials, not a rule.
            //
            // Named rather than skipped: a queue that silently holds a record
            // it has no way to send is a backlog nobody is told about.
            tracing::warn!(
                key = %consignment.key,
                wire = %consignment.wire,
                "no client for this wire yet, so the record stays queued"
            );
            continue;
        }

        // The document is the record's, and it is rendered from a store this
        // daemon does not own: the session's location, its signed payloads and
        // the token it was authorised with. Until that store exists, a record
        // whose document cannot be built is reported rather than dropped.
        let Some(document) = render(service, &consignment) else {
            tracing::warn!(
                key = %consignment.key,
                recipient = %consignment.recipient(),
                "this record was routed without a document, so there is nothing to send"
            );
            continue;
        };

        // The registration is the credential *and* the endpoint, and it is
        // read fresh each tick: a partner that rotated its token this morning
        // is reached with the new one.
        let Some(to) = lock(service)
            .state()
            .peers()
            .for_party(consignment.recipient())
            .cloned()
        else {
            tracing::warn!(
                key = %consignment.key,
                recipient = %consignment.recipient(),
                "no platform has registered for this party, so the record stays owed — a \
                 credentials exchange that has not happened is not a record that was refused"
            );
            continue;
        };
        let answer = dispatcher.deliver(&to, &document).await;
        match &answer {
            Answer::Accepted { location } => tracing::info!(
                key = %consignment.key,
                recipient = %consignment.recipient(),
                location = location.as_deref().unwrap_or("(none returned)"),
                "the partner took it"
            ),
            Answer::Refused { reason } => tracing::error!(
                key = %consignment.key,
                recipient = %consignment.recipient(),
                reason = reason.as_str(),
                "the partner refused it — a person has to read this, not a retry"
            ),
            Answer::Unreached { why } => tracing::warn!(
                key = %consignment.key,
                recipient = %consignment.recipient(),
                why = why.as_str(),
                "nobody answered, so the record stays owed"
            ),
        }
        if let Err(error) = lock(service).answered(&consignment.key, &answer, now) {
            tracing::error!(key = %consignment.key, %error, "the answer could not be recorded");
        }
    }

    for late in lock(service).state().unsettled(now) {
        // Not a backlog item: a session that has been delivered, settled on
        // this side, and never billed to anybody.
        tracing::error!(unsettled = %late, "a record is past its partner's own window");
    }
}

/// The document for one consignment, as it was rendered when the record was
/// routed.
///
/// A lookup, not a rendering. See `Roamd`'s outbox: the account of what the
/// crossing cost points into the copy that was built at consign time, so
/// building a second one here — from a session that may since have been
/// re-rated — would put a document on the wire that the account does not
/// describe. `None` is a record consigned without a document, which the caller
/// is told about rather than guessed at.
fn render(
    service: &Mutex<Node<emob_service::journal::File>>,
    consignment: &roamd::Consignment,
) -> Option<emob_roam::ocpi::cdr::Outbound> {
    lock(service).state().document(&consignment.key).cloned()
}

/// A lock that survives a panic elsewhere.
///
/// The guarded value is a plain map, and a roaming node that answers a poisoned
/// lock by refusing to send a record it has already settled is a worse failure
/// than the one it is avoiding — the same reasoning `csmsd` states for its
/// ledgers.
fn lock(
    service: &Mutex<Node<emob_service::journal::File>>,
) -> std::sync::MutexGuard<'_, Node<emob_service::journal::File>> {
    service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
