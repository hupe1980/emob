//! Starts the provider with its estate, signed journal and current customer key store.

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
        custody: empd::daemon::custody_directory(),
        estate,
        env: estate::process_env(),
        shutdown,
        identity: me,
    })
    .await?;
    running.wait().await;
    Ok(())
}
