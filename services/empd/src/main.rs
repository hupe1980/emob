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
//! # What is here now
//!
//! `POST /v1/authorize`, which `csmsd` asks before a transaction opens, and the
//! operator's `GET /v1/empd` — both in [`empd::http`], started by
//! [`empd::daemon::run`]. This file is configuration only.

use anyhow::{Context as _, Result};
use emob_service::{Identity, Shutdown, estate, identity, journal};
use empd::daemon::{Options, run};

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,empd=debug");

    let path = estate::path();
    let estate: empd::daemon::Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;

    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    let running = run(Options {
        http: emob_service::serve::bind_address("empd", "127.0.0.1:9585")?,
        journal: journal::dir().join("empd.ndjson"),
        estate,
        env: estate::process_env(),
        shutdown,
        identity: me,
    })
    .await?;
    running.wait().await;
    Ok(())
}
