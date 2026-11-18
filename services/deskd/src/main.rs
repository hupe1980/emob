//! `deskd` — the operator's page.
//!
//! Configuration only: the estate and the address. Everything that runs is
//! [`deskd::run`].

use anyhow::{Context as _, Result};
use deskd::{Options, run};
use emob_service::{Identity, Shutdown, estate, identity};

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,deskd=debug");

    let path = estate::path();
    let estate: deskd::Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;

    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    let running = run(Options {
        // Loopback unless told otherwise: Basic sends the operator token with
        // every request (SR-050).
        http: emob_service::serve::bind_address("deskd", "127.0.0.1:9587")?,
        estate,
        env: estate::process_env(),
        shutdown,
        identity: me,
        clock: deskd::system_clock(),
        // On deskd's own volume, never one of the daemons' it watches.
        witness: emob_service::journal::dir().join("witness.ndjson"),
    })
    .await?;
    running.wait().await;
    Ok(())
}
