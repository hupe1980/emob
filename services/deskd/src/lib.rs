//! `deskd` — the operator's page.
//!
//! # One page, and it decides nothing
//!
//! Heute is what an operator opens in the morning: the charging success rate,
//! then everything that needs a person today, in the order it needs them. It is
//! rendered on the server from the read models of the daemons that made each
//! decision — `csmsd`'s outcomes, presence and tally, `billd`'s documents,
//! `roamd`'s consignments — and from the obligation calendar. `deskd` holds no
//! journal and computes nothing that decides: a figure on the page is the
//! figure the function that billed, refused or published produced (design
//! rule 2), worded.
//!
//! # Who gets in
//!
//! An operator's browser, by HTTP Basic whose password is one of the estate's
//! operator tokens ([`emob_service::ui::operator_page`]; SR-050). `deskd` reads
//! the daemons with the token `[desk] token_env` names. It binds loopback unless
//! told otherwise, because Basic sends the token with every request.
//!
//! # Layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`fetch`] | the three reads, their timeout and cap, since-when memory |
//! | [`model`] | the read-model fields the page renders |
//! | [`heute`] | the page, a pure function of a [`heute::Snapshot`] |
//! | [`why`] | "Warum?": each finding in German and English |
//! | [`Watch`] | the witness: reads each daemon's signed head and remembers it (spec 008) |
//!
//! # The one thing `deskd` keeps
//!
//! The witness file. Every 30 seconds `deskd` reads `GET /v1/<name>/head` from
//! csmsd, billd, roamd and empd and records it in its own chained file
//! ([`emob_service::witness`]); a head that went backwards, forked or came
//! under another key is an alarm on Heute until somebody moves the file aside.
//! A daemon's volume does not hold this copy, which is the point.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use emob_core::TimeZone;
use emob_service::estate::{self, Desk, Env, Operator, Services};
use emob_service::ui::{self, Lang};
use emob_service::{Identity, Readiness, Server, Shutdown};
use serde::Deserialize;

pub mod europe;
pub mod fetch;
pub mod heute;
pub mod map;
pub mod model;
pub mod station;
pub mod stations;
pub mod why;

/// The refresh script, compiled in.
pub const SCRIPT: &str = include_str!("heute.js");

/// What `deskd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// Where `csmsd` is, and the token to read with.
    pub desk: Desk,
    /// Where `billd` and `roamd` are.
    pub services: Services,
    /// Who may open the page.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// Which verifier keys the witness trusts.
    #[serde(default)]
    pub witness: estate::Witness,
    /// Where receipt links point, for the station page's sessions.
    #[serde(default)]
    pub receipts: Option<estate::ReceiptSection>,
}

/// Where the page reads "now" from: its date, its clock and the 90-day duty
/// window.
pub use emob_service::{Clock, system_clock};

/// What a `deskd` is started with.
pub struct Options {
    /// The HTTP address.
    pub http: SocketAddr,
    /// The estate's sections `deskd` reads.
    pub estate: Estate,
    /// Where secrets are looked up.
    pub env: Env,
    /// The stop signal.
    pub shutdown: Shutdown,
    /// Who this is.
    pub identity: Identity,
    /// What time it is.
    pub clock: Clock,
    /// The witness's own file — `EMOB_JOURNAL_DIR/witness.ndjson` in a
    /// deployment, on a volume the daemons it watches do not share.
    pub witness: std::path::PathBuf,
}

/// deskd's one readiness probe: it can render the page.
const PAGE: &str = "page";

/// How often the witness reads the heads.
pub const WITNESS_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// The witness `deskd` runs: which daemons, where, and what it remembers.
pub struct Watch {
    reader: Arc<fetch::Reader>,
    daemons: Vec<(&'static str, String)>,
    witness: emob_service::witness::Witness,
    clock: Clock,
}

impl core::fmt::Debug for Watch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

impl Watch {
    /// Read every daemon's head once and record it. A daemon that does not
    /// answer, or whose note does not verify, is named with the class of its
    /// failure and nothing is recorded for it.
    pub async fn round(
        &self,
    ) -> Vec<(&'static str, Result<emob_service::witness::Verdict, String>)> {
        let mut seen = Vec::with_capacity(self.daemons.len());
        for (name, base) in &self.daemons {
            let verdict = match self.reader.head(base, name).await {
                Ok((note, key)) => self
                    .witness
                    .observe(
                        name,
                        &note,
                        key.as_deref(),
                        (self.clock)(),
                        &emob_service::Actor::Timer,
                    )
                    .map_err(|error| error.to_string()),
                Err(why) => Err(format!("{why:?}")),
            };
            match &verdict {
                Ok(verdict) if verdict.is_alarm() => {
                    tracing::error!(daemon = name, ?verdict, "a journal head went wrong");
                }
                Ok(_) => {}
                Err(why) => tracing::warn!(daemon = name, why = %why, "a head was not witnessed"),
            }
            seen.push((*name, verdict));
        }
        seen
    }

    /// What the witness holds, per daemon.
    #[must_use]
    pub fn view(&self) -> Vec<emob_service::witness::Seen> {
        self.witness.view()
    }
}

/// A `deskd` that is running.
pub struct Running {
    /// Where the page is.
    pub http: SocketAddr,
    /// The witness.
    pub watch: Arc<Watch>,
    task: tokio::task::JoinHandle<()>,
    witnessing: tokio::task::JoinHandle<()>,
    shutdown: Shutdown,
}

impl Running {
    /// Stop now.
    pub fn kill(self) {
        self.task.abort();
        self.witnessing.abort();
    }

    /// Wait until the shutdown signal, then stop.
    pub async fn wait(self) {
        self.shutdown.wait().await;
        let _ = self.task.await;
    }
}

struct Page {
    reader: Arc<fetch::Reader>,
    watch: Arc<Watch>,
    csmsd: String,
    billd: String,
    roamd: String,
    zone: TimeZone,
    clock: Clock,
    receipts: String,
    ocpp_url: Option<String>,
}

/// What the page reads from its own URL: the language and, per list, the
/// cursor of the page to show. A cursor is a `u64` or the request is refused —
/// nothing else from the query string goes anywhere (SR-095).
#[derive(Deserialize)]
struct Params {
    lang: Option<String>,
    refused: Option<u64>,
    silent: Option<u64>,
    invoices: Option<u64>,
    consignments: Option<u64>,
}

/// The page's routes: `GET /` behind the operators, the shared stylesheet and
/// the script. GET only — nothing on this surface changes anything (SR-056).
fn router(desk: Arc<Page>, operators: emob_service::Operators) -> axum::Router {
    let page = axum::Router::new()
        .route("/", get(heute_page))
        .route("/station/{identity}", get(station_page))
        .route("/stations", get(stations_page).post(create_station))
        .route("/stations/new", get(new_station_page))
        .route(
            "/station/{identity}/edit",
            get(edit_station_page).post(edit_station),
        )
        .route(
            "/station/{identity}/reservations",
            axum::routing::post(reserve),
        )
        .route(
            "/station/{identity}/reservations/{number}/cancel",
            axum::routing::post(cancel_reservation),
        )
        .route("/station/{identity}/notes", axum::routing::post(write_note))
        .route(
            "/station/{identity}/notes/{number}/remove",
            axum::routing::post(remove_note),
        )
        .route(
            "/station/{identity}/retire",
            axum::routing::post(retire_station),
        )
        .route(
            "/station/{identity}/password",
            axum::routing::post(new_password),
        )
        .with_state(desk);
    ui::operator_page(page, operators)
        .merge(ui::assets())
        .merge(ui::script(heute::SCRIPT_PATH, SCRIPT))
}

async fn heute_page(
    State(desk): State<Arc<Page>>,
    params: Result<Query<Params>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let Ok(Query(params)) = params else {
        return (
            StatusCode::BAD_REQUEST,
            "a list's position is a whole number — this link is not one Heute wrote",
        )
            .into_response();
    };
    let lang = Lang::negotiate(
        params.lang.as_deref(),
        headers
            .get(axum::http::header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok()),
    );
    let now = (desk.clock)();
    let at = heute::Cursors {
        refused: params.refused,
        silent: params.silent,
        invoices: params.invoices,
        consignments: params.consignments,
    };
    let rows = heute::ROWS;
    let csmsd_at = [("refused", at.refused), ("silent", at.silent)];
    let billd_at = [("invoices", at.invoices)];
    let roamd_at = [("consignments", at.consignments)];
    let (csmsd, billd, roamd) = tokio::join!(
        desk.reader.read("csmsd", &desk.csmsd, rows, &csmsd_at, now),
        desk.reader.read("billd", &desk.billd, rows, &billd_at, now),
        desk.reader.read("roamd", &desk.roamd, rows, &roamd_at, now),
    );
    let snapshot = heute::Snapshot {
        csmsd,
        billd,
        roamd,
        now,
        zone: desk.zone.clone(),
        at,
        journals: desk.watch.view(),
    };
    ui::html(heute::render(&snapshot, lang))
}

/// The page's language, from `?lang=` or the browser's.
fn language(lang: Option<&str>, headers: &HeaderMap) -> Lang {
    Lang::negotiate(
        lang,
        headers
            .get(axum::http::header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok()),
    )
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    offset: Option<u64>,
    area: Option<String>,
    lang: Option<String>,
}

/// `GET /stations` — the registry, searched (spec 021).
async fn stations_page(
    State(desk): State<Arc<Page>>,
    params: Result<Query<SearchParams>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let Ok(Query(params)) = params else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let lang = language(params.lang.as_deref(), &headers);
    let query = params.q.unwrap_or_default();
    let offset = params.offset.unwrap_or(0);
    // Only an area of four numbers is passed on; `csmsd` judges the rest.
    let area = params.area.filter(|area| {
        let edges: Vec<&str> = area.split(',').collect();
        edges.len() == 4 && edges.iter().all(|edge| map::hundredths(edge).is_some())
    });
    let (found, drawn) = tokio::join!(
        desk.reader.stations::<stations::Found>(
            &desk.csmsd,
            &query,
            area.as_deref(),
            offset,
            heute::ROWS
        ),
        desk.reader
            .map::<stations::Drawn>(&desk.csmsd, &query, area.as_deref()),
    );
    match found {
        Ok(found) => ui::html(stations::render_list(
            &stations::Listing {
                found: &found,
                // A map that did not come is a page without one, not an error.
                map: drawn
                    .map(|drawn| drawn.svg(&query, lang))
                    .unwrap_or_default(),
                query: &query,
                area: area.as_deref(),
                offset,
                limit: heute::ROWS,
            },
            lang,
        )),
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            lang.pick("csmsd antwortet gerade nicht.", "csmsd is not answering."),
        )
            .into_response(),
    }
}

/// `GET /stations/new` — the onboarding form.
async fn new_station_page(
    params: Result<Query<SearchParams>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let lang = language(params.ok().and_then(|Query(p)| p.lang).as_deref(), &headers);
    ui::html(stations::render_form(
        &stations::Form::blank(),
        stations::Purpose::Register,
        None,
        lang,
    ))
}

/// The checks every write passes before anything is forwarded: same origin
/// (SR-148), and the operator's own token to forward (SR-147).
fn writer(headers: &HeaderMap) -> Result<String, (StatusCode, &'static str)> {
    if !stations::same_origin(headers) {
        return Err((
            StatusCode::FORBIDDEN,
            "a change is accepted only from this page",
        ));
    }
    stations::operator_token(headers).ok_or((StatusCode::UNAUTHORIZED, "sign in first"))
}

/// `POST /stations` — register, then the installer sheet.
async fn create_station(
    State(desk): State<Arc<Page>>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<stations::Form>,
) -> Response {
    let lang = language(None, &headers);
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!("{}/v1/csmsd/stations", desk.csmsd.trim_end_matches('/'));
    match desk
        .reader
        .write(reqwest::Method::POST, &url, &token, &form.to_json())
        .await
    {
        Ok((201, body)) => ui::html(stations::render_sheet(
            form.identity.trim(),
            body["password"].as_str(),
            form.security_profile.trim(),
            form.ocpp.trim(),
            desk.ocpp_url.as_deref(),
            lang,
        )),
        Ok((422, body)) => {
            let field = body["field"].as_str().unwrap_or("body");
            let reason = body["reason"].as_str().unwrap_or("refused");
            let mut page = ui::html(stations::render_form(
                &form,
                stations::Purpose::Register,
                Some((field, reason)),
                lang,
            ));
            *page.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            page
        }
        Ok((status, _)) => {
            (StatusCode::BAD_GATEWAY, format!("csmsd answered {status}")).into_response()
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "csmsd is not answering").into_response(),
    }
}

/// `GET /station/{identity}/edit` — the change form, filled with what the
/// station is now.
async fn edit_station_page(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    params: Result<Query<SearchParams>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let lang = language(params.ok().and_then(|Query(p)| p.lang).as_deref(), &headers);
    match desk
        .reader
        .station::<station::StationPage>(&desk.csmsd, &identity)
        .await
    {
        Ok(Some(page)) => ui::html(stations::render_form(
            &stations::Form::of(&page),
            stations::Purpose::Change(&page.identity),
            None,
            lang,
        )),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "csmsd is not answering").into_response(),
    }
}

/// `POST /station/{identity}/edit` — forwarded as `PUT` with the operator's
/// token and reason (spec 021, US3); back to the card when it is accepted.
async fn edit_station(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::Form(mut form): axum::Form<stations::Form>,
) -> Response {
    let lang = language(None, &headers);
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    // The path names the station; a form cannot rename it.
    form.identity.clone_from(&identity);
    let url = format!(
        "{}/v1/csmsd/stations/{}",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).trim_start_matches("/station/")
    );
    let body = serde_json::json!({
        "station": form.to_json(),
        "reason": form.reason.trim(),
    });
    match desk
        .reader
        .write(reqwest::Method::PUT, &url, &token, &body)
        .await
    {
        Ok((200, _)) => axum::response::Redirect::to(&station::href(&identity)).into_response(),
        Ok((404, _)) => StatusCode::NOT_FOUND.into_response(),
        Ok((422, body)) => {
            let field = body["field"].as_str().unwrap_or("body");
            let reason = body["reason"].as_str().unwrap_or("refused");
            let mut page = ui::html(stations::render_form(
                &form,
                stations::Purpose::Change(&identity),
                Some((field, reason)),
                lang,
            ));
            *page.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            page
        }
        Ok((status, _)) => {
            (StatusCode::BAD_GATEWAY, format!("csmsd answered {status}")).into_response()
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "csmsd is not answering").into_response(),
    }
}

#[derive(Deserialize)]
struct Reason {
    reason: String,
}

/// `POST /station/{identity}/retire`.
async fn retire_station(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::Form(why): axum::Form<Reason>,
) -> Response {
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!(
        "{}/v1/csmsd/stations/{}/retire",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).trim_start_matches("/station/")
    );
    match desk
        .reader
        .write(
            reqwest::Method::POST,
            &url,
            &token,
            &serde_json::json!({ "reason": why.reason }),
        )
        .await
    {
        Ok((204, _)) => axum::response::Redirect::to("/stations").into_response(),
        Ok((_, body)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            body["reason"].as_str().unwrap_or("refused").to_owned(),
        )
            .into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "csmsd is not answering").into_response(),
    }
}

#[derive(Deserialize)]
struct ReserveForm {
    uid: String,
    minutes: u32,
    reason: String,
}

/// `POST /station/{identity}/reservations` — forwarded with the operator's
/// own token from this origin only, as a note is (spec 025).
async fn reserve(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<ReserveForm>,
) -> Response {
    let lang = language(None, &headers);
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!(
        "{}/v1/csmsd{}/reservations",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).replacen("/station/", "/stations/", 1)
    );
    let body =
        serde_json::json!({ "uid": form.uid, "minutes": form.minutes, "reason": form.reason });
    let answer = desk
        .reader
        .write(reqwest::Method::POST, &url, &token, &body)
        .await;
    let typed = station::Typed {
        uid: &form.uid,
        minutes: form.minutes,
        reason: &form.reason,
    };
    back_to(&identity, "reservation", answer, Some(&typed), lang)
}

/// `POST /station/{identity}/reservations/{number}/cancel`.
async fn cancel_reservation(
    State(desk): State<Arc<Page>>,
    axum::extract::Path((identity, number)): axum::extract::Path<(String, u64)>,
    headers: HeaderMap,
) -> Response {
    let lang = language(None, &headers);
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!(
        "{}/v1/csmsd{}/reservations/{number}/cancel",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).replacen("/station/", "/stations/", 1)
    );
    let answer = desk
        .reader
        .write(
            reqwest::Method::POST,
            &url,
            &token,
            &serde_json::Value::Null,
        )
        .await;
    back_to(&identity, "reservation", answer, None, lang)
}

/// Back to the section on the asset card; a refusal is a page that says why
/// and links back — the station's own word when it was the station.
fn back_to(
    identity: &str,
    section: &str,
    answer: Result<(u16, serde_json::Value), fetch::Why>,
    typed: Option<&station::Typed<'_>>,
    lang: Lang,
) -> Response {
    let back = format!("{}#{section}", station::href(identity));
    let reason = match answer {
        Ok((201 | 204 | 404, _)) => return axum::response::Redirect::to(&back).into_response(),
        Ok((_, body)) => station::refusal_text(body["reason"].as_str().unwrap_or("refused"), lang),
        Err(_) => lang
            .pick("csmsd antwortet nicht", "csmsd is not answering")
            .to_owned(),
    };
    let body = format!(
        "<div class=\"verdict tone-bad\"><span class=\"icon\" aria-hidden=\"true\">✗</span><div>\
         <h1>{title}</h1><p>{reason}</p></div></div>{again_form}<p><a href=\"{back}\">{back_label}</a></p>",
        title = match lang {
            Lang::De => format!("{identity} ist nicht reserviert"),
            Lang::En => format!("{identity} is not held"),
        },
        reason = ui::escape(&reason),
        // What was typed stays, so a changed card or time is one click.
        again_form = typed.map_or_else(String::new, |typed| format!(
            "<section class=\"card\">{}</section>",
            station::reserve_form(identity, Some(typed), lang)
        )),
        back = ui::escape(&back),
        back_label = lang.pick("Zurück zur Station", "Back to the station"),
    );
    let mut page = ui::html(ui::page(
        &ui::Shell {
            title: lang.pick("Nicht reserviert", "Not held"),
            place: lang.pick("Station", "Station"),
            lang,
            reading: true,
            script: None,
        },
        &body,
    ));
    *page.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    page
}

#[derive(Deserialize)]
struct NoteForm {
    text: String,
}

/// `POST /station/{identity}/notes` — forwarded with the operator's own token
/// (SR-156) from this origin only (SR-157); back to the notes.
async fn write_note(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::Form(note): axum::Form<NoteForm>,
) -> Response {
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!(
        "{}/v1/csmsd{}/notes",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).replacen("/station/", "/stations/", 1)
    );
    let body = serde_json::json!({ "text": note.text });
    back_to_notes(
        &identity,
        desk.reader
            .write(reqwest::Method::POST, &url, &token, &body)
            .await,
    )
}

/// `POST /station/{identity}/notes/{number}/remove`.
async fn remove_note(
    State(desk): State<Arc<Page>>,
    axum::extract::Path((identity, number)): axum::extract::Path<(String, u64)>,
    headers: HeaderMap,
) -> Response {
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!(
        "{}/v1/csmsd{}/notes/{number}/remove",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).replacen("/station/", "/stations/", 1)
    );
    let answer = desk
        .reader
        .write(
            reqwest::Method::POST,
            &url,
            &token,
            &serde_json::Value::Null,
        )
        .await;
    back_to_notes(&identity, answer)
}

/// A note written or removed goes back to the notes; a refusal says why.
fn back_to_notes(identity: &str, answer: Result<(u16, serde_json::Value), fetch::Why>) -> Response {
    match answer {
        Ok((201 | 204 | 404, _)) => {
            axum::response::Redirect::to(&format!("{}#notes", station::href(identity)))
                .into_response()
        }
        Ok((_, body)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            body["reason"].as_str().unwrap_or("refused").to_owned(),
        )
            .into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "csmsd is not answering").into_response(),
    }
}

/// `POST /station/{identity}/password` — a new password, on a new sheet.
async fn new_password(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::Form(why): axum::Form<Reason>,
) -> Response {
    let lang = language(None, &headers);
    let token = match writer(&headers) {
        Ok(token) => token,
        Err(refusal) => return refusal.into_response(),
    };
    let url = format!(
        "{}/v1/csmsd/stations/{}/password",
        desk.csmsd.trim_end_matches('/'),
        station::href(&identity).trim_start_matches("/station/")
    );
    match desk
        .reader
        .write(
            reqwest::Method::POST,
            &url,
            &token,
            &serde_json::json!({ "reason": why.reason }),
        )
        .await
    {
        Ok((200, body)) => ui::html(stations::render_sheet(
            &identity,
            body["password"].as_str(),
            "",
            "",
            desk.ocpp_url.as_deref(),
            lang,
        )),
        Ok((_, body)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            body["reason"].as_str().unwrap_or("refused").to_owned(),
        )
            .into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "csmsd is not answering").into_response(),
    }
}

/// One station (spec 012). An identity the estate does not list is the
/// shell's plain 404; nothing the browser sent is echoed (SR-139).
async fn station_page(
    State(desk): State<Arc<Page>>,
    axum::extract::Path(identity): axum::extract::Path<String>,
    params: Result<Query<Params>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    let lang = Lang::negotiate(
        params.ok().and_then(|Query(p)| p.lang).as_deref(),
        headers
            .get(axum::http::header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok()),
    );
    match desk
        .reader
        .station::<station::StationPage>(&desk.csmsd, &identity)
        .await
    {
        Ok(Some(page)) => ui::html(station::render(
            &page,
            lang,
            &desk.zone,
            (desk.clock)(),
            &desk.receipts,
        )),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(why) => {
            tracing::warn!(why = ?why, "csmsd did not answer for a station page");
            (
                StatusCode::BAD_GATEWAY,
                lang.pick(
                    "csmsd antwortet gerade nicht — die Station lässt sich nicht anzeigen.",
                    "csmsd is not answering — the station cannot be shown.",
                ),
            )
                .into_response()
        }
    }
}

/// Start `deskd`.
///
/// # Errors
///
/// A secret the estate names is unset, the zone is not one, or the socket
/// cannot be bound.
pub async fn run(options: Options) -> Result<Running> {
    let Options {
        http,
        estate,
        env,
        shutdown,
        identity,
        clock,
        witness,
    } = options;
    let operators = estate::operator_tokens(&estate.operators, &env)?;
    let token = estate.desk.token_env.resolve_in("desk.token_env", &env)?;
    let zone =
        TimeZone::new(&estate.desk.time_zone).map_err(|error| estate::EstateError::Invalid {
            key: "desk.time_zone".to_owned(),
            reason: error.to_string(),
        })?;
    let reader = Arc::new(fetch::Reader::new(token)?);
    let watch = Arc::new(Watch {
        reader: Arc::clone(&reader),
        daemons: vec![
            ("csmsd", estate.desk.csmsd.clone()),
            ("billd", estate.services.billd.clone()),
            ("roamd", estate.services.roamd.clone()),
            ("empd", estate.services.empd.clone()),
        ],
        witness: emob_service::witness::Witness::open(&witness, &estate.witness.keys)
            .with_context(|| format!("opening the witness at {}", witness.display()))?,
        clock: Arc::clone(&clock),
    });
    let desk = Arc::new(Page {
        reader,
        watch: Arc::clone(&watch),
        csmsd: estate.desk.csmsd.clone(),
        billd: estate.services.billd.clone(),
        roamd: estate.services.roamd.clone(),
        zone,
        clock,
        receipts: estate
            .receipts
            .as_ref()
            .map(|r| r.public_url.clone())
            .unwrap_or_default(),
        ocpp_url: estate.desk.ocpp_url.clone(),
    });

    // deskd keeps no state and reads what it shows when asked: once it can
    // answer, it is ready. Without a probe it would never be — an empty
    // readiness is "not ready" by design (found by `just run`, spec 012).
    let readiness = Readiness::new().expecting(PAGE);
    readiness.up(PAGE);
    let server = Server::new(identity, readiness, shutdown.clone()).with(router(desk, operators));
    let listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("binding {http}"))?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(error) = server.serve_on(listener).await {
            tracing::error!(%error, "the page stopped");
        }
    });
    let rounds = Arc::clone(&watch);
    let stopping = shutdown.clone();
    let witnessing = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(WITNESS_EVERY);
        loop {
            tokio::select! {
                _ = ticker.tick() => { rounds.round().await; }
                () = stopping.wait() => return,
            }
        }
    });
    tracing::info!(%addr, "deskd listening — Heute is at /");
    Ok(Running {
        http: addr,
        watch,
        task,
        witnessing,
        shutdown,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn sr_148_sr_157_a_write_from_another_origin_is_refused_before_it_is_forwarded() {
        use axum::http::HeaderValue;
        let mut headers = HeaderMap::new();
        // demo:token, as the browser sends it.
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Basic ZGVtbzp0b2tlbg=="),
        );
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert_eq!(writer(&headers).unwrap_err().0, StatusCode::FORBIDDEN);
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        assert_eq!(
            writer(&headers).unwrap(),
            "token",
            "the operator's own token is forwarded"
        );
    }

    use super::*;

    #[test]
    fn sr_059_the_script_is_small_same_origin_and_keeps_nothing() {
        assert!(SCRIPT.len() < 2048, "{} bytes", SCRIPT.len());
        for forbidden in [
            "http:",
            "https:",
            "//",
            "localStorage",
            "sessionStorage",
            "indexedDB",
            "document.cookie",
            "eval(",
        ] {
            assert!(!SCRIPT.contains(forbidden), "the script names {forbidden}");
        }
        assert!(SCRIPT.contains("prefers-reduced-motion"));
        assert!(SCRIPT.contains("visibilityState"));
    }
}
