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

pub mod fetch;
pub mod heute;
pub mod model;
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
    });

    let readiness = Readiness::new();
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
