//! `csmsd` — the CSMS a charging station connects to.
//!
//! Sockets and configuration. Everything that decides money is in the crates
//! below it, under test: see [`csmsd`] for why that split is the point rather
//! than a convenience.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use csmsd::{ChargePoint, Csmsd, Provisioning};
use emob_core::{Currency, PartyId};
use emob_eichrecht::KeyRegistry;
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocpp_kit::Version;
use ocpp_kit::transport::{Auth, AuthOutcome, Csms, SessionContext, SessionEvent};
use ocpp_kit::types::Identity;
use rust_decimal::Decimal;

#[tokio::main]
async fn main() -> Result<()> {
    let addr: SocketAddr = std::env::var("CSMSD_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9000".to_owned())
        .parse()
        .context("CSMSD_BIND is not a socket address")?;

    // A real deployment loads all four of these from its own store. They are
    // deliberately arguments rather than lookups: the provisioning and the key
    // registry are the two bindings a station may not supply for itself.
    let provisioning = Provisioning::new().with(
        Identity::new("CP-1").context("a valid station identity")?,
        ChargePoint {
            evse_id: "DE*ABC*E00001".parse()?,
            rated_power_kw: Decimal::from(150),
        },
    );
    let party = PartyId::new("DE", "ABC")?;
    let registry = KeyRegistry::new();
    let tariff = Tariff::simple(
        "ad-hoc".parse()?,
        Currency::EUR,
        TariffKind::AdHoc,
        vec![PriceComponent::new(
            Dimension::Energy,
            Decimal::from_str_exact("0.49")?,
        )],
    );

    let handler = Arc::new(Csmsd::new(party, registry, tariff));
    let csms = Csms::builder()
        .bind(addr)
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
                println!("+ {identity} speaks OCPP {version}");
            }
        }
    });

    println!("csmsd listening on {addr}");
    csms.serve().await?;
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
