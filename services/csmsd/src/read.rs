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
//! Presence is held in memory, so after a restart "silent since" is never
//! earlier than the restart — which the page says rather than guessing.

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
}

/// The two windows the page heads with.
#[derive(Debug, Serialize)]
pub struct Windows {
    /// Today, in the estate's zone.
    pub today: Window,
    /// The seven days ending today.
    pub week: Window,
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
    /// The station.
    pub identity: String,
    /// The transaction.
    pub transaction: String,
    /// The chain's sentences.
    pub reasons: Vec<String>,
    /// The chain's typed findings.
    pub findings: Vec<emob_eichrecht::Finding>,
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
        let silent = silent(csmsd, stations, params.silent.unwrap_or(0), limit);
        let now = csmsd.now();
        Ok(csmsd.with_state(|state| {
            Self::from_state(state, now, limit, params.refused, registry_keys, silent)
        }))
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
            today: window(&state.success.window(today, today), today, today),
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
                        identity: identity.clone(),
                        transaction: transaction.clone(),
                        reasons: reasons.clone(),
                        findings: findings.clone(),
                    },
                ))
            }),
            state.refusals.len() as u64,
            limit,
        );
        Self {
            limit,
            registry_keys,
            success,
            refused,
            silent,
        }
    }
}

/// The estate's silent stations, the most recently silent first, from
/// `offset` on. Presence is about the link, so it is read on the wall clock it
/// was written with.
fn silent<J: Journal>(
    csmsd: &Csmsd<J>,
    stations: &[(String, String)],
    offset: u64,
    limit: usize,
) -> Page<StationView> {
    let wall = time::OffsetDateTime::now_utc();
    let mut silent: Vec<StationView> = stations
        .iter()
        .filter_map(|(identity, evse)| {
            let last_heard = csmsd.last_heard(identity);
            let since = silent_since(last_heard, csmsd.started(), wall)?;
            Some(StationView {
                identity: identity.clone(),
                evse: evse.clone(),
                heartbeat_secs: crate::HEARTBEAT_SECS,
                last_heard,
                silent_since: since,
            })
        })
        .collect();
    silent.sort_by(|a, b| {
        b.silent_since
            .cmp(&a.silent_since)
            .then_with(|| a.identity.cmp(&b.identity))
    });
    let total = silent.len() as u64;
    Page::from_offset(silent.into_iter(), total, offset, limit)
}

/// What the route reads: the CSMS and the estate's stations.
type Shared<J> = (Arc<Csmsd<J>>, Arc<Vec<(String, String)>>);

async fn today<J: Journal>(
    Extract((csmsd, stations)): Extract<Shared<J>>,
    params: Result<Query<Params>, QueryRejection>,
) -> Response {
    let params = match params {
        Ok(Query(params)) => params,
        Err(rejection) => return Refused::from(rejection).into_response(),
    };
    match Today::of(&csmsd, &stations, &params) {
        Ok(view) => Json(view).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The route, behind the operators. `stations` are the estate's, as
/// `(identity, evse)`.
pub fn router<J: Journal>(
    csmsd: Arc<Csmsd<J>>,
    operators: Operators,
    stations: Vec<(String, String)>,
) -> axum::Router {
    let routes = axum::Router::new()
        .route("/v1/csmsd/today", get(today::<J>))
        .with_state((csmsd, Arc::new(stations)));
    emob_service::serve::operator_only(routes, operators)
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
