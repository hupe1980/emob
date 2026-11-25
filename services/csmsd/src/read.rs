//! `GET /v1/csmsd/today` — what the operator page reads (specs 004, 007).
//!
//! Bounded by the page and the estate, never by how long the platform has run:
//! the success rate for **today** and **the seven days ending today**, the
//! newest refused sessions and the silent stations, each a [`Page`] of at most
//! `limit` rows with its total and the cursor for the older ones. A refusal's
//! cursor is its position in the outcome list, which only grows, so "show
//! older" is the same page however many sessions arrived since (US3).
//!
//! Nothing here scans history: the refusals are indexed in the fold
//! (`State::refusals`), the tally is kept per day, and a page is a binary
//! search plus `limit` rows (SR-091). The whole-history view this replaced is
//! gone (SR-094).
//!
//! Behind the estate's operator tokens (SR-034). An authorisation answer is
//! not shown at all, so no uid is (SR-057).
//!
//! Presence is journalled as transitions (spec 012), so "silent since"
//! survives a restart; between two sweeps the last message heard decides.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State as Extract};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use emob_service::Operators;
use emob_service::journal::Journal;
use emob_service::page::{self, Page, Refused};
use emob_session::success::{Rate, Tally};
use serde::{Deserialize, Serialize};

pub use crate::investigation::Explanation;
use crate::journal::State;
use crate::{Csmsd, Outcome};

/// The points a window names as weakest.
pub const WORST: usize = 3;

/// The days the longer window spans, today included.
pub const WEEK_DAYS: i64 = 7;

/// What a caller may ask: nothing else is accepted (SR-089).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// Rows per list, `1..=200`, default 50.
    pub limit: Option<usize>,
    /// Refusals strictly older than this position.
    pub refused: Option<u64>,
    /// Skip this many silent stations.
    pub silent: Option<u64>,
}

/// The bounded read model.
#[derive(Debug, Serialize)]
pub struct Today {
    /// Stations currently registered in this estate.
    pub station_count: usize,
    /// The rows per list this answer was cut at.
    pub limit: usize,
    /// How many meter keys verification runs against.
    pub registry_keys: usize,
    /// The charging success rate, today and over the week.
    pub success: Windows,
    /// The newest refused sessions.
    pub refused: Page<Refusal>,
    /// The stations heard from too long ago, the most recently silent first.
    pub silent: Page<StationView>,
    /// Each mobile network with a silent station: how many of its stations
    /// are silent, the most silent first (spec 022). Forty stations silent on
    /// one network is one outage, not forty faults.
    pub networks: Vec<NetworkShare>,
}

/// One mobile network with a silent station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkShare {
    /// The MCC-MNC.
    pub network: String,
    /// Who runs it, when the table knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier: Option<&'static str>,
    /// Its stations that are silent.
    pub silent: u64,
    /// Its stations.
    pub stations: u64,
}

/// The two windows the page heads with.
#[derive(Debug, Serialize)]
pub struct Windows {
    /// One window per local calendar day, oldest first.
    pub days: Vec<DailyRate>,
    /// Today, in the estate's zone.
    pub today: Window,
    /// The seven days ending today.
    pub week: Window,
}

/// The estate's success rate on a local calendar day.
#[derive(Debug, Serialize)]
pub struct DailyRate {
    /// Civil date, YYYY-MM-DD.
    pub from: String,
    /// The estate's recorded attempts that day.
    pub estate: Rate,
}

/// The success rate over a span of days.
#[derive(Debug, Serialize)]
pub struct Window {
    /// The first day, `YYYY-MM-DD`.
    pub from: String,
    /// The last day, inclusive.
    pub to: String,
    /// The estate.
    pub estate: Rate,
    /// The points that failed most in it, lowest rate first.
    pub worst: Vec<Point>,
}

/// One point's rate.
#[derive(Debug, Serialize)]
pub struct Point {
    /// Its OCPP identity.
    pub point: String,
    /// Its rate in the window.
    pub rate: Rate,
}

/// One refused session.
#[derive(Debug, Serialize)]
pub struct Refusal {
    /// Position of this specific decision, even for a reused local ID.
    pub position: u64,
    /// The station.
    pub identity: String,
    /// The transaction.
    pub transaction: String,
    /// A bounded explanation of this refusal.
    #[serde(flatten)]
    pub explanation: Explanation,
}

/// One provisioned station and its link.
#[derive(Debug, Serialize)]
pub struct StationView {
    /// The OCPP identity.
    pub identity: String,
    /// The point it is.
    pub evse: String,
    /// The heartbeat interval it was handed.
    pub heartbeat_secs: i32,
    /// When it was last heard from, since this process started.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_heard: Option<time::OffsetDateTime>,
    /// Since when it has said nothing for longer than its heartbeat.
    #[serde(with = "time::serde::rfc3339")]
    pub silent_since: time::OffsetDateTime,
    /// The mobile network it reported at boot, as MCC-MNC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Who runs that network, when the table knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier: Option<&'static str>,
    /// Its newest note — what the person who drives out reads first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `14:30` when the silent point is held for a driver (spec 025) — who
    /// will arrive at a station that does not answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_until: Option<String>,
}

/// A station is silent once nothing has been heard from it for longer than its
/// heartbeat interval — since it was last heard, or since this process started
/// when it has not been heard at all.
#[must_use]
pub fn silent_since(
    last_heard: Option<time::OffsetDateTime>,
    started: time::OffsetDateTime,
    now: time::OffsetDateTime,
) -> Option<time::OffsetDateTime> {
    let since = last_heard.unwrap_or(started);
    (now - since > time::Duration::seconds(i64::from(crate::HEARTBEAT_SECS))).then_some(since)
}

fn window(tally: &Tally, from: time::Date, to: time::Date) -> Window {
    Window {
        from: from.to_string(),
        to: to.to_string(),
        estate: tally.estate.clone(),
        worst: tally
            .worst(WORST)
            .into_iter()
            .map(|(point, rate)| Point {
                point: point.to_owned(),
                rate: rate.clone(),
            })
            .collect(),
    }
}

impl Today {
    /// The bounded view of one CSMS. `stations` are the estate's, as
    /// `(identity, evse)`.
    ///
    /// # Errors
    ///
    /// [`Refused`] for a `limit` outside `1..=200` (SR-088).
    pub fn of<J: Journal>(
        csmsd: &Csmsd<J>,
        stations: &[(String, String)],
        params: &Params,
    ) -> Result<Self, Refused> {
        let limit = page::limit(params.limit)?;
        let registry_keys = csmsd.registry().len();
        let (silent, networks) = silent(csmsd, stations, params.silent.unwrap_or(0), limit);
        let now = csmsd.now();
        let mut view = csmsd.with_state(|state| {
            Self::from_state(state, now, limit, params.refused, registry_keys, silent)
        });
        view.networks = networks;
        Ok(view)
    }

    /// The part of the view that is the fold's: the two windows ending on
    /// `now`'s day in the estate zone, and the refusals strictly older than
    /// `refused` — a binary search over the index, then `limit` rows (SR-090,
    /// SR-091). What it costs is independent of how many sessions settled.
    #[must_use]
    pub fn from_state(
        state: &State,
        now: time::OffsetDateTime,
        limit: usize,
        refused: Option<u64>,
        registry_keys: usize,
        silent: Page<StationView>,
    ) -> Self {
        let today = state.zone.local(now).date;
        let week_from = today.saturating_sub(time::Duration::days(WEEK_DAYS - 1));
        let success = Windows {
            days: (0..WEEK_DAYS)
                .map(|offset| {
                    let day = week_from.saturating_add(time::Duration::days(offset));
                    DailyRate {
                        from: day.to_string(),
                        estate: state
                            .success
                            .on(day)
                            .map(|tally| &tally.estate)
                            .cloned()
                            .unwrap_or_default(),
                    }
                })
                .collect(),
            today: window(
                state.success.on(today).unwrap_or(&Tally::default()),
                today,
                today,
            ),
            week: window(&state.success.window(week_from, today), week_from, today),
        };
        let cut = refused.map_or(state.refusals.len(), |before| {
            state
                .refusals
                .partition_point(|position| *position < before)
        });
        let refused = Page::newest(
            state.refusals[..cut].iter().filter_map(|&position| {
                let outcome = state.outcomes.get(usize::try_from(position).ok()?)?;
                let Outcome::Refused {
                    identity,
                    transaction,
                    reasons,
                    findings,
                } = outcome
                else {
                    return None;
                };
                Some((
                    position,
                    Refusal {
                        position,
                        identity: identity.clone(),
                        transaction: transaction.clone(),
                        explanation: Explanation::of(reasons, findings),
                    },
                ))
            }),
            state.refusals.len() as u64,
            limit,
        );
        Self {
            station_count: state.registry.len(),
            limit,
            registry_keys,
            success,
            refused,
            silent,
            networks: Vec::new(),
        }
    }
}

/// The estate's silent stations, the most recently silent first, from
/// `offset` on, and the networks they are silent on. Presence is about the
/// link, so it is read on the wall clock it was written with.
fn silent<J: Journal>(
    csmsd: &Csmsd<J>,
    stations: &[(String, String)],
    offset: u64,
    limit: usize,
) -> (Page<StationView>, Vec<NetworkShare>) {
    let wall = time::OffsetDateTime::now_utc();
    let mut per_network: std::collections::BTreeMap<String, (u64, u64)> =
        std::collections::BTreeMap::new();
    let mut silent: Vec<StationView> = stations
        .iter()
        .filter_map(|(identity, evse)| {
            let known = csmsd.known(identity);
            let network = known
                .as_ref()
                .and_then(|known| known.identity.as_ref())
                .and_then(|(said, _)| said.network.clone());
            if let Some(network) = &network {
                per_network.entry(network.clone()).or_default().1 += 1;
            }
            let note = known
                .as_ref()
                .and_then(|known| known.notes.last())
                .map(|note| note.text.clone());
            let journalled = known.and_then(|known| known.presence);
            let (last_heard, since) = match journalled {
                Some(crate::journal::Presence::Offline { last_heard, .. }) => {
                    (Some(last_heard), last_heard)
                }
                Some(crate::journal::Presence::Online(online)) => {
                    let last_heard = csmsd.last_heard(identity).or(Some(online));
                    (last_heard, silent_since(last_heard, csmsd.started(), wall)?)
                }
                None => {
                    let last_heard = csmsd.last_heard(identity);
                    (last_heard, silent_since(last_heard, csmsd.started(), wall)?)
                }
            };
            if let Some(network) = &network {
                per_network.entry(network.clone()).or_default().0 += 1;
            }
            Some(StationView {
                identity: identity.clone(),
                evse: evse.clone(),
                heartbeat_secs: crate::HEARTBEAT_SECS,
                last_heard,
                silent_since: since,
                carrier: network.as_deref().and_then(crate::carrier::name),
                network,
                note,
                reserved_until: csmsd
                    .reservation(identity)
                    .map(|held| csmsd.held_until(identity, held.until)),
            })
        })
        .collect();
    let mut networks: Vec<NetworkShare> = per_network
        .into_iter()
        .filter(|(_, (silent, _))| *silent > 0)
        .map(|(network, (silent, stations))| NetworkShare {
            carrier: crate::carrier::name(&network),
            network,
            silent,
            stations,
        })
        .collect();
    networks.sort_by(|a, b| {
        b.silent
            .cmp(&a.silent)
            .then_with(|| a.network.cmp(&b.network))
    });
    silent.sort_by(|a, b| {
        b.silent_since
            .cmp(&a.silent_since)
            .then_with(|| a.identity.cmp(&b.identity))
    });
    let total = silent.len() as u64;
    (
        Page::from_offset(silent.into_iter(), total, offset, limit),
        networks,
    )
}

/// What the routes read: the CSMS (whose registry holds the stations, spec
/// 021), and whether a profile-3 station can be admitted here.
pub(crate) type Shared<J> = (Arc<Csmsd<J>>, bool);

async fn today<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    params: Result<Query<Params>, QueryRejection>,
) -> Response {
    let params = match params {
        Ok(Query(params)) => params,
        Err(rejection) => return Refused::from(rejection).into_response(),
    };
    let stations: Vec<(String, String)> = csmsd.with_registry(|registry, _| {
        registry
            .values()
            .map(|station| (station.identity.clone(), station.evse.to_string()))
            .collect()
    });
    match Today::of(&csmsd, &stations, &params) {
        Ok(view) => Json(view).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The routes, behind the operators. `certificates` says whether a profile-3
/// station can be admitted by this CSMS at all.
pub fn router<J: Journal>(
    csmsd: Arc<Csmsd<J>>,
    operators: Operators,
    certificates: bool,
) -> axum::Router {
    let reads = axum::Router::new()
        .route("/v1/csmsd/today", get(today::<J>))
        .route(
            "/v1/csmsd/sessions/{position}",
            get(crate::investigation::session::<J>),
        )
        .route(
            "/v1/csmsd/sessions/search",
            axum::routing::post(crate::investigation::search::<J>)
                .layer(axum::extract::DefaultBodyLimit::max(2048)),
        )
        .route("/v1/csmsd/public-points", get(public_points::<J>))
        .route("/v1/csmsd/stations", get(search_stations::<J>))
        .route("/v1/csmsd/map", get(map::<J>))
        .route("/v1/csmsd/stations/{identity}", get(station::<J>))
        .with_state((Arc::clone(&csmsd), certificates));
    let writes = axum::Router::new()
        .route(
            "/v1/csmsd/stations",
            axum::routing::post(register_station::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}",
            axum::routing::put(update_station::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}/retire",
            axum::routing::post(retire_station::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}/password",
            axum::routing::post(new_password::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}/notes",
            axum::routing::post(note::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}/notes/{number}/remove",
            axum::routing::post(remove_note::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}/reservations",
            axum::routing::post(reserve::<J>),
        )
        .route(
            "/v1/csmsd/stations/{identity}/reservations/{number}/cancel",
            axum::routing::post(cancel_reservation::<J>),
        )
        .with_state((csmsd, certificates));
    emob_service::serve::operator_only(reads, operators.clone())
        .merge(emob_service::serve::operator_write(writes, operators))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("private, no-store"),
                );
                response
            },
        ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryQuery {
    revision: Option<u64>,
    #[serde(default)]
    offset: usize,
}

async fn public_points<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    Query(query): Query<RegistryQuery>,
) -> Response {
    use emob_service::publication::{PublicPoint, RegistryPage};
    const LIMIT: usize = 100;
    let now = csmsd.now();
    csmsd.with_registry(|registry, state| {
        let expired = state
            .stations
            .values()
            .filter(|s| s.projected_expiry(now).is_some())
            .count();
        // Pagination compares an opaque token. A wall-clock expiry and the next durable
        // update can occur between pages even when their simple counts cancel out.
        let revision = if expired == 0 {
            state.publication_revision
        } else {
            use sha2::{Digest as _, Sha256};
            let mut hash = Sha256::new();
            hash.update(state.publication_revision.to_be_bytes());
            hash.update((expired as u64).to_be_bytes());
            u64::from_be_bytes(
                hash.finalize()[..8]
                    .try_into()
                    .expect("SHA-256 has 32 bytes"),
            )
        };
        if query.revision.is_some_and(|rev| rev != revision)
            || (query.offset > 0 && query.revision.is_none())
        {
            return axum::http::StatusCode::CONFLICT.into_response();
        }
        let mut rows = registry
            .values()
            .filter(|s| s.publicly_accessible)
            .skip(query.offset);
        let points = rows
            .by_ref()
            .take(LIMIT)
            .filter_map(|s| {
                let known = state.stations.get(&s.identity)?;
                let (observation, status_changed_at) = known.public_status(now);
                Some(PublicPoint {
                    fully_renewable: s.fully_renewable,
                    identity: s.identity.clone(),
                    evse: s.evse.clone(),
                    rated_power_kw: s.rated_power_kw,
                    connector: s.connector,
                    location: s.location.clone(),
                    tariff: s.tariff.clone(),
                    version: known.facility_version,
                    site_version: *state.site_versions.get(&s.location.id)?,
                    changed_at: known.registered_at?,
                    status_changed_at,
                    observation,
                })
            })
            .collect();
        Json(RegistryPage {
            revision,
            points,
            next: rows.next().and_then(|_| query.offset.checked_add(LIMIT)),
        })
        .into_response()
    })
}

/// What a registration answers — the only place a minted password appears
/// (SR-144).
#[derive(Debug, Serialize)]
pub struct Registered {
    /// The station.
    pub identity: String,
    /// The password, for profiles 1 and 2; shown once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// A refusal of a write, naming the field (`422`).
fn refused_write(refusal: &crate::registry::Refusal) -> Response {
    (axum::http::StatusCode::UNPROCESSABLE_ENTITY, Json(refusal)).into_response()
}

/// A body that is not the shape the route takes (`422`).
fn malformed(error: &axum::extract::rejection::JsonRejection) -> Response {
    refused_write(&crate::registry::Refusal {
        field: "body".to_owned(),
        reason: error.body_text(),
    })
}

/// `POST /v1/csmsd/stations` — register a station (spec 021). The body is a
/// station as the estate spells one, without a credential: the password is
/// minted and answered once.
async fn register_station<J: Journal>(
    Extract((csmsd, certificates)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    body: Result<Json<emob_service::estate::Station>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let station = match body {
        Ok(Json(station)) => station,
        Err(error) => return malformed(&error),
    };
    if station.password_hash.is_some() {
        return refused_write(&crate::registry::Refusal {
            field: "password_hash".to_owned(),
            reason: "a credential is minted here, never sent".to_owned(),
        });
    }
    if station.security_profile == emob_service::estate::SecurityProfile::TlsClientCertificate
        && !certificates
    {
        return refused_write(&crate::registry::Refusal {
            field: "security_profile".to_owned(),
            reason: "profile 3 needs [csms] client_ca and TLS ending at csmsd".to_owned(),
        });
    }
    let identity = station.identity.clone();
    match csmsd.register(&by, station) {
        Ok(password) => {
            tracing::info!(station = %identity, "a station was registered");
            (
                axum::http::StatusCode::CREATED,
                Json(Registered { identity, password }),
            )
                .into_response()
        }
        Err(refusal) => refused_write(&refusal),
    }
}

/// A change to a registered station.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// The new record; its identity is the path's.
    pub station: emob_service::estate::Station,
    /// Why, at least ten characters.
    pub reason: String,
    /// Mint a new password, answered once.
    #[serde(default)]
    pub new_password: bool,
}

/// `PUT /v1/csmsd/stations/{identity}` — change a station, keeping its
/// credential unless `new_password` asks for one.
async fn update_station<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    body: Result<Json<Change>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let change = match body {
        Ok(Json(change)) => change,
        Err(error) => return malformed(&error),
    };
    if change.station.identity != identity {
        return refused_write(&crate::registry::Refusal {
            field: "identity".to_owned(),
            reason: "an identity is not changed; retire the station and register the new one"
                .to_owned(),
        });
    }
    match csmsd.update(&by, change.station, &change.reason, change.new_password) {
        Ok(password) => Json(Registered { identity, password }).into_response(),
        Err(refusal) => refused_write(&refusal),
    }
}

/// `POST /v1/csmsd/stations/{identity}/password` — mint a new password for a
/// station, its record otherwise unchanged; answered once (SR-144).
async fn new_password<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    body: Result<Json<Retirement>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let why = match body {
        Ok(Json(why)) => why,
        Err(error) => return malformed(&error),
    };
    let Some(station) = csmsd.registered(&identity) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    match csmsd.update(&by, station, &why.reason, true) {
        Ok(password) => Json(Registered { identity, password }).into_response(),
        Err(refusal) => refused_write(&refusal),
    }
}

/// Why a station is retired.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retirement {
    /// At least ten characters.
    pub reason: String,
}

/// `POST /v1/csmsd/stations/{identity}/retire` — its next connection is
/// refused; its history stays.
async fn retire_station<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    body: Result<Json<Retirement>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let retirement = match body {
        Ok(Json(retirement)) => retirement,
        Err(error) => return malformed(&error),
    };
    match csmsd.retire(&by, &identity, &retirement.reason) {
        Ok(()) => axum::http::StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => refused_write(&refusal),
    }
}

/// A note, as the route takes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteBody {
    /// 1–500 characters.
    pub text: String,
}

/// What writing a note answers.
#[derive(Debug, Serialize)]
pub struct Written {
    /// The note's number on the station.
    pub number: u64,
}

/// `POST /v1/csmsd/stations/{identity}/notes` — a note on a station, under
/// the operator who wrote it (spec 023, SR-156).
async fn note<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    body: Result<Json<NoteBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return malformed(&error),
    };
    match csmsd.note(&by, &identity, &body.text) {
        Ok(number) => (axum::http::StatusCode::CREATED, Json(Written { number })).into_response(),
        Err(refusal) if refusal.field == "identity" => {
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
        Err(refusal) => refused_write(&refusal),
    }
}

/// `POST /v1/csmsd/stations/{identity}/notes/{number}/remove`.
async fn remove_note<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path((identity, number)): axum::extract::Path<(String, u64)>,
) -> Response {
    match csmsd.remove_note(&by, &identity, number) {
        Ok(true) => axum::http::StatusCode::NO_CONTENT.into_response(),
        Ok(false) => axum::http::StatusCode::NOT_FOUND.into_response(),
        Err(refusal) => refused_write(&refusal),
    }
}

/// A reservation asked for (spec 025).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveBody {
    /// The card's UID, as the station reads it. Sent to the station and to
    /// the provider, journalled as the provider's reference only (SR-176).
    pub uid: String,
    /// How long, 5–1440.
    pub minutes: u32,
    /// Why — what the next person reading the asset card needs.
    pub reason: String,
}

/// What a reservation answers.
#[derive(Debug, Serialize)]
pub struct Held {
    /// Its number on the station.
    pub number: u64,
    /// Held until.
    #[serde(with = "time::serde::rfc3339")]
    pub until: time::OffsetDateTime,
}

/// `POST /v1/csmsd/stations/{identity}/reservations`.
async fn reserve<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    body: Result<Json<ReserveBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => return malformed(&error),
    };
    match csmsd
        .reserve(&by, &identity, &body.uid, body.minutes, &body.reason)
        .await
    {
        Ok((number, until)) => (
            axum::http::StatusCode::CREATED,
            Json(Held { number, until }),
        )
            .into_response(),
        Err(refusal) if refusal.field == "identity" => {
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
        Err(refusal) => refused_write(&refusal),
    }
}

/// `POST /v1/csmsd/stations/{identity}/reservations/{number}/cancel`.
async fn cancel_reservation<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::Extension(by): axum::Extension<emob_service::Actor>,
    axum::extract::Path((identity, number)): axum::extract::Path<(String, u64)>,
) -> Response {
    match csmsd.cancel_reservation(&by, &identity, number).await {
        Ok(()) => axum::http::StatusCode::NO_CONTENT.into_response(),
        Err(_) => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// What a station search may ask: nothing else is accepted.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// Words every row must contain, in any of its identity, EVSE, vendor,
    /// model, serial or firmware — station or meter. Case does not matter.
    pub q: Option<String>,
    /// Rows, `1..=200`, default 50.
    pub limit: Option<usize>,
    /// Skip this many matches.
    pub offset: Option<u64>,
    /// Only stations inside `west,south,east,north`, in decimal degrees
    /// (spec 022) — what a cell of the map links to.
    pub area: Option<String>,
}

/// What the map may ask: the search's words and area, nothing else.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapQuery {
    /// As [`Search::q`].
    pub q: Option<String>,
    /// As [`Search::area`].
    pub area: Option<String>,
}

/// A box on the map, in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    /// Its western edge.
    pub west: rust_decimal::Decimal,
    /// Its southern edge.
    pub south: rust_decimal::Decimal,
    /// Its eastern edge.
    pub east: rust_decimal::Decimal,
    /// Its northern edge.
    pub north: rust_decimal::Decimal,
}

impl Area {
    /// `west,south,east,north`.
    ///
    /// # Errors
    ///
    /// [`Refused`] naming the shape when it is not four degrees in order
    /// (SR-151).
    pub fn parse(text: &str) -> Result<Self, Refused> {
        use rust_decimal::Decimal;
        let refused = || {
            Refused(format!(
                "area {text:?} is not west,south,east,north in degrees, e.g. 5.9,47.3,15.0,55.1"
            ))
        };
        let edges: Vec<Decimal> = text
            .split(',')
            .map(|edge| edge.trim().parse::<Decimal>())
            .collect::<Result<_, _>>()
            .map_err(|_| refused())?;
        let [west, south, east, north] = edges[..] else {
            return Err(refused());
        };
        let lon = Decimal::from(180);
        let lat = Decimal::from(90);
        if west >= east
            || south >= north
            || west < -lon
            || east > lon
            || south < -lat
            || north > lat
        {
            return Err(refused());
        }
        Ok(Self {
            west,
            south,
            east,
            north,
        })
    }

    /// Whether a point lies inside, its western and southern edges included.
    #[must_use]
    pub fn holds(&self, latitude: rust_decimal::Decimal, longitude: rust_decimal::Decimal) -> bool {
        (self.west..self.east).contains(&longitude) && (self.south..self.north).contains(&latitude)
    }
}

/// One station in a search.
#[derive(Debug, Serialize)]
pub struct StationRow {
    /// The OCPP identity.
    pub identity: String,
    /// The point.
    pub evse: String,
    /// `1.6`, `2.0.1`, `2.1`.
    pub ocpp: &'static str,
    /// `online`, `offline` or `unknown`.
    pub presence: &'static str,
    /// Since when, when known.
    #[serde(with = "time::serde::rfc3339::option")]
    pub since: Option<time::OffsetDateTime>,
    /// What it said at boot.
    pub vendor: Option<String>,
    /// Its model.
    pub model: Option<String>,
    /// Its firmware.
    pub firmware: Option<String>,
    /// What its meters sign as firmware, one per point.
    pub meter_firmware: Vec<String>,
    /// The mobile network it reported at boot, as MCC-MNC.
    pub network: Option<String>,
    /// Who runs that network, when the table knows.
    pub carrier: Option<&'static str>,
    /// WGS 84, as registered.
    pub latitude: rust_decimal::Decimal,
    /// WGS 84, as registered.
    pub longitude: rust_decimal::Decimal,
    /// Its newest note.
    pub note: Option<String>,
    /// `14:30` while it is held for a driver (spec 025).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_until: Option<String>,
    /// Every note it holds, for the search only.
    #[serde(skip)]
    notes: String,
}

/// `GET /v1/csmsd/stations?q=&limit=&offset=` — the estate's stations, in
/// estate order, filtered by words (spec 012). Bounded by `limit` and by the
/// estate, never by history: the first question after a vendor advisory —
/// "which stations run firmware X" — is one request.
async fn search_stations<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    params: Result<Query<Search>, QueryRejection>,
) -> Response {
    let search = match params {
        Ok(Query(search)) => search,
        Err(rejection) => return Refused::from(rejection).into_response(),
    };
    let limit = match page::limit(search.limit) {
        Ok(limit) => limit,
        Err(refused) => return refused.into_response(),
    };
    let area = match search.area.as_deref().map(Area::parse).transpose() {
        Ok(area) => area,
        Err(refused) => return refused.into_response(),
    };
    Json(search_registry(
        &csmsd,
        search.q.as_deref().unwrap_or_default(),
        area,
        search.offset.unwrap_or(0),
        limit,
    ))
    .into_response()
}

/// The search itself: words, case-insensitive, all of them in a station's
/// identity, EVSE, OCPP version, presence, vendor, model, firmware, network or
/// carrier, inside `area` when one is given — under one lock, so ten thousand
/// stations cost one pass (spec 021, SC-002).
#[must_use]
pub fn search_registry<J: Journal>(
    csmsd: &Csmsd<J>,
    query: &str,
    area: Option<Area>,
    offset: u64,
    limit: usize,
) -> Page<StationRow> {
    let rows = matches(csmsd, query, area);
    let total = rows.len() as u64;
    Page::from_offset(rows.into_iter(), total, offset, limit)
}

fn matches<J: Journal>(csmsd: &Csmsd<J>, query: &str, area: Option<Area>) -> Vec<StationRow> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    csmsd.with_registry(|registry, state| {
        registry
            .values()
            .filter(|entry| {
                area.is_none_or(|area| {
                    area.holds(entry.location.latitude, entry.location.longitude)
                })
            })
            .map(|entry| StationRow::from_parts(entry, state.stations.get(&entry.identity)))
            .filter(|row| {
                let haystack = row.haystack();
                words.iter().all(|word| haystack.contains(word.as_str()))
            })
            .collect()
    })
}

/// The most stations the map draws one by one; above this it draws a grid
/// (SR-151).
pub const MAP_POINTS: usize = 300;

/// The grid's cells per side: at most `MAP_GRID²` cells, whatever the fleet.
pub const MAP_GRID: i64 = 16;

/// What the map draws (spec 022): every match as a point, or — above
/// [`MAP_POINTS`] — a grid over the matches' extent.
#[derive(Debug, Serialize)]
pub struct Map {
    /// The matches.
    pub total: u64,
    /// Each match, when there are at most [`MAP_POINTS`].
    pub points: Vec<MapPoint>,
    /// Otherwise each occupied cell.
    pub cells: Vec<Cell>,
}

/// One station on the map.
#[derive(Debug, Serialize)]
pub struct MapPoint {
    /// Its identity.
    pub identity: String,
    /// WGS 84.
    pub latitude: rust_decimal::Decimal,
    /// WGS 84.
    pub longitude: rust_decimal::Decimal,
    /// `online`, `offline`, `unknown`.
    pub presence: &'static str,
}

/// One cell of the grid: its bounds, as an [`Area`] links to, and its count.
#[derive(Debug, Serialize)]
pub struct Cell {
    /// Western edge.
    pub west: rust_decimal::Decimal,
    /// Southern edge.
    pub south: rust_decimal::Decimal,
    /// Eastern edge.
    pub east: rust_decimal::Decimal,
    /// Northern edge.
    pub north: rust_decimal::Decimal,
    /// Stations inside.
    pub stations: u64,
    /// Of them, silent.
    pub silent: u64,
}

/// `GET /v1/csmsd/map?q=&area=` (spec 022).
async fn map<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    params: Result<Query<MapQuery>, QueryRejection>,
) -> Response {
    let query = match params {
        Ok(Query(query)) => query,
        Err(rejection) => return Refused::from(rejection).into_response(),
    };
    let area = match query.area.as_deref().map(Area::parse).transpose() {
        Ok(area) => area,
        Err(refused) => return refused.into_response(),
    };
    Json(map_of(&csmsd, query.q.as_deref().unwrap_or_default(), area)).into_response()
}

/// The map of a search: bounded by [`MAP_POINTS`] and `MAP_GRID²`, never by the
/// fleet (SR-151). The grid is laid in hundredths of a degree, so it is exact.
#[must_use]
pub fn map_of<J: Journal>(csmsd: &Csmsd<J>, query: &str, area: Option<Area>) -> Map {
    let rows = matches(csmsd, query, area);
    let total = rows.len() as u64;
    if rows.len() <= MAP_POINTS {
        return Map {
            total,
            points: rows
                .into_iter()
                .map(|row| MapPoint {
                    identity: row.identity,
                    latitude: row.latitude,
                    longitude: row.longitude,
                    presence: row.presence,
                })
                .collect(),
            cells: Vec::new(),
        };
    }
    let at: Vec<(i64, i64, bool)> = rows
        .iter()
        .map(|row| {
            (
                hundredths(row.longitude),
                hundredths(row.latitude),
                row.presence == "offline",
            )
        })
        .collect();
    let (west, east) = bounds(at.iter().map(|p| p.0));
    let (south, north) = bounds(at.iter().map(|p| p.1));
    // Square cells would waste the grid on a fleet that is long and thin; each
    // axis gets its own step, at least a hundredth.
    let step_x = ((east - west) / MAP_GRID + 1).max(1);
    let step_y = ((north - south) / MAP_GRID + 1).max(1);
    let mut grid: std::collections::BTreeMap<(i64, i64), (u64, u64)> =
        std::collections::BTreeMap::new();
    for (x, y, silent) in at {
        let cell = grid
            .entry(((x - west) / step_x, (y - south) / step_y))
            .or_default();
        cell.0 += 1;
        cell.1 += u64::from(silent);
    }
    let degrees = |h: i64| rust_decimal::Decimal::new(h, 2);
    Map {
        total,
        points: Vec::new(),
        cells: grid
            .into_iter()
            .map(|((i, j), (stations, silent))| Cell {
                west: degrees(west + i * step_x),
                south: degrees(south + j * step_y),
                east: degrees(west + (i + 1) * step_x),
                north: degrees(south + (j + 1) * step_y),
                stations,
                silent,
            })
            .collect(),
    }
}

fn hundredths(degrees: rust_decimal::Decimal) -> i64 {
    use rust_decimal::prelude::ToPrimitive as _;
    (degrees * rust_decimal::Decimal::ONE_HUNDRED)
        .floor()
        .to_i64()
        .unwrap_or_default()
}

fn bounds(values: impl Iterator<Item = i64>) -> (i64, i64) {
    values.fold((i64::MAX, i64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
}

impl StationRow {
    fn from_parts(
        entry: &emob_service::estate::Station,
        known: Option<&crate::journal::Known>,
    ) -> Self {
        use crate::journal::Presence;
        let known = known.cloned().unwrap_or_default();
        let (presence, since) = match known.presence {
            Some(Presence::Online(at)) => ("online", Some(at)),
            Some(Presence::Offline { last_heard, .. }) => ("offline", Some(last_heard)),
            None => ("unknown", None),
        };
        let said = known.identity.map(|(said, _)| said);
        Self {
            identity: entry.identity.clone(),
            evse: entry.evse.to_string(),
            ocpp: ocpp_name(entry.ocpp),
            presence,
            since,
            vendor: said.as_ref().map(|s| s.vendor.clone()),
            model: said.as_ref().map(|s| s.model.clone()),
            firmware: said.as_ref().and_then(|s| s.firmware.clone()),
            meter_firmware: known
                .meters
                .values()
                .filter_map(|(meter, _)| meter.firmware_label().map(ToOwned::to_owned))
                .collect(),
            carrier: said
                .as_ref()
                .and_then(|s| s.network.as_deref())
                .and_then(crate::carrier::name),
            network: said.and_then(|s| s.network),
            latitude: entry.location.latitude,
            longitude: entry.location.longitude,
            note: known.notes.last().map(|note| note.text.clone()),
            reserved_until: known
                .reservation
                .as_ref()
                .filter(|held| held.until > time::OffsetDateTime::now_utc())
                .map(|held| crate::reservation::wall_clock(&entry.location.time_zone, held.until)),
            notes: known
                .notes
                .iter()
                .map(|note| note.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    fn haystack(&self) -> String {
        let mut text = format!(
            "{} {} {} {} {} {}",
            self.identity,
            self.evse,
            self.ocpp,
            self.presence,
            self.vendor.as_deref().unwrap_or_default(),
            self.model.as_deref().unwrap_or_default(),
        );
        for word in self
            .firmware
            .iter()
            .chain(&self.meter_firmware)
            .chain(&self.network)
            .map(String::as_str)
            .chain(self.carrier)
            .chain(Some(self.notes.as_str()))
        {
            text.push(' ');
            text.push_str(word);
        }
        text.to_lowercase()
    }
}

const fn ocpp_name(ocpp: emob_service::estate::Ocpp) -> &'static str {
    match ocpp {
        emob_service::estate::Ocpp::V1_6 => "1.6",
        emob_service::estate::Ocpp::V2_0_1 => "2.0.1",
        emob_service::estate::Ocpp::V2_1 => "2.1",
    }
}

/// `GET /v1/csmsd/stations/{identity}` — one station as the platform knows it
/// (spec 012, `contracts/station-view.md`). An identity the estate does not
/// list is an empty `404` that does not echo it (SR-139).
async fn station<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
) -> Response {
    let Some(entry) = csmsd.registered(&identity) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    Json(StationPage::of(&csmsd, &entry)).into_response()
}

/// One station: what the estate says, what the station says, what its meter
/// signs, and what happened there.
#[derive(Debug, Serialize)]
pub struct StationPage {
    /// Whether station tariff presentation is enabled.
    pub show_price: bool,
    /// Whether the operator has opted this station in for public disclosure.
    pub publicly_accessible: bool,
    /// Declaration of the electricity supply, when known.
    pub fully_renewable: Option<bool>,
    /// Latest current-boot tariff installation evidence.
    pub station_tariff: Option<crate::journal::StationTariff>,
    /// The OCPP identity.
    pub identity: String,
    /// The point.
    pub evse: String,
    /// `1.6`, `2.0.1`, `2.1`.
    pub ocpp: &'static str,
    /// OCPP security profile.
    pub security_profile: u8,
    /// Rated power, kW.
    pub rated_power_kw: String,
    /// In service since, as the estate says.
    pub commissioned: Option<String>,
    /// The tariff it is priced by.
    pub tariff: String,
    /// Its connector.
    pub connector: emob_core::ConnectorType,
    /// How long its display shows a receipt's code, when it does.
    pub receipt_display_minutes: Option<u8>,
    /// Where it stands.
    pub location: emob_service::estate::Location,
    /// Online or offline, and since when; `None` before it was ever heard.
    pub presence: Option<PresenceView>,
    /// What it said at its last boot.
    pub reported_identity: Option<ReportedIdentity>,
    /// Firmware changes, newest first — the intervention record.
    pub firmware_history: Vec<crate::journal::FirmwareChange>,
    /// What each point's meter signs about itself.
    pub meters: Vec<MeterView>,
    /// Firmware and security reports, newest first.
    pub reports: Vec<ReportView>,
    /// Boots in the last seven days.
    pub boots_7d: usize,
    /// Its success rate today and over seven days.
    pub success: StationSuccess,
    /// Its last sessions, newest first.
    pub recent: Vec<RecentView>,
    /// Operators' notes, newest first (spec 023).
    pub notes: Vec<crate::journal::Note>,
    /// The reservation holding it, if one is open (spec 025).
    pub reservation: Option<ReservationView>,
    /// The price a session starting now is rated with, as the price page
    /// shows it (spec 015).
    pub price: Option<PriceView>,
}

/// A reservation, as the asset card shows it — the card's tail, never its
/// UID (SR-176).
#[derive(Debug, Serialize)]
pub struct ReservationView {
    /// The reservation itself.
    #[serde(flatten)]
    pub held: crate::journal::Reservation,
    /// `until` on the station's wall clock, `14:30`.
    pub until_local: String,
}

/// The price in force, as the asset card names it.
#[derive(Debug, Serialize)]
pub struct PriceView {
    /// Its first line, e.g. `0,49 € pro kWh`.
    pub line: String,
    /// The Preisstand.
    pub preisstand: String,
    /// The public price page, `/p/{evse}`.
    pub path: String,
}

/// Presence on the page.
#[derive(Debug, Serialize)]
pub struct PresenceView {
    /// `online` or `offline`.
    pub state: &'static str,
    /// Since when.
    #[serde(with = "time::serde::rfc3339")]
    pub since: time::OffsetDateTime,
    /// When it was last heard from, when offline.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_heard: Option<time::OffsetDateTime>,
}

/// The station's own claim, and when it made it.
#[derive(Debug, Serialize)]
pub struct ReportedIdentity {
    /// What it said.
    #[serde(flatten)]
    pub identity: crate::station::StationIdentity,
    /// Who runs its network, when the table knows (spec 022).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier: Option<&'static str>,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
}

/// A point's meter, as its signed records name it.
#[derive(Debug, Serialize)]
pub struct MeterView {
    /// Complete key in the key registry wire representation.
    pub registered_key: Option<RegisteredPoint>,
    /// The point.
    pub evse: String,
    /// What it signs.
    #[serde(flatten)]
    pub meter: crate::station::MeterIdentity,
    /// Since when.
    #[serde(with = "time::serde::rfc3339")]
    pub since: time::OffsetDateTime,
    /// Whether the registry holds a key for this meter now.
    pub key_registered: bool,
}

/// The point as stored in the key registry, preserving compressed encoding.
#[derive(Debug, Serialize)]
pub struct RegisteredPoint {
    /// OCMF curve name.
    pub curve: &'static str,
    /// Complete SEC1 bytes in lowercase hex.
    pub sec1: String,
}

/// A report on the page.
#[derive(Debug, Serialize)]
pub struct ReportView {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// What.
    #[serde(flatten)]
    pub report: crate::station::Report,
}

/// One session on the page.
#[derive(Debug, Serialize)]
pub struct RecentView {
    /// Stable decision reference on the operator desk.
    pub position: u64,
    /// The transaction.
    pub transaction: String,
    /// When it was decided.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// Billed or not.
    pub billed: bool,
    /// The receipt path, when this CSMS issues receipts.
    pub receipt: Option<String>,
}

/// The station's success rate.
#[derive(Debug, Serialize)]
pub struct StationSuccess {
    /// Today.
    pub today: Option<Rate>,
    /// The seven days ending today.
    pub week: Option<Rate>,
}

impl StationPage {
    /// The page for one estate station.
    #[must_use]
    pub fn of<J: Journal>(csmsd: &Csmsd<J>, entry: &emob_service::estate::Station) -> Self {
        use crate::journal::Presence;
        let now = csmsd.now();
        let known = csmsd.known(&entry.identity).unwrap_or_default();
        let presence = known.presence.map(|presence| match presence {
            Presence::Online(since) => PresenceView {
                state: "online",
                since,
                last_heard: None,
            },
            Presence::Offline { since, last_heard } => PresenceView {
                state: "offline",
                since,
                last_heard: Some(last_heard),
            },
        });
        let registry = csmsd.registry();
        let meters = known
            .meters
            .iter()
            .map(|(evse, (meter, since))| {
                let registered_key = meter
                    .component
                    .as_ref()
                    .and_then(|component| registry.key_at(component, now))
                    .map(|key| RegisteredPoint {
                        curve: key.key.curve().name(),
                        sec1: hex::encode(key.key.sec1_bytes()),
                    });
                MeterView {
                    evse: evse.clone(),
                    key_registered: registered_key.is_some(),
                    registered_key,
                    meter: meter.clone(),
                    since: *since,
                }
            })
            .collect();
        let today = csmsd.with_state(|state| state.zone.local(now).date);
        let (day, week) = csmsd.with_state(|state| {
            let day = state.success.window(today, today);
            let week = state
                .success
                .window(today - time::Duration::days(WEEK_DAYS - 1), today);
            (
                day.points.get(&entry.identity).cloned(),
                week.points.get(&entry.identity).cloned(),
            )
        });
        let week_ago = now - time::Duration::days(7);
        Self {
            station_tariff: known.station_tariff.clone(),
            publicly_accessible: entry.publicly_accessible,
            show_price: entry.show_price,
            fully_renewable: entry.fully_renewable,
            identity: entry.identity.clone(),
            evse: entry.evse.to_string(),
            ocpp: ocpp_name(entry.ocpp),
            security_profile: entry.security_profile.number(),
            rated_power_kw: entry.rated_power_kw.to_string(),
            commissioned: entry.commissioned.clone(),
            tariff: entry.tariff.clone(),
            connector: entry.connector,
            receipt_display_minutes: entry.receipt_display_minutes,
            location: entry.location.clone(),
            presence,
            reported_identity: known.identity.map(|(identity, at)| ReportedIdentity {
                carrier: identity.network.as_deref().and_then(crate::carrier::name),
                identity,
                at,
            }),
            firmware_history: known.firmware.into_iter().rev().collect(),
            meters,
            reports: known
                .reports
                .into_iter()
                .rev()
                .map(|(at, report)| ReportView { at, report })
                .collect(),
            boots_7d: known.boots.iter().filter(|at| **at >= week_ago).count(),
            success: StationSuccess { today: day, week },
            recent: known
                .recent
                .into_iter()
                .rev()
                .map(|recent| RecentView {
                    position: recent.position,
                    receipt: csmsd.with_state(|state| {
                        csmsd.filed_receipt_path(
                            state,
                            &state.investigations.receipt(recent.position)?,
                        )
                    }),
                    transaction: recent.transaction,
                    at: recent.at,
                    billed: recent.billed,
                })
                .collect(),
            notes: known.notes.into_iter().rev().collect(),
            reservation: csmsd
                .reservation(&entry.identity)
                .map(|held| ReservationView {
                    until_local: csmsd.held_until(&entry.identity, held.until),
                    held,
                }),
            price: csmsd
                .tariff(&entry.tariff)
                .and_then(|history| history.in_force_at(now))
                .map(|tariff| PriceView {
                    line: crate::price::headline(tariff, now),
                    preisstand: crate::price::preisstand(tariff),
                    path: crate::price::path(&entry.evse),
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn a_station_is_silent_only_past_its_heartbeat_and_never_before_the_start() {
        let started = datetime!(2026-11-18 08:00 UTC);
        let heard = datetime!(2026-11-18 09:00 UTC);
        let within = heard + time::Duration::seconds(300);
        let past = heard + time::Duration::seconds(301);
        assert_eq!(silent_since(Some(heard), started, within), None);
        assert_eq!(silent_since(Some(heard), started, past), Some(heard));
        // Never heard: silent since the process started, not since forever.
        assert_eq!(silent_since(None, started, past), Some(started));
        assert_eq!(silent_since(None, started, started), None);
    }

    fn refused(state: &mut State, n: u64, at: time::OffsetDateTime) {
        crate::journal::apply(
            state,
            crate::journal::Event::Noted {
                closes: Some(("CP-1".to_owned(), n.to_string())),
                outcome: Outcome::Refused {
                    identity: "CP-1".to_owned(),
                    transaction: n.to_string(),
                    reasons: vec!["no".to_owned()],
                    findings: Vec::new(),
                },
                at,
            },
        );
    }

    fn view(state: &State, limit: usize, before: Option<u64>) -> Today {
        let now = datetime!(2026-11-18 12:00 UTC);
        let none = Page::from_offset(std::iter::empty(), 0, 0, limit);
        Today::from_state(state, now, limit, before, 0, none)
    }

    fn transactions(view: &Today) -> Vec<String> {
        view.refused
            .items
            .iter()
            .map(|refusal| refusal.transaction.clone())
            .collect()
    }

    #[test]
    fn us3_sr_090_older_is_stable_while_sessions_arrive_and_any_cursor_is_a_page() {
        let mut state = State::new(emob_core::TimeZone::new("Europe/Berlin").unwrap());
        let at = datetime!(2026-11-18 09:00 UTC);
        for n in 0..5 {
            refused(&mut state, n, at);
        }
        let first = view(&state, 2, None);
        assert_eq!(transactions(&first), ["4", "3"]);
        let cursor = first.refused.older;
        // Three more arrive; the older page is the same page.
        for n in 5..8 {
            refused(&mut state, n, at);
        }
        let older = view(&state, 2, cursor);
        assert_eq!(transactions(&older), ["2", "1"]);
        assert_eq!(older.refused.total, 8);
        let last = view(&state, 2, older.refused.older);
        assert_eq!(
            (transactions(&last), last.refused.older),
            (vec!["0".to_owned()], None)
        );
        // Cursor 0 is an empty page; the largest one is the newest page.
        assert!(view(&state, 2, Some(0)).refused.items.is_empty());
        assert_eq!(transactions(&view(&state, 2, Some(u64::MAX))), ["7", "6"]);
    }

    #[test]
    fn us2_the_windows_are_today_and_the_seven_days_ending_today() {
        let mut state = State::new(emob_core::TimeZone::new("Europe/Berlin").unwrap());
        refused(&mut state, 1, datetime!(2026-11-18 09:00 UTC));
        refused(&mut state, 2, datetime!(2026-11-12 09:00 UTC));
        refused(&mut state, 3, datetime!(2026-11-11 09:00 UTC));
        let today = view(&state, 50, None);
        assert_eq!(today.success.today.from, "2026-11-18");
        assert_eq!(today.success.today.estate.attempted, 1);
        assert_eq!(today.success.week.from, "2026-11-12");
        assert_eq!(today.success.week.estate.attempted, 2);
        assert_eq!(today.success.week.worst[0].point, "CP-1");
    }
}
