//! Process entry point for the customer portal.
use anyhow::Result;
use emob_service::{Shutdown, estate, identity, serve};
use portald::{Options, run};

#[tokio::main]
async fn main() -> Result<()> {
    let identity = identity!();
    emob_service::telemetry::init(identity, "info");
    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });
    run(Options {
        http: serve::bind_address("portald", "127.0.0.1:9589")?,
        estate: estate::load(&estate::path())?,
        shutdown,
        identity,
    })
    .await?
    .wait()
    .await
}
