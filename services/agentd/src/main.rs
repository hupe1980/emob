//! `agentd` — the advisory plane a charging operator reads in the morning.
//!
//! Sockets and configuration. What the specialists decide is in [`agentd`], and
//! what they may decide is bounded there rather than here.

use std::net::SocketAddr;
use std::sync::Arc;

use agentd::{advisory, runtime};
use agentplane::prelude::{JournalStore, RedbStore};
use anyhow::{Context as _, Result};
use emob_core::PartyId;
use emob_service::{Identity, Principal, Readiness, Role, Server, Shutdown, identity};

/// What the daemon waits for before it takes traffic.
const JOURNAL: &str = "journal";
const PRINCIPAL: &str = "principal";

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,agentd=debug", false);

    let http: SocketAddr = std::env::var("AGENTD_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9580".to_owned())
        .parse()
        .context("AGENTD_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new().expecting(JOURNAL).expecting(PRINCIPAL);
    let shutdown = Shutdown::new();

    // A real deployment opens a file, or a database several instances share.
    let store: Arc<dyn JournalStore> =
        Arc::new(RedbStore::open_in_memory().context("opening the journal")?);
    let plane = runtime(Arc::clone(&store));
    readiness.up(JOURNAL);

    // The operator this daemon acts for, and the attenuated principal every
    // specialist runs under. `advisory` is the only constructor, and it cannot
    // widen — so "nothing an agent says can move money" is settled here.
    let operator = Principal::operator(PartyId::new("DE", "ABC")?, Role::Cpo);
    let agent = advisory(&operator).context("the operator does not hold every read capability")?;
    tracing::info!(
        party = %agent.party,
        role = %agent.role,
        capabilities = ?agent.capabilities.patterns(),
        "specialists run under an advisory principal"
    );
    readiness.up(PRINCIPAL);

    tracing::info!(
        specialists = ?agentd::registered_specialists(),
        "registered"
    );
    // The runtime is held for the life of the process; the HTTP surface is what
    // an operator and an orchestrator reach.
    let _plane = plane;

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        // Nothing here holds a long-lived connection, so a short window is
        // enough for the requests in flight.
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}
