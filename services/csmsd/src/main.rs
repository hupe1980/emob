//! `csmsd` — the CSMS a charging station connects to.
//!
//! Configuration only: the estate, the two addresses, the journal. Everything
//! that runs is [`csmsd::daemon::run`], and everything that decides money is in
//! the crates below it.
//!
//! # Two listeners, and they stop in the right order
//!
//! On `SIGTERM` the shell stops reporting ready **first**, so no further station
//! is routed here, and only then does the drain window run. Killing the process
//! instead loses nothing the journal holds — every accepted event was recorded
//! before the station heard back.

use anyhow::{Context as _, Result};
use csmsd::daemon::{Options, run};
use emob_service::{Identity, Shutdown, estate, identity, journal};

#[tokio::main]
async fn main() -> Result<()> {
    // `csmsd hash-password <ocpp-version>` — the one subcommand: reads the
    // password a station is configured with on stdin and prints the Argon2id
    // hash its `[[stations]]` entry takes as `password_hash`.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("hash-password") {
        return hash_password(args.next().as_deref());
    }

    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,csmsd=debug");

    let path = estate::path();
    let mut estate: csmsd::estate::Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;
    // `[csms]` paths are read against the estate file, not the working directory.
    estate.csms.dir = path.parent().map(ToOwned::to_owned).unwrap_or_default();

    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    let running = run(Options {
        // The default is loopback on purpose; inside a container it has to be
        // told otherwise.
        ocpp: emob_service::serve::bind_socket("CSMSD_BIND", "127.0.0.1:9000")?,
        http: emob_service::serve::bind_address("csmsd", "127.0.0.1:9001")?,
        journal: journal::dir().join("csmsd.ndjson"),
        estate,
        env: estate::process_env(),
        shutdown,
        identity: me,
        clock: emob_service::system_clock(),
    })
    .await?;
    running.wait().await;
    Ok(())
}

fn hash_password(version: Option<&str>) -> Result<()> {
    let version = match version {
        Some("1.6") => ocpp_kit::Version::V1_6,
        Some("2.0.1") => ocpp_kit::Version::V2_0_1,
        Some("2.1") => ocpp_kit::Version::V2_1,
        _ => anyhow::bail!(
            "usage: csmsd hash-password <1.6|2.0.1|2.1> < password — the station's OCPP version \
             decides how its password is written (hex AuthorizationKey for 1.6)"
        ),
    };
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let hash = csmsd::edge::hash_password(version, line.trim_end_matches(['\r', '\n']))
        .map_err(anyhow::Error::msg)?;
    println!("{hash}");
    Ok(())
}
