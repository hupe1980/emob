//! `sited` — the daemon that keeps a site lawful under § 14a.
//!
//! Sockets, a clock and a cadence. Every rule is in [`sited`], `emob-eebus`,
//! `emob-smart` and `emob-ocpp`; what this process adds is the SHIP session a
//! setpoint arrives over, the timer that notices a silence, and the OCPP call
//! that installs a profile on a station.
//!
//! # Two sockets, and they face opposite ways
//!
//! Northbound is **EEBUS**: SHIP over TLS 1.2, mDNS discovery, and a SPINE
//! engine underneath. The grid operator's control box is the client and this is
//! the Controllable System. `eebus`'s `runtime` feature is what serves it, and
//! it is enabled **here and nowhere else** — `emob-eebus` takes the same crate
//! without it, so `cargo xtask check-graph` can hold the seam to no tokio and
//! no sockets.
//!
//! Southbound is **OCPP**: the profiles this site's own points are given. In a
//! deployment that is either a local controller role or a call up to `csmsd`,
//! and which of the two is a topology decision rather than a rule.
//!
//! # The edge is where the WAN is not
//!
//! `[LPC-912]` puts a site into its failsafe when the Energy Guard goes quiet
//! and `[LPC-021]` floors that timer at two hours, so a site whose controller
//! sits behind a broken WAN link is held at 4.2 kW because this process could
//! not be reached — not because a grid operator asked. That is why this daemon
//! is designed to run at the site, and why the readiness probe waits on the
//! **site inventory** rather than on anything upstream: a controller that
//! cannot say which points it controls must not take traffic.

use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use rust_decimal::Decimal;
use sited::Sited;

/// What the daemon waits for before it takes traffic.
const INVENTORY: &str = "site-inventory";

/// `[LPC-901/1]`'s failsafe consumption active power limit, in kW.
///
/// `[BK6-22-300 Ziff. 4.5]`'s Mindestleistung, which is the figure a site with
/// no other instruction is entitled to draw — and the default a deployment
/// overrides with whatever its own connection contract says.
const FAILSAFE_KW: &str = "4.2";

/// `[LPC-021]`'s floor under the Failsafe Duration Minimum.
///
/// The Energy Guard writes the real period over
/// `deviceConfigurationKeyValue`; two hours is the shortest the specification
/// admits, which is the reading that keeps a site under control for longer
/// rather than shorter.
const FAILSAFE_AFTER: time::Duration = time::Duration::hours(2);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,sited=debug", false);

    let http: SocketAddr = std::env::var("SITED_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9584".to_owned())
        .parse()
        .context("SITED_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new().expecting(INVENTORY);
    let shutdown = Shutdown::new();

    // A real deployment loads the site from the register: which points are
    // steuerbare Verbrauchseinrichtungen `[BK6-22-300 Ziff. 2.4.1]`, and what
    // the connection carries. Both are `emob-smart`'s to judge and neither is
    // this process's to invent.
    let site = emob_smart::Site {
        connection_kw: Decimal::ZERO,
        devices: Vec::new(),
    };
    let failsafe_kw = std::env::var("SITED_FAILSAFE_KW")
        .unwrap_or_else(|_| FAILSAFE_KW.to_owned())
        .parse::<Decimal>()
        .context("SITED_FAILSAFE_KW is not a decimal number of kilowatts")?;
    let service = Sited::new(site, failsafe_kw, FAILSAFE_AFTER);
    readiness.up(INVENTORY);

    // The SHIP session itself is the socket a deployment supplies: `eebus`'s
    // `runtime` with a node identity, a trust store and the pairing the
    // installer ran. What reaches [`Sited`] is a decoded SPINE payload and an
    // instant, which is exactly what a replay two years later can be given
    // again.
    let now = time::OffsetDateTime::now_utc();
    tracing::info!(
        state = ?service.limit_at(now).lpc_state(),
        "the site is waiting for its Energy Guard"
    );
    let _ = &service;

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}
