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
//! [`roamd::Verdict`]. The inbound router is mounted at `/ocpi`, behind the
//! reach-back guard of [`roamd::reach`].
//!
//! This file is configuration only; everything that runs is
//! [`roamd::daemon::run`].

use anyhow::{Context as _, Result};
use emob_service::{Identity, Shutdown, estate, identity, journal};
use roamd::daemon::{Options, run};

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,roamd=debug");

    let path = estate::path();
    let estate: roamd::daemon::Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;

    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    let running = run(Options {
        http: emob_service::serve::bind_address("roamd", "127.0.0.1:9583")?,
        journal: journal::dir().join("roamd.ndjson"),
        estate,
        env: estate::process_env(),
        shutdown,
        identity: me,
    })
    .await?;
    running.wait().await;
    Ok(())
}
