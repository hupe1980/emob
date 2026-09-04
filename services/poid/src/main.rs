//! `poid` — the daemon that publishes an operator's charge points.
//!
//! Sockets, a clock and a cadence. The documents are [`emob_poi`]'s and the
//! decisions about what may be said are its; what this process adds is *when* a
//! snapshot goes out and *whether the access point took it* — see [`poid`].
//!
//! # Two rhythms, not one
//!
//! `[AFIR Art. 20(2)]` splits the duty in two, and so does the profile: the
//! **table** is the estate as it stands and goes out on a cadence, while a
//! **status** message is a change and goes out when there is one. They are not
//! interchangeable — a status message references a facility at the version the
//! table published it at, so a status that outran its own table is a document
//! every consumer drops without a word.

use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use emob_poi::datex::Publisher;
use emob_poi::site::Facility;
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use poid::Poid;

/// What the daemon waits for before it takes traffic.
const INVENTORY: &str = "inventory";

/// How often the estate is republished, and how long a feed may go unrefreshed
/// before an operator is told.
///
/// A feed nobody refreshed is one route planners read as current, and nothing
/// about it errors.
const CADENCE: time::Duration = time::Duration::hours(1);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,poid=debug", false);

    let http: SocketAddr = std::env::var("POID_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9582".to_owned())
        .parse()
        .context("POID_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new().expecting(INVENTORY);
    let shutdown = Shutdown::new();

    // A real deployment loads the estate from the register and the prices from
    // what `tarifd` published. Both are arguments here for the reason every
    // instant in this workspace is one: a snapshot replayed two years later has
    // to produce the same bytes.
    let no_rates = |_: &emob_poi::site::ChargingPoint| None;
    let service = Poid::new(
        Publisher {
            country: "DE".to_owned(),
            national_identifier: std::env::var("POID_NAP_IDENTIFIER")
                .unwrap_or_else(|_| "DE-NAP-0000".to_owned()),
            language: "de".to_owned(),
        },
        Facility::new("TABLE-1"),
        Vec::new(),
        &no_rates,
    );
    readiness.up(INVENTORY);

    let now = time::OffsetDateTime::now_utc();
    if let Some(stale) = service.stale(now, CADENCE) {
        tracing::warn!(feed = %stale, "the national access point is behind");
    }
    // The push itself is a socket the deployment supplies: `snapshotPush` to
    // the Mobilithek, with the operator's credentials. Only a push that came
    // back accepted is recorded, which is why `accepted` is a separate call.
    let _ = &service;

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}
