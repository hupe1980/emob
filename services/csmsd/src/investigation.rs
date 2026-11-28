//! A closed decision can be reopened by its position in the append-only outcome
//! list. The projection freezes its station evidence before the open transaction
//! is dropped. A later use of the same local transaction ID cannot change it.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State as Extract};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use emob_service::journal::Journal;
use emob_service::page::{Page, limit};
use emob_session::TokenRef;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::Outcome;
use crate::journal::{Key, Open};
use crate::read::Shared;

#[derive(Debug, Default)]
pub(crate) struct Investigations {
    decisions: BTreeMap<u64, Decision>,
    by_transaction: BTreeMap<String, Vec<u64>>,
}

#[derive(Debug, Serialize)]
struct Summary {
    position: u64,
    identity: String,
    transaction: String,
    billed: bool,
    #[serde(with = "time::serde::rfc3339")]
    decided_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct Decision {
    #[serde(flatten)]
    summary: Summary,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
    observed_frames: usize,
    missing_frames: usize,
    card_reference: Option<TokenRef>,
    #[serde(skip)]
    has_receipt: bool,
    #[serde(skip)]
    evse: Option<emob_core::EvseId>,
}

impl Investigations {
    pub(crate) fn record(
        &mut self,
        position: u64,
        key: &Key,
        at: OffsetDateTime,
        billed: bool,
        open: Option<&Open>,
        has_receipt: bool,
    ) {
        use emob_ocpp::EventKind;

        let events = open.map_or(&[][..], |session| &session.transaction.events[..]);
        let summary = Summary {
            position,
            identity: key.0.clone(),
            transaction: key.1.clone(),
            billed,
            decided_at: at,
        };
        self.by_transaction
            .entry(key.1.clone())
            .or_default()
            .push(position);
        self.decisions.insert(
            position,
            Decision {
                summary,
                started_at: events
                    .iter()
                    .find(|e| e.kind == EventKind::Started)
                    .map(|e| e.at),
                ended_at: events
                    .iter()
                    .rev()
                    .find(|e| e.kind == EventKind::Ended)
                    .map(|e| e.at),
                observed_frames: events.len(),
                missing_frames: open.map_or(0, |s| s.missing_messages.len()),
                card_reference: open.and_then(|s| s.transaction.authorization.token_ref.clone()),
                has_receipt,
                evse: open.map(|s| s.transaction.evse_id.clone()),
            },
        );
    }

    /// A late token cannot establish a new payer for an already closed decision.
    pub(crate) fn has_decision(&self, key: &Key) -> bool {
        self.by_transaction.get(&key.1).is_some_and(|positions| {
            positions
                .iter()
                .any(|p| self.decisions[p].summary.identity == key.0)
        })
    }

    fn search(&self, query: &Search, limit: usize) -> Page<&Summary> {
        let positions = self
            .by_transaction
            .get(&query.transaction)
            .map_or(&[][..], Vec::as_slice);
        let cut = query.before.map_or(positions.len(), |before| {
            positions.partition_point(|p| *p < before)
        });
        Page::newest(
            positions[..cut]
                .iter()
                .map(|p| (*p, &self.decisions[p].summary)),
            positions.len() as u64,
            limit,
        )
    }

    pub(crate) fn receipt(&self, position: u64) -> Option<Key> {
        let decision = self.decisions.get(&position)?;
        decision.has_receipt.then(|| {
            (
                decision.summary.identity.clone(),
                decision.summary.transaction.clone(),
            )
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Search {
    transaction: String,
    before: Option<u64>,
    limit: Option<usize>,
}

pub(crate) async fn search<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    body: Result<Json<Search>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let query = match body {
        Ok(Json(query)) => query,
        Err(rejection) => return rejection.status().into_response(),
    };
    if query.transaction.trim().is_empty() || query.transaction.len() > 256 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let limit = match limit(query.limit) {
        Ok(limit) => limit,
        Err(refusal) => return refusal.into_response(),
    };
    csmsd.with_state(|s| Json(s.investigations.search(&query, limit)).into_response())
}

#[derive(Serialize)]
struct Detail<'a> {
    #[serde(flatten)]
    decision: &'a Decision,
    station_registered: bool,
    receipt: Option<String>,
    #[serde(flatten)]
    explanation: Explanation,
}

/// A bounded excerpt of the decision, with the size of the undisplayed tail.
#[derive(Debug, Serialize)]
pub struct Explanation {
    /// The first reasons, in decision order.
    pub reasons: Vec<String>,
    /// Typed findings, in decision order.
    pub findings: Vec<emob_eichrecht::Finding>,
    /// Additional reason sentences beyond the excerpt.
    pub omitted_reasons: usize,
    /// Additional typed findings beyond the excerpt.
    pub omitted_findings: usize,
}

impl Explanation {
    pub(crate) fn of(reasons: &[String], findings: &[emob_eichrecht::Finding]) -> Self {
        const MAX: usize = 12;
        Self {
            reasons: reasons.iter().take(MAX).map(|s| excerpt(s)).collect(),
            findings: findings
                .iter()
                .take(MAX)
                .map(|f| emob_eichrecht::Finding {
                    kind: f.kind,
                    takes: f.takes,
                    detail: excerpt(&f.detail),
                })
                .collect(),
            omitted_reasons: reasons.len().saturating_sub(MAX),
            omitted_findings: findings.len().saturating_sub(MAX),
        }
    }
}

fn excerpt(text: &str) -> String {
    const MAX: usize = 512;
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(MAX).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

pub(crate) async fn session<J: Journal>(
    Extract((csmsd, _)): Extract<Shared<J>>,
    Path(value): Path<String>,
) -> Response {
    let Ok(position) = value.parse::<u64>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    csmsd.with_state(|s| {
        let Some(decision) = s.investigations.decisions.get(&position) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let (reasons, findings) = match usize::try_from(position)
            .ok()
            .and_then(|p| s.outcomes.get(p))
        {
            Some(Outcome::Refused {
                reasons, findings, ..
            }) => (reasons.as_slice(), findings.as_slice()),
            _ => (&[][..], &[][..]),
        };
        Json(Detail {
            decision,
            station_registered: s
                .registry
                .get(&decision.summary.identity)
                .is_some_and(|station| {
                    decision
                        .evse
                        .as_ref()
                        .is_none_or(|evse| *evse == station.evse)
                }),
            receipt: s
                .investigations
                .receipt(position)
                .and_then(|key| csmsd.filed_receipt_path(s, &key)),
            explanation: Explanation::of(reasons, findings),
        })
        .into_response()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Event, Presented, Receipt, State, apply};
    use crate::{Answer, Status};
    use emob_ocpp::TransactionEvent;
    use time::macros::datetime;

    fn close(state: &mut State, station: &str, reason: &str, at: OffsetDateTime) {
        apply(
            state,
            Event::Noted {
                closes: Some((station.into(), "7".into())),
                outcome: Outcome::Refused {
                    identity: station.into(),
                    transaction: "7".into(),
                    reasons: vec![reason.into()],
                    findings: vec![],
                },
                at,
            },
        );
    }

    #[test]
    fn sr_030_sr_032_reused_ids_keep_the_original_evidence_and_receipt() {
        let at = datetime!(2026-11-25 10:00 UTC);
        let mut state = State::new(emob_core::TimeZone::utc());
        let point: emob_core::EvseId = "DE*ABC*E00001".parse().unwrap();
        let key = ("CP-1".to_owned(), "7".to_owned());
        apply(
            &mut state,
            Event::Authorized {
                station: key.0.clone(),
                transaction: Some(key.1.clone()),
                at,
                answer: Answer {
                    status: Status::Accepted,
                    reason: "active contract".into(),
                    token: Some(Presented {
                        issuer: emob_core::PartyId::new("DE", "MSP").unwrap(),
                        uid: "04FF223344".into(),
                        token_type: emob_roam::TokenType::Rfid,
                        contract_id: "DEMSP000000001".parse().unwrap(),
                    }),
                    retail: None,
                    reference: Some("a".repeat(64)),
                },
            },
        );
        apply(
            &mut state,
            Event::Observed {
                station: key.0.clone(),
                transaction: key.1.clone(),
                evse: point.clone(),
                event: TransactionEvent::started(at, vec![]),
                missing: vec![1],
            },
        );
        let receipt = |signature: &str| Event::Receipted {
            station: key.0.clone(),
            transaction: key.1.clone(),
            receipt: Receipt {
                digest: "receipt-digest".into(),
                evse: point.clone(),
                at: Some(at),
                signed: vec![signature.into()],
            },
        };
        apply(&mut state, receipt("first"));
        close(&mut state, &key.0, "first decision", at);
        // A second journalled attempt under the same key cannot change a link
        // the driver has already received, even on replay of an older journal.
        apply(&mut state, receipt("second"));
        close(
            &mut state,
            &key.0,
            "second decision",
            at + time::Duration::hours(1),
        );
        let first = &state.investigations.decisions[&0];
        let second = &state.investigations.decisions[&1];
        assert_eq!(first.started_at, Some(at));
        assert_eq!(first.ended_at, None);
        assert_eq!((first.observed_frames, first.missing_frames), (1, 1));
        assert_eq!(
            first.card_reference.as_ref().unwrap().as_str(),
            "a".repeat(64)
        );
        assert_eq!((second.started_at, second.observed_frames), (None, 0));
        assert!(second.card_reference.is_none());
        assert_eq!(state.investigations.receipt(0), Some(key));
        assert_eq!(state.investigations.receipt(1), None);
        let issued = &state.receipts["receipt-digest"];
        assert_eq!(issued.receipt.signed, ["first"]);
        assert!(
            matches!(&issued.verdict, Some(crate::journal::Verdict::Refused {reasons,..}) if reasons == &["first decision"])
        );
    }

    #[test]
    fn sr_033_exact_search_preserves_station_identity_and_cursor_under_new_arrivals() {
        let at = datetime!(2026-11-25 10:00 UTC);
        let mut state = State::new(emob_core::TimeZone::utc());
        close(&mut state, "CP-1", "refused", at);
        close(&mut state, "CP-2", "refused", at);
        let mut query = Search {
            transaction: "7".into(),
            limit: Some(1),
            before: None,
        };
        let first = state.investigations.search(&query, 1);
        assert_eq!(first.total, 2);
        assert_eq!(first.items[0].identity, "CP-2");
        query.before = first.older;
        close(&mut state, "CP-3", "refused", at);
        let older = state.investigations.search(&query, 1);
        assert_eq!(older.total, 3);
        assert_eq!(older.items[0].identity, "CP-1");
        assert_eq!(older.older, None);
        query.transaction = "07".into();
        assert_eq!(state.investigations.search(&query, 1).total, 0);
        let many = vec!["ü".repeat(600); 50];
        let excerpt = Explanation::of(&many, &[]);
        assert_eq!(excerpt.reasons.len(), 12);
        assert_eq!(excerpt.omitted_reasons, 38);
        assert_eq!(excerpt.reasons[0], format!("{}…", "ü".repeat(512)));
    }
}
