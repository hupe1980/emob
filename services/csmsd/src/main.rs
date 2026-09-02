//! `csmsd` — the CSMS a charging station connects to.
//!
//! Sockets and configuration. Everything that decides money is in the crates
//! below it, under test: see [`csmsd`] for why that split is the point rather
//! than a convenience.
//!
//! # Two listeners, and they stop in the right order
//!
//! A station holds a WebSocket for the length of a session, and an orchestrator
//! decides where to route one by asking an HTTP probe. So this binds two
//! sockets: the OCPP endpoint stations connect to, and the shell
//! `emob-service` provides — `/health/live`, `/health/ready`, `/about`.
//!
//! On `SIGTERM` the shell stops reporting ready **first**, so no further station
//! is routed here, and only then does the drain window run. Killing the process
//! instead does not lose a request — it loses the `StopTransaction` that carries
//! the signed meter record, and that session becomes a kilowatt-hour nobody can
//! bill.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use csmsd::{ChargePoint, Csmsd, Provisioning};
use emob_core::{Currency, PartyId};
use emob_eichrecht::KeyRegistry;
use emob_service::{Identity, Readiness, Server, Shutdown, identity};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocpp_kit::Version;
use ocpp_kit::transport::{Auth, AuthOutcome, Csms, SessionContext, SessionEvent};
use ocpp_kit::types::Identity as StationIdentity;
use rust_decimal::Decimal;

/// The two bindings a station may not supply for itself, and the readiness
/// probes that name them.
///
/// Declared rather than discovered, so the health surface says what the daemon
/// is waiting *for* while it waits — which is the one moment an operator needs
/// it. A `csmsd` whose key registry has not loaded refuses every session that
/// arrives, and that looks like a fleet fault until the probe says otherwise.
const PROVISIONING: &str = "provisioning";
const KEY_REGISTRY: &str = "key-registry";
const TARIFF: &str = "tariff";

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,csmsd=debug", false);

    let ocpp: SocketAddr = std::env::var("CSMSD_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9000".to_owned())
        .parse()
        .context("CSMSD_BIND is not a socket address")?;
    let http: SocketAddr = std::env::var("CSMSD_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9001".to_owned())
        .parse()
        .context("CSMSD_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new()
        .expecting(PROVISIONING)
        .expecting(KEY_REGISTRY)
        .expecting(TARIFF);
    let shutdown = Shutdown::new();

    // A real deployment loads all four of these from its own store. They are
    // deliberately arguments rather than lookups: the provisioning and the key
    // registry are the two bindings a station may not supply for itself.
    let provisioning = Provisioning::new().with(
        StationIdentity::new("CP-1").context("a valid station identity")?,
        ChargePoint {
            evse_id: "DE*ABC*E00001".parse()?,
            rated_power_kw: Decimal::from(150),
        },
    );
    readiness.up(PROVISIONING);

    let party = PartyId::new("DE", "ABC")?;
    let registry = KeyRegistry::new();
    readiness.up(KEY_REGISTRY);

    let tariff = Tariff::simple(
        "ad-hoc".parse()?,
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(
            Dimension::Energy,
            Decimal::from_str_exact("0.49")?,
        )],
    );
    readiness.up(TARIFF);

    let handler = Arc::new(Csmsd::new(party, registry, tariff));
    let csms = Csms::builder()
        .bind(ocpp)
        .versions([Version::V2_1, Version::V2_0_1, Version::V1_6])
        .authenticate(move |auth: Auth| {
            // An identity nobody provisioned is answered 404 rather than 401,
            // so an operator can tell a typo from a bad password
            // `[OCPP 2.0.1 Part 4 §3.1.1]`.
            // The station's own charge point travels with the session from
            // here, so no later lookup can attribute a session to a point
            // nobody provisioned.
            let point = provisioning.get(&auth.identity).cloned();
            async move {
                match point {
                    Some(point) => AuthOutcome::Accept(SessionContext::new(point)),
                    None => AuthOutcome::Unknown,
                }
            }
        })
        .handler(SharedCsmsd(Arc::clone(&handler)))
        .build()?;

    let mut events = csms.handle().events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if let SessionEvent::Opened {
                identity, version, ..
            } = event
            {
                // `de.emob.station.booted`, once there is a bus to put it on.
                tracing::info!(station = %identity, ocpp = %version, "station connected");
            }
        }
    });

    // The orchestrator's signal reaches both listeners through one token.
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    let shell = Server::new(me, readiness, shutdown.clone())
        // A station's transaction can outlive any window worth waiting, so the
        // drain is for the requests in flight rather than for the sessions:
        // long enough for an in-progress `StopTransaction` to land, and no
        // longer.
        .draining_for(std::time::Duration::from_secs(15));
    let shell = tokio::spawn(async move { shell.listen(http).await });

    tracing::info!(%ocpp, %http, "csmsd listening");
    let served = csms.serve();
    tokio::select! {
        result = served => result?,
        () = shutdown.wait() => tracing::info!("stopping"),
    }
    shell.abort();
    Ok(())
}

/// The handler behind an `Arc`, so the outcome log outlives the server.
struct SharedCsmsd(Arc<Csmsd>);

impl ocpp_kit::transport::Handler for SharedCsmsd {
    fn on_request(
        &self,
        ctx: ocpp_kit::transport::Ctx,
        request: ocpp_kit::engine::IncomingRequest,
    ) -> ocpp_kit::transport::BoxFuture<'_, Result<Box<ocpp_kit::RawValue>, ocpp_kit::rpc::CallError>>
    {
        self.0.on_request(ctx, request)
    }
}
