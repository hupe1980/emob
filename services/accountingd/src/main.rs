//! Start the standalone receivables owner from the estate configuration.
use accountingd::{Options, run};
use anyhow::Result;
use emob_service::{Shutdown, estate, identity, journal, serve};

#[tokio::main]
async fn main() -> Result<()> {
    let identity = identity!();
    emob_service::telemetry::init(identity, "info");
    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });
    run(Options {
        http: serve::bind_address("accountingd", "127.0.0.1:9591")?,
        journal: journal::dir().join("accountingd.ndjson"),
        estate: estate::load(&estate::path())?,
        env: estate::process_env(),
        shutdown,
        identity,
    })
    .await?
    .wait()
    .await
}
