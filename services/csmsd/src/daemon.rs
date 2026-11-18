//! `csmsd`, started: two sockets, the outbox loop, and the readiness that says
//! what it waits for.
//!
//! `main.rs` reads the environment and calls [`run`]; a test calls [`run`] with
//! port `0`, a journal in a temporary directory and its own [`Env`], and gets
//! the real daemon.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use emob_service::estate::{self, Env};
use emob_service::head::{self, Signing, SigningKey};
use emob_service::health::Probe;
use emob_service::journal::{File, Journal as _};
use emob_service::vault::{DataKey, Sealing};
use emob_service::{Identity, Readiness, Server, Shutdown};
use ocpp_kit::Version;
use ocpp_kit::transport::Csms;

use crate::authorize::Empd;
use crate::edge::{Edge, Gate, Guarded, Security};
use crate::estate::Estate;
use crate::outbox::Dispatcher;
use crate::{Csmsd, Shared};

/// The probes `/health/ready` names while it waits.
pub const PROVISIONING: &str = "provisioning";
/// The key registry: empty is not ready, because every session would be
/// refused (SR-005).
pub const KEY_REGISTRY: &str = "key-registry";

/// What a `csmsd` is started with.
pub struct Options {
    /// The OCPP WebSocket address.
    pub ocpp: SocketAddr,
    /// The shell's HTTP address.
    pub http: SocketAddr,
    /// The journal file.
    pub journal: PathBuf,
    /// The estate's sections `csmsd` reads.
    pub estate: Estate,
    /// Where secrets are looked up.
    pub env: Env,
    /// The stop signal.
    pub shutdown: Shutdown,
    /// Who this is.
    pub identity: Identity,
    /// What time it is — what the facts that end an attempt are dated by.
    pub clock: emob_service::Clock,
}

/// The variable holding the data key csmsd's journal is sealed under
/// (SR-048): 32 random bytes, base64.
pub const DATA_KEY: &str = "CSMSD_DATA_KEY";

/// csmsd's journal: every record body sealed under [`DATA_KEY`], because its
/// answers, CDRs and delivery bodies all carry the card's UID (SR-048), and
/// its head signed under `CSMSD_SIGNING_KEY` into `<journal>.head`, so a
/// journal cut at its end does not start (SR-096).
pub type Journal = Signing<Sealing<File>>;

/// A `csmsd` that is running.
pub struct Running {
    /// Where stations connect.
    pub ocpp: SocketAddr,
    /// Where the shell answers.
    pub http: SocketAddr,
    /// The CSMS, for a caller that reads its state.
    pub csmsd: Arc<Csmsd<Journal>>,
    /// The readiness the shell reports.
    pub readiness: Readiness,
    /// The station edge: what it refused, counted.
    pub edge: Arc<Edge>,
    shutdown: Shutdown,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Stop as a crash would: every task aborted, nothing drained.
    pub fn kill(self) {
        for task in &self.tasks {
            task.abort();
        }
    }

    /// Wait until the shutdown signal, then stop.
    pub async fn wait(self) {
        self.shutdown.wait().await;
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// Start `csmsd`.
///
/// # Errors
///
/// The estate refuses (a key named), a secret is unset, the journal does not
/// fold, or a socket cannot be bound.
pub async fn run(options: Options) -> Result<Running> {
    let Options {
        ocpp,
        http,
        journal,
        estate,
        env,
        shutdown,
        identity,
        clock,
    } = options;

    let readiness = Readiness::new()
        .expecting(PROVISIONING)
        .expecting(KEY_REGISTRY);

    let provisioning = crate::estate::provisioning(&estate)?;
    let registry = crate::estate::registry(&estate.meter_keys)?;
    let zone = crate::estate::zone(&estate)?;
    let operators = estate::operator_tokens(&estate.operators, &env)?;
    let secret = estate::webhook_secret(&estate.services, &env)?;
    let service_token = estate
        .services
        .service_token_env
        .resolve_in("services.service_token_env", &env)?;

    if provisioning.is_empty() {
        readiness.set(
            PROVISIONING,
            Probe::down("the estate lists no stations, so every connection would be answered 404"),
        );
    } else {
        readiness.up(PROVISIONING);
    }
    if registry.is_empty() {
        readiness.set(
            KEY_REGISTRY,
            Probe::down(
                "the key registry is empty: every session would be refused, because no signed \
                 value can be verified without a key provisioned out of band",
            ),
        );
    } else {
        readiness.up(KEY_REGISTRY);
    }

    let authority = Arc::new(Empd::new(
        &estate.services.empd,
        service_token,
        Duration::from_millis(estate.services.authorize_timeout_ms),
    )?);
    let key = Arc::new(DataKey::from_env(DATA_KEY, &env)?);
    let signing = SigningKey::for_daemon("csmsd", &env)?;
    let file = Signing::open(
        Sealing::new(
            File::open(&journal).context("opening csmsd's journal")?,
            key,
            "csmsd/journal",
        ),
        signing,
        head::beside(&journal),
    )
    .context("opening csmsd's journal against its signed head")?;
    let latest = file.latest();
    let csmsd = Arc::new(
        Csmsd::open(
            estate.parties.operator.clone(),
            registry,
            authority,
            file,
            zone,
        )
        .context("folding csmsd's journal")?
        .with_clock(clock)
        .issuing_receipts(
            crate::receipt::Receipts::open(
                journal
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
                &estate.receipts.public_url,
            )
            .context("opening the receipt key beside csmsd's journal")?,
        ),
    );
    let sites = crate::receipt::sites(&estate.stations);
    tracing::info!(
        settled = csmsd.settled(),
        pending = csmsd.with_state(|state| state.pending().count()),
        head = csmsd.with_journal(|journal| journal.head().map(|h| h.position).unwrap_or_default()),
        "the ledger is what the journal says it is"
    );

    let stations: Vec<(String, String)> = estate
        .stations
        .iter()
        .map(|station| (station.identity.clone(), station.evse.to_string()))
        .collect();
    let mut versions: Vec<Version> = Vec::new();
    for station in &estate.stations {
        let version = crate::estate::version(station.ocpp);
        if !versions.contains(&version) {
            versions.push(version);
        }
    }
    // Spec 005: the credential each station's profile requires, TLS or a
    // reason there is none, and the limits — all in `edge`.
    let security = Arc::new(Security::from_estate(&estate.csms, &estate.stations, ocpp)?);
    let edge = Arc::new(Edge::new(&estate.csms));
    let mut builder = Csms::builder()
        .bind(ocpp)
        .versions(versions)
        .max_connections(estate.csms.max_connections)
        .handshake_timeout(Duration::from_secs(10))
        .authenticate(Gate::new(provisioning, Arc::clone(&security)))
        .handler(Guarded::new(Shared(Arc::clone(&csmsd)), Arc::clone(&edge)));
    if let Some(tls) = security.tls() {
        builder = builder.tls(tls);
    }
    let csms = builder.build()?;
    // Spec 011: the stations that show their receipt link on their display.
    csmsd.showing_receipts(
        crate::display::Display::from_estate(&estate.stations)?,
        csms.handle(),
    );

    let mut tasks = Vec::new();
    let (watcher, stopping) = (Arc::clone(&edge).watch(csms.handle()), shutdown.clone());
    tasks.push(tokio::spawn(async move {
        tokio::select! {
            () = watcher => {}
            () = stopping.wait() => {}
        }
    }));
    let (sweeper, handle, stopping) = (Arc::clone(&edge), csms.handle(), shutdown.clone());
    tasks.push(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(sweeper.sweep_every());
        loop {
            tokio::select! {
                _ = ticker.tick() => sweeper.sweep(&handle).await,
                () = stopping.wait() => return,
            }
        }
    }));

    let ocpp_listener = tokio::net::TcpListener::bind(ocpp)
        .await
        .with_context(|| format!("binding the OCPP socket {ocpp}"))?;
    let ocpp_addr = ocpp_listener.local_addr()?;
    let stopping = shutdown.clone();
    tasks.push(tokio::spawn(async move {
        tokio::select! {
            result = csms.serve_on(ocpp_listener) => {
                if let Err(error) = result {
                    tracing::error!(%error, "the OCPP socket stopped");
                }
            }
            () = stopping.wait() => {}
        }
    }));

    let dispatcher = Dispatcher::new(
        &estate.services.billd,
        &estate.services.roamd,
        secret,
        Duration::from_secs(10),
    )?;
    let retry = Duration::from_secs(estate.services.retry_secs.max(1));
    let outbox = Arc::clone(&csmsd);
    let stopping = shutdown.clone();
    tasks.push(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(retry);
        loop {
            tokio::select! {
                _ = ticker.tick() => dispatcher.drain(&outbox).await,
                () = stopping.wait() => return,
            }
        }
    }));

    let http_listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("binding the shell {http}"))?;
    let http_addr = http_listener.local_addr()?;
    let shell = Server::new(identity, readiness.clone(), shutdown.clone())
        .with(emob_service::serve::operator_only(
            head::router("csmsd", latest),
            operators.clone(),
        ))
        .with(crate::read::router(Arc::clone(&csmsd), operators, stations))
        .with(crate::receipt::router(Arc::clone(&csmsd), sites))
        // Long enough for an in-progress `StopTransaction` to land.
        .draining_for(Duration::from_secs(15));
    tasks.push(tokio::spawn(async move {
        if let Err(error) = shell.serve_on(http_listener).await {
            tracing::error!(%error, "the shell stopped");
        }
    }));

    tracing::info!(%ocpp_addr, %http_addr, "csmsd listening");
    Ok(Running {
        ocpp: ocpp_addr,
        http: http_addr,
        csmsd,
        readiness,
        edge,
        shutdown,
        tasks,
    })
}
