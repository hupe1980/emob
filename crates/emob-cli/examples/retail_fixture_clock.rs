//! Test launcher for emobtest's isolated retail period-close and replay scenario.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use emob_service::{Clock, Identity, Shutdown, estate, journal, serve};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let name = args.next().context("expected empd or billd")?;
    let at = OffsetDateTime::parse(
        &args.next().context("expected a fixture instant")?,
        &Rfc3339,
    )?;
    let clock: Clock = Arc::new(move || at);
    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });
    match name.as_str() {
        "empd" => {
            let identity = Identity::new("empd", env!("CARGO_PKG_VERSION"));
            emob_service::telemetry::init(identity, "info");
            empd::daemon::run_with_clock(
                empd::daemon::Options {
                    http: serve::bind_address("empd", "127.0.0.1:9585")?,
                    journal: journal::dir().join("empd.ndjson"),
                    custody: empd::daemon::custody_directory(),
                    estate: estate::load(&estate::path())?,
                    env: estate::process_env(),
                    shutdown,
                    identity,
                },
                clock,
            )
            .await?
            .wait()
            .await
        }
        "billd" => {
            let identity = Identity::new("billd", env!("CARGO_PKG_VERSION"));
            emob_service::telemetry::init(identity, "info");
            billd::daemon::run_with_clock(
                billd::daemon::Options {
                    http: serve::bind_address("billd", "127.0.0.1:9584")?,
                    journal: journal::dir().join("billd.ndjson"),
                    estate: estate::load(&estate::path())?,
                    env: estate::process_env(),
                    shutdown,
                    identity,
                },
                clock,
            )
            .await?
            .wait()
            .await
        }
        _ => anyhow::bail!("expected empd or billd"),
    }
    Ok(())
}
