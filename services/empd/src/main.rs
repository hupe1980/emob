//! `empd` — the provider daemon.
//!
//! A socket, a clock and a key. What a token *is* and what a price is *worth*
//! are decided by [`emob_roam`] and [`emob_tariff`]; what this process adds is
//! the mapping neither of them may hold, and the three questions that need a
//! clock or a ledger — see [`empd`] for why that split is the whole design.
//!
//! # There is no loop
//!
//! Every other daemon here drains a queue or waits for a date. This one answers.
//! `[OCPI 2.3.0 §mod_tokens]` makes the provider a **responder**: an operator
//! asks whether a token may start a session and takes one of five answers back,
//! and the answer has to be right at the instant it is given rather than at the
//! instant a batch ran.
//!
//! The one thing that *is* scheduled is the month: at the turn of a period the
//! fees C-60/23 keeps apart are handed to `billd`, and they are derived from the
//! **contracts in force** rather than from the records, because the fee is owed
//! *"regardless of whether the user actually purchased electricity"*.
//!
//! # What is not here
//!
//! The socket the operator asks over, and the store behind the key. A peer is
//! somebody else's server behind a credentials exchange, exactly as `csmsd`'s
//! WebSocket and the Mobilithek subscription are, and CI cannot run it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use emob_core::PartyId;
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use empd::Empd;

/// What the daemon waits for before it takes traffic.
///
/// The token store. A provider that answers `[OCPI 2.3.0 §mod_tokens]` before
/// its own contracts are loaded answers `EXPIRED` for every driver it has, and
/// an operator that gets that answer stops the session at the point.
const CONTRACTS: &str = "contracts";

/// How often the calendar is asked for a period that has turned.
const TICK: std::time::Duration = std::time::Duration::from_secs(3600);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,empd=debug", false);

    let http: SocketAddr = std::env::var("EMPD_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9585".to_owned())
        .parse()
        .context("EMPD_HTTP_BIND is not a socket address")?;

    // The key the token store is hashed under. A deployment supplies it; a
    // default would be a store whose digests anybody holding this source can
    // recompute, which is the privacy property `TokenRef` exists for.
    let key = std::env::var("EMPD_TOKEN_KEY")
        .context("EMPD_TOKEN_KEY is unset: the token store is keyed, and a default key is a store whose digests anybody can recompute")?;

    let readiness = Readiness::new().expecting(CONTRACTS);
    let shutdown = Shutdown::new();

    // A real deployment loads contracts, tokens and the price list from its
    // store. Everything the service answers is derived from them, so answering
    // a readiness probe first is answering with an empty book.
    let party = PartyId::new("DE", "MSP").context("the provider's own party id")?;
    let service = Arc::new(Mutex::new(Empd::new(party, key.into_bytes())));
    readiness.up(CONTRACTS);

    let calendar = Arc::clone(&service);
    let stopping = shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if stopping.is_stopping() {
                return;
            }
            report_the_price_list(&calendar);
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

/// One turn of the calendar.
///
/// Split out because the only two things this daemon schedules are here: the
/// fees a period owes, and the compliance answer the price list itself gives.
fn report_the_price_list(service: &Mutex<Empd>) {
    let provider = lock(service);

    // `emob_core::ProviderProfile` takes four booleans and this derives them, so
    // an operator's compliance report is a fact about the price list rather than
    // a claim somebody entered. A provider with no price list discloses nothing,
    // and `[AFIR Art. 5(5)]` binds it from the day the Regulation applied.
    let profile = provider.provider_profile();
    let assessment =
        emob_core::obligation::assess_provider(&profile, time::OffsetDateTime::now_utc().date());
    for breach in assessment.breaches() {
        tracing::error!(
            obligation = %breach.obligation.id,
            citation = breach.obligation.citation,
            remedy = breach.obligation.remedy,
            "the price list does not meet a duty that binds this provider"
        );
    }

    // A real deployment closes the period here: `fees_for` over the month that
    // has turned, handed to `billd` as the `Subscription` on each driver's
    // document. The list is derived from the contracts in force, so a driver who
    // charged nothing is still on it.
    tracing::debug!(contracts = provider.contracts().count(), "in force");
}

/// A lock that survives a panic elsewhere.
///
/// The guarded value is a book of contracts, and a provider that answered a
/// poisoned lock by refusing every authorisation is a worse failure than the one
/// it is avoiding — the same reasoning `csmsd` states for its ledgers.
fn lock(service: &Mutex<Empd>) -> std::sync::MutexGuard<'_, Empd> {
    service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
