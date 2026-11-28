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
//! the Controllable System. `eebus`'s `runtime` feature is what would serve it,
//! and this workspace enables it **nowhere** — `sited` takes `eebus` only to
//! name the SPINE types that cross the socket, and `emob-eebus` takes it the
//! same way, so `cargo xtask check-graph` can hold the seam to no tokio and no
//! sockets. The SHIP session itself is the deployment's to supply.
//!
//! Southbound is **OCPP**: the profiles this site's own points are given. In a
//! deployment that is either a local controller role or a call up to `csmsd`,
//! and which of the two is a topology decision rather than a rule.
//!
//! # The edge is where the WAN is not
//!
//! In both controlled states, 120 seconds without a heartbeat triggers failsafe.
//! The controller uses local time and site configuration to derive each profile.

use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use emob_service::{Identity, Readiness, Server, Shutdown, estate, identity};
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

/// What `sited` reads of the estate.
#[derive(Debug, serde::Deserialize)]
struct Estate {
    #[serde(default)]
    operators: Vec<emob_service::estate::Operator>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,sited=debug");

    let http: SocketAddr = emob_service::serve::bind_address("sited", "127.0.0.1:9586")?;

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
    anyhow::ensure!(
        failsafe_kw >= Decimal::ZERO,
        "SITED_FAILSAFE_KW must be nonnegative"
    );
    let service = Sited::new(site, failsafe_kw, emob_eebus::MIN_FAILSAFE_RESIDENCE);
    readiness.up(INVENTORY);

    // Who may read the read model: the estate's operators, and nobody when
    // the estate names none.
    let path = estate::path();
    let estate: Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;
    let operators = estate::operator_tokens(&estate.operators, &estate::process_env())?;

    // The SHIP session itself is the socket a deployment supplies — `eebus`'s
    // `runtime`, which this build does not enable, with a node identity, a
    // trust store and the pairing the installer ran. What reaches [`Sited`] is
    // a decoded SPINE payload and an instant, which is exactly what a replay
    // two years later can be given again.
    let now = time::OffsetDateTime::now_utc();
    tracing::info!(
        state = ?service.limit_at(now).lpc_state(),
        "the site is waiting for its Energy Guard"
    );
    let service = std::sync::Arc::new(std::sync::Mutex::new(service));
    let view = axum::Router::new()
        .route(
            "/v1/sited",
            axum::routing::get(
                |axum::extract::State(service): axum::extract::State<
                    std::sync::Arc<std::sync::Mutex<Sited>>,
                >| async move {
                    let now = time::OffsetDateTime::now_utc();
                    let held = service
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    axum::Json(serde_json::json!({
                        "lpc_state": held.limit_at(now).lpc_state(),
                        "state_ceiling_kw": held.limit_at(now).stated_kw(),
                        "effective_ceiling_kw": held.allocate(&[], now).ceiling_kw(),
                    }))
                },
            ),
        )
        .with_state(service);

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .with(emob_service::serve::operator_only(view, operators))
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}
