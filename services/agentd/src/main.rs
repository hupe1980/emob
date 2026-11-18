//! `agentd` — the advisory plane a charging operator reads in the morning.
//!
//! Sockets and configuration. What the specialists decide is in [`agentd`], and
//! what they may decide is bounded there rather than here.

use std::net::SocketAddr;
use std::sync::Arc;

use agentd::{advisory, runtime};
use agentplane::prelude::{JournalStore, RedbStore};
use anyhow::{Context as _, Result};
use emob_service::{Identity, Principal, Readiness, Role, Server, Shutdown, identity};

/// What the daemon waits for before it takes traffic.
const JOURNAL: &str = "journal";
const PRINCIPAL: &str = "principal";

/// What `agentd` reads of the estate.
#[derive(Debug, serde::Deserialize)]
struct Estate {
    parties: emob_service::estate::Parties,
    #[serde(default)]
    operators: Vec<emob_service::estate::Operator>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,agentd=debug");

    let http: SocketAddr = emob_service::serve::bind_address("agentd", "127.0.0.1:9580")?;

    let readiness = Readiness::new().expecting(JOURNAL).expecting(PRINCIPAL);
    let shutdown = Shutdown::new();

    // In memory on purpose, and it is not `emob_service::journal`: that one
    // keeps the facts a service **decided**, and this plane decides nothing —
    // its output is a leaf nothing consumes. What `agentplane` stores is the
    // run of a specialist, which is worth keeping across a restart only where
    // several instances share it, and that is a deployment's choice.
    let store: Arc<dyn JournalStore> =
        Arc::new(RedbStore::open_in_memory().context("opening the journal")?);
    readiness.up(JOURNAL);

    // The operator this daemon acts for, and the attenuated principal its
    // specialists run under. `advisory` is the only constructor and it cannot
    // widen, and the runtime acts as it: a run whose capability is outside it
    // is refused by `agentplane` at admission (SR-024, SR-104).
    let path = emob_service::estate::path();
    let estate: Estate = emob_service::estate::load(&path)
        .with_context(|| format!("reading the estate at {}", path.display()))?;
    let operator = Principal::operator(estate.parties.operator, Role::Cpo);
    let agent = advisory(&operator).context("the operator does not hold every read capability")?;
    tracing::info!(
        party = %agent.party,
        role = %agent.role,
        capabilities = ?agent.capabilities.patterns(),
        "specialists run under an advisory principal"
    );
    let plane = runtime(Arc::clone(&store), &agent)
        .context("the advisory principal is not a delegation agentplane accepts")?;
    readiness.up(PRINCIPAL);

    tracing::info!(
        specialists = ?agentd::registered_specialists(),
        "registered"
    );
    // The runtime is held for the life of the process; the HTTP surface is what
    // an operator and an orchestrator reach.
    let _plane = plane;

    // `GET /v1/agentd`: which specialists run, and what they may do — read
    // only, behind the estate's operators.
    let operators = emob_service::estate::operator_tokens(
        &estate.operators,
        &emob_service::estate::process_env(),
    )?;
    let capabilities: Vec<String> = agent
        .capabilities
        .patterns()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let view = serde_json::json!({
        "party": agent.party.to_string(),
        "specialists": agentd::registered_specialists(),
        "capabilities": capabilities,
    });
    let view: axum::Router = axum::Router::new().route(
        "/v1/agentd",
        axum::routing::get(move || {
            let view = view.clone();
            async move { axum::Json(view) }
        }),
    );

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .with(emob_service::serve::operator_only(view, operators))
        // Nothing here holds a long-lived connection, so a short window is
        // enough for the requests in flight.
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}
