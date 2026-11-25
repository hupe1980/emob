//! `roamd`, started: the node folded, the estate's partners offered their
//! `Token A`, the inbound OCPI router mounted behind the reach-back guard, the
//! receiver of `csmsd`'s deliveries, and the drain loop that pushes.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use emob_core::PartyId;
use emob_poi::site::{Address, ChargingPoint, Connector, Coordinates, Facility, Site};
use emob_roam::{Partner as RoamPartner, PartnerRegistry, Wire};
use emob_service::estate::{
    Env, EstateError, Operator, Parties, Partner, Roaming, Services, Station,
};
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::{File, Journal as _};
use emob_service::vault::{DataKey, Sealing};
use emob_service::{Actor, Identity, Readiness, Server, Shutdown};
use ocpi_kit::client::ClientConfig;
use ocpi_kit::types::{CiString, OcpiString, Url as KitUrl, UrlPolicy};
use ocpi_kit::v2_3_0::credentials::{Credentials, CredentialsRole};
use serde::Deserialize;

use crate::reach::ReachBack;
use crate::{Answer, Dispatcher, Handshake, Node, Roamd};

/// The probe `/health/ready` names while it waits.
pub const PARTNERS: &str = "partners";

/// The variable holding the key credentials tokens are digested and sealed
/// under (SR-044): 32 random bytes, base64.
pub const DATA_KEY: &str = "ROAMD_DATA_KEY";

/// The node, folded from its sealed journal under `ROAMD_DATA_KEY`, its head
/// signed under `ROAMD_SIGNING_KEY` — a journal shorter than, diverged from or
/// written past its signed head does not start (SR-096) — and each estate
/// partner offered its `Token A`.
///
/// # Errors
///
/// An unset key or `Token A`, a journal that cannot be read, one that disagrees with its
/// signed head, one that does not fold.
pub fn open(estate: &Estate, journal: &std::path::Path, env: &Env) -> Result<Node<Journal>> {
    let mut offers = Vec::new();
    for (index, partner) in estate.partners.iter().enumerate() {
        let token = partner
            .token_a_env
            .resolve_in(&format!("partners[{index}].token_a_env"), env)?;
        offers.push((partner.party.to_string(), token));
    }
    let key = Arc::new(DataKey::from_env(DATA_KEY, env)?);
    let signing = SigningKey::for_daemon("roamd", env)?;
    let file = Signing::open(
        Sealing::new(
            File::open(journal).context("opening roamd's journal")?,
            Arc::clone(&key),
            "roamd/journal",
        ),
        signing,
        head::beside(journal),
    )
    .context("checking roamd's journal against its signed head")?;
    let mut node =
        Node::open(Roamd::new(registry(estate)), file, key).context("folding roamd's journal")?;
    // Each estate partner is handed its `Token A` once; a partner that has
    // registered since holds a token of its own.
    for (peer, token) in offers {
        if !node.state().peers().has_offer_for(&peer) {
            node.offer(&Actor::Estate, &peer, token.expose())?;
        }
    }
    Ok(node)
}

/// roamd's journal: every record body sealed under [`DATA_KEY`] — its
/// consignments carry the card's UID a partner's CDR needs (SR-048), and its
/// registrations the partners' tokens (SR-044) — and its head signed beside it.
pub type Journal = Signing<Sealing<File>>;

/// What `roamd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// The operator.
    pub parties: Parties,
    /// Where the stations stand, for the CDR's location.
    pub stations: Vec<Station>,
    /// Whom records go to.
    #[serde(default)]
    pub partners: Vec<Partner>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The webhook secret and the retry interval.
    pub services: Services,
    /// This node's face to its partners.
    pub roaming: Roaming,
}

/// A station's location as a CDR carries it.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `stations[index].location`.
pub fn location(
    index: usize,
    station: &Station,
) -> Result<ocpi_kit::v2_3_0::cdrs::CdrLocation, EstateError> {
    point_location(&format!("stations[{index}]"), &Point::of(station))
}

/// What a CDR's location is built from: the point and its place — what the
/// estate holds for a station, and what `de.emob.cdr.issued` carries (spec 021).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Point {
    /// The EVSE id.
    pub evse: emob_core::EvseId,
    /// The connector.
    pub connector: emob_core::ConnectorType,
    /// Rated power, kW.
    pub rated_power_kw: rust_decimal::Decimal,
    /// The place.
    pub location: emob_service::estate::Location,
}

impl Point {
    /// The point an estate station is.
    #[must_use]
    pub fn of(station: &Station) -> Self {
        Self {
            evse: station.evse.clone(),
            connector: station.connector,
            rated_power_kw: station.rated_power_kw,
            location: station.location.clone(),
        }
    }
}

/// A point's location as a CDR carries it; `key` prefixes the field names a
/// refusal gives.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `<key>.location.<field>`.
pub fn point_location(
    key: &str,
    station: &Point,
) -> Result<ocpi_kit::v2_3_0::cdrs::CdrLocation, EstateError> {
    let at = |field: &str| format!("{key}.location.{field}");
    let place = &station.location;
    let site = Site {
        facility: Facility::new(&place.id),
        name: place.name.clone(),
        coordinates: Coordinates {
            latitude: place.latitude,
            longitude: place.longitude,
        },
        address: Address {
            street: place.street.clone(),
            house_number: place.house_number.clone(),
            postcode: place.postal_code.clone(),
            city: place.city.clone(),
            country_code: place.country.clone(),
        },
        time_zone: emob_core::TimeZone::new(&place.time_zone).map_err(|error| {
            EstateError::Invalid {
                key: at("time_zone"),
                reason: error.to_string(),
            }
        })?,
        stations: Vec::new(),
    };
    let point = ChargingPoint::new(
        Facility::new(station.evse.to_string()),
        station.evse.clone(),
        Connector::new(station.connector, station.rated_power_kw),
    );
    emob_roam::ocpi::location::cdr_location(&site, &point, &point.connectors[0], 0).map_err(
        |error| EstateError::Invalid {
            key: format!("{key}.location"),
            reason: error.to_string(),
        },
    )
}

/// The partner registry the estate describes.
#[must_use]
pub fn registry(estate: &Estate) -> PartnerRegistry {
    estate.partners.iter().fold(
        PartnerRegistry::new(estate.parties.operator.clone()),
        |registry, partner| {
            let mut entry = RoamPartner::emsp(partner.party.clone());
            if partner.signed_data {
                entry = entry.on_signed_data();
            }
            if let Some(days) = partner.settles_within_days {
                entry = entry.settling_within(time::Duration::days(days));
            }
            registry.with(entry)
        },
    )
}

/// This node's own credentials, as it hands them to a registrant.
///
/// # Errors
///
/// A party id OCPI cannot carry.
pub fn credentials(operator: &PartyId, public_url: &str) -> Result<Credentials> {
    Ok(Credentials::builder()
        .token(OcpiString::<64>::new("replaced-when-answering").map_err(anyhow::Error::msg)?)
        .url(KitUrl::new_lenient(format!(
            "{}/versions",
            public_url.trim_end_matches('/')
        )))
        .roles(vec![
            CredentialsRole::builder()
                .role(ocpi_kit::v2_3_0::types::Role::Cpo)
                .business_details(
                    ocpi_kit::v2_3_0::locations::BusinessDetails::builder()
                        .name(operator.to_string())
                        .build(),
                )
                .party_id(CiString::new(operator.party_id()).map_err(anyhow::Error::msg)?)
                .country_code(CiString::new(operator.country_code()).map_err(anyhow::Error::msg)?)
                .build(),
        ])
        .build())
}

/// A credentials token nobody can guess: 32 random bytes, hex.
fn mint() -> crate::inbound::MintToken {
    Arc::new(|_peer: &str| {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).expect("the operating system has a random source");
        hex::encode(bytes)
    })
}

/// The kit's own URL policy, opened only as far as the estate's allowlist.
fn client_config(roaming: &Roaming) -> ClientConfig {
    if roaming.reach_back_allow.is_empty() {
        return ClientConfig::default();
    }
    ClientConfig::default().with_url_policy(UrlPolicy {
        allowed_schemes: vec!["https".to_owned(), "http".to_owned()],
        allow_private_networks: true,
        ..UrlPolicy::default()
    })
}

/// What a `roamd` is started with.
pub struct Options {
    /// The HTTP address (shell, receiver, read model and `/ocpi`).
    pub http: SocketAddr,
    /// The journal file.
    pub journal: PathBuf,
    /// The estate.
    pub estate: Estate,
    /// Where secrets are looked up.
    pub env: Env,
    /// The stop signal.
    pub shutdown: Shutdown,
    /// Who this is.
    pub identity: Identity,
}

/// A `roamd` that is running.
pub struct Running {
    /// Where it answers.
    pub http: SocketAddr,
    /// The node.
    pub node: Arc<Mutex<Node<Journal>>>,
    shutdown: Shutdown,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Stop as a crash would.
    pub async fn kill(self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
    }

    /// Wait for the shutdown signal and the drain.
    pub async fn wait(self) {
        self.shutdown.wait().await;
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Start `roamd`.
///
/// # Errors
///
/// An unset secret, a station location the estate cannot describe, a journal
/// that does not fold, an address that cannot be bound.
pub async fn run(options: Options) -> Result<Running> {
    let Options {
        http,
        journal,
        estate,
        env,
        shutdown,
        identity,
    } = options;
    let secret = emob_service::estate::webhook_secret(&estate.services, &env)?;
    let operators = emob_service::estate::operator_tokens(&estate.operators, &env)?;
    let mut locations = BTreeMap::new();
    for (index, station) in estate.stations.iter().enumerate() {
        locations.insert(station.identity.clone(), location(index, station)?);
    }

    let readiness = Readiness::new().expecting(PARTNERS);
    let node = open(&estate, &journal, &env)?;
    let latest = node.journal().latest();
    tracing::info!(
        position = node
            .journal()
            .head()
            .map(|head| head.position)
            .unwrap_or_default(),
        registered = node.state().peers().registered().count(),
        "the consignment ledger is what the journal says it is"
    );
    readiness.up(PARTNERS);
    let node = Arc::new(Mutex::new(node));

    let reach = ReachBack::allowing(estate.roaming.reach_back_allow.clone());
    let config = client_config(&estate.roaming);
    let base = KitUrl::new_lenient(estate.roaming.public_url.trim_end_matches('/').to_owned());
    let handshake = Handshake::new(
        credentials(&estate.parties.operator, &estate.roaming.public_url)?,
        Arc::clone(&node),
        mint(),
        config.clone(),
        reach.clone(),
    )
    .map_err(anyhow::Error::msg)?;
    let ocpi = crate::inbound::router(
        base,
        "/ocpi",
        Arc::clone(&node),
        crate::peering::Registry::new(Arc::clone(&node)),
        handshake,
    );
    let routes = crate::receive::router(
        crate::receive::Shared {
            node: Arc::clone(&node),
            secret,
            locations: Arc::new(locations),
            refused: Arc::new(AtomicU64::new(0)),
        },
        operators.clone(),
    )
    .merge(emob_service::serve::operator_only(
        head::router("roamd", latest),
        operators,
    ))
    .merge(ocpi);

    let dispatcher =
        Dispatcher::new(&estate.parties.operator, config).context("building the OCPI client")?;
    let mut tasks = Vec::new();
    let queue = Arc::clone(&node);
    let stopping = shutdown.clone();
    let every = std::time::Duration::from_secs(estate.services.retry_secs.max(1));
    tasks.push(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        loop {
            tokio::select! {
                _ = ticker.tick() => drain(&queue, &dispatcher, &reach).await,
                () = stopping.wait() => return,
            }
        }
    }));

    let listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("binding {http}"))?;
    let addr = listener.local_addr()?;
    let server = Server::new(identity, readiness, shutdown.clone())
        .with(routes)
        .draining_for(std::time::Duration::from_secs(3));
    tasks.push(tokio::spawn(async move {
        if let Err(error) = server.serve_on(listener).await {
            tracing::error!(%error, "roamd stopped");
        }
    }));
    Ok(Running {
        http: addr,
        node,
        shutdown,
        tasks,
    })
}

/// Send each pending consignment once, recording what came back.
///
/// Only an answer is recorded: a push nobody answered leaves the record owed,
/// and [`Roamd::unsettled`] reports it against that partner's window.
pub async fn drain(service: &Mutex<Node<Journal>>, dispatcher: &Dispatcher, reach: &ReachBack) {
    let now = time::OffsetDateTime::now_utc();
    let pending: Vec<_> = lock(service).state().pending().cloned().collect();

    for consignment in pending {
        if consignment.wire != Wire::Ocpi {
            tracing::warn!(key = %consignment.key, wire = %consignment.wire, "no client for this wire yet, so the record stays queued");
            continue;
        }
        let Some(document) = lock(service).state().document(&consignment.key).cloned() else {
            tracing::warn!(key = %consignment.key, "this record was routed without a document, so there is nothing to send");
            continue;
        };
        let Some(to) = lock(service)
            .state()
            .peers()
            .for_party(consignment.recipient())
            .cloned()
        else {
            tracing::debug!(key = %consignment.key, recipient = %consignment.recipient(), "no platform has registered for this party yet, so the record stays owed");
            continue;
        };
        // The endpoint is one the peer advertised, so it passes the same guard
        // its versions URL did.
        if let Some(endpoint) = to.endpoint("cdrs", "RECEIVER")
            && let Err(why) = reach.admit(endpoint).await
        {
            tracing::error!(key = %consignment.key, %why, "the partner's endpoint is refused");
            continue;
        }
        let key = lock(service).key();
        let answer = dispatcher.deliver(&to, &key, &document).await;
        match &answer {
            Answer::Accepted { .. } => {
                tracing::info!(key = %consignment.key, recipient = %consignment.recipient(), "the partner took it");
            }
            Answer::Refused { reason } => {
                tracing::error!(key = %consignment.key, reason = reason.as_str(), "the partner refused it — a person has to read this, not a retry");
            }
            Answer::Unreached { why } => {
                tracing::warn!(key = %consignment.key, why = why.as_str(), "nobody answered, so the record stays owed");
            }
        }
        if let Err(error) = lock(service).answered(&Actor::Timer, &consignment.key, &answer, now) {
            tracing::error!(key = %consignment.key, %error, "the answer could not be recorded");
        }
    }

    for late in lock(service).state().unsettled(now) {
        tracing::error!(unsettled = %late, "a record is past its partner's own window");
    }
}
