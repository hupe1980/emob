//! The provider owns both sides of a change to a card's standing. A preview is
//! held only in memory, at most one per configured operator. Only a confirmed
//! change is a business fact. The book's lock covers the state check and append.

use std::collections::BTreeMap;

use emob_service::{Actor, journal::Journal};
use emob_session::TokenRef;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::cards::label;
use crate::{Book, Empd};

const HISTORY: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Lost,
    Stolen,
    SuspectedMisuse,
    Recovered,
    Cleared,
    Correction,
}

impl Reason {
    const fn permits(self, blocked: bool) -> bool {
        match self {
            Self::Lost | Self::Stolen | Self::SuspectedMisuse => blocked,
            Self::Recovered | Self::Cleared => !blocked,
            Self::Correction => true,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    reference: TokenRef,
    blocked: bool,
    reason: Reason,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Confirmation {
    pub(crate) operation: String,
    pub(crate) reference: Option<TokenRef>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Preview {
    operation: String,
    reference: TokenRef,
    visual: String,
    blocked: bool,
    reason: Reason,
    effect: &'static str,
    offline_cache: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(skip)]
    reviewed_at: OffsetDateTime,
    #[serde(skip)]
    revision: u64,
}

/// A confirmed change to a provider card, as recorded in its signed journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardChange {
    operation: String,
    pub(crate) reference: TokenRef,
    pub(crate) blocked: bool,
    reason: Reason,
    actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    reviewed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

#[derive(Serialize)]
pub(crate) struct ResultView {
    #[serde(flatten)]
    change: CardChange,
    visual: String,
}

impl ResultView {
    pub(crate) fn of(change: CardChange) -> Self {
        Self {
            visual: label(&change.reference),
            change,
        }
    }
}

impl Empd {
    pub(crate) fn apply_change(&mut self, change: CardChange) {
        let Some(token) = self.tokens.get_mut(change.reference.as_str()) else {
            return;
        };
        token.blocked = change.blocked;
        token.revision = token.revision.saturating_add(1);
        let recent = self
            .card_history
            .entry(change.reference.clone())
            .or_default();
        if recent.len() == HISTORY {
            recent.pop_back();
        }
        recent.push_front(change);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Invalid,
    Unknown,
    Stale,
    Unavailable,
}

pub(crate) type Pending = BTreeMap<Actor, Preview>;

impl<J: Journal> Book<J> {
    pub(crate) fn preview(
        &mut self,
        actor: &Actor,
        request: Request,
        now: OffsetDateTime,
    ) -> Result<Preview, Refusal> {
        if self.is_halted() {
            return Err(Refusal::Unavailable);
        }
        if !request.reason.permits(request.blocked) {
            return Err(Refusal::Invalid);
        }
        let token = self
            .state()
            .token(&request.reference)
            .ok_or(Refusal::Unknown)?;
        if token.blocked == request.blocked {
            return Err(Refusal::Stale);
        }
        let mut nonce = [0_u8; 32];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| Refusal::Unavailable)?;
        let preview = Preview {
            operation: hex::encode(nonce),
            visual: label(&request.reference),
            reference: request.reference,
            blocked: request.blocked,
            reason: request.reason,
            effect: if request.blocked {
                "refuse_online"
            } else {
                "evaluate_contract"
            },
            offline_cache: "unchanged",
            expires_at: now + Duration::minutes(5),
            reviewed_at: now,
            revision: token.revision,
        };
        self.pending.insert(actor.clone(), preview.clone());
        Ok(preview)
    }

    pub(crate) fn confirm(
        &mut self,
        actor: &Actor,
        operation: &str,
        expected_reference: Option<&TokenRef>,
        now: OffsetDateTime,
    ) -> Result<ResultView, Refusal> {
        if self.is_halted() {
            return Err(Refusal::Unavailable);
        }
        if operation.len() != 64
            || !operation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Refusal::Invalid);
        }
        let preview = self.pending.get(actor).ok_or(Refusal::Stale)?;
        if !emob_service::Token::new(&preview.operation).verify(operation)
            || expected_reference.is_some_and(|reference| *reference != preview.reference)
        {
            return Err(Refusal::Stale);
        }
        let preview = self.pending.remove(actor).ok_or(Refusal::Stale)?;
        let token = self
            .state()
            .token(&preview.reference)
            .ok_or(Refusal::Unknown)?;
        if now < preview.reviewed_at
            || now >= preview.expires_at
            || preview.revision != token.revision
            || preview.blocked == token.blocked
        {
            return Err(Refusal::Stale);
        }
        let change = CardChange {
            operation: preview.operation,
            reference: preview.reference,
            blocked: preview.blocked,
            reason: preview.reason,
            actor: actor.clone(),
            reviewed_at: preview.reviewed_at,
            at: now,
        };
        self.record_card_fact(
            actor,
            crate::Event::CardChanged {
                change: change.clone(),
            },
        )
        .map_err(|_| Refusal::Unavailable)?;
        Ok(ResultView::of(change))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Contract, TokenType, Whitelist};
    use emob_service::journal::{JournalError, Memory, Record};
    use emob_service::vault::DataKey;
    use time::macros::{date, datetime};

    const UID: &str = "04A1B2C3D4E5F6";
    const NOW: OffsetDateTime = datetime!(2026-11-18 15:00 UTC);

    fn empty() -> Empd {
        Empd::new("DE*MSP".parse().unwrap(), DataKey::from_bytes(&[7; 32]))
    }

    fn issued<J: Journal>(journal: J) -> (Book<J>, TokenRef) {
        let contract: emob_core::ContractId = "DE-MSP-C00000001-W".parse().unwrap();
        let mut book = Book::open(empty(), journal).unwrap();
        book.with(
            &Actor::Estate,
            Contract::new(contract.clone(), date!(2026 - 01 - 01)),
        )
        .unwrap();
        let card = book
            .issue_token(
                &Actor::Estate,
                &contract,
                UID,
                TokenType::Rfid,
                Whitelist::Allowed,
            )
            .unwrap();
        (book, card)
    }

    fn request(card: &TokenRef, blocked: bool) -> Request {
        Request {
            reference: card.clone(),
            blocked,
            reason: if blocked {
                Reason::Lost
            } else {
                Reason::Recovered
            },
        }
    }

    // heute-acts/SR-031, heute-acts/SR-035. A second tab replaces its operator's pending review.
    // Different operators, target substitutions and elapsed reviews cannot apply it.
    #[test]
    fn previews_are_actor_bound_subject_bound_replaced_and_time_limited() {
        let (mut book, card) = issued(Memory::new());
        let writer = Actor::operator("on duty").unwrap();
        let other = Actor::operator("relief").unwrap();
        let head = book.journal().head().unwrap();
        let old = book.preview(&writer, request(&card, true), NOW).unwrap();
        let new = book.preview(&writer, request(&card, true), NOW).unwrap();
        assert_ne!(old.operation, new.operation);
        assert_eq!(book.pending.len(), 1);
        assert_eq!(
            book.confirm(&writer, &old.operation, None, NOW).err(),
            Some(Refusal::Stale)
        );
        assert_eq!(
            book.confirm(&other, &new.operation, None, NOW).err(),
            Some(Refusal::Stale)
        );
        let wrong = book.state().reference_for("someone else");
        assert_eq!(
            book.confirm(&writer, &new.operation, Some(&wrong), NOW)
                .err(),
            Some(Refusal::Stale)
        );
        assert!(book.pending.contains_key(&writer));
        assert_eq!(
            book.confirm(&writer, &new.operation, None, new.expires_at)
                .err(),
            Some(Refusal::Stale)
        );
        let newer = book.preview(&writer, request(&card, true), NOW).unwrap();
        assert_eq!(
            book.confirm(&writer, &newer.operation, None, NOW - Duration::seconds(1))
                .err(),
            Some(Refusal::Stale)
        );
        assert_eq!(book.journal().head().unwrap(), head);
    }

    // heute-acts/SR-031. Returning to the same standing or re-issuing a card invalidates an earlier review.
    #[test]
    fn a_revision_change_invalidates_an_earlier_review_even_after_reversal() {
        let (mut book, card) = issued(Memory::new());
        let one = Actor::operator("one").unwrap();
        let two = Actor::operator("two").unwrap();
        let old = book.preview(&one, request(&card, true), NOW).unwrap();
        for blocked in [true, false] {
            let p = book.preview(&two, request(&card, blocked), NOW).unwrap();
            book.confirm(&two, &p.operation, Some(&card), NOW).unwrap();
        }
        assert_eq!(
            book.confirm(&one, &old.operation, None, NOW).err(),
            Some(Refusal::Stale)
        );
        let p = book.preview(&one, request(&card, true), NOW).unwrap();
        book.confirm(&one, &p.operation, None, NOW).unwrap();
        let restore = book.preview(&one, request(&card, false), NOW).unwrap();
        let contract = book.state().token(&card).unwrap().contract.clone();
        book.issue_token(
            &Actor::Estate,
            &contract,
            UID,
            TokenType::Rfid,
            Whitelist::Allowed,
        )
        .unwrap();
        assert!(book.state().token(&card).unwrap().blocked);
        assert_eq!(
            book.confirm(&one, &restore.operation, None, NOW).err(),
            Some(Refusal::Stale)
        );
        assert_eq!(book.state().card_history[&card].len(), 3);
    }

    // heute-acts/SR-033, heute-acts/SR-035. Keep the newest ten in the view while preserving the full signed ledger.
    #[test]
    fn history_is_newest_first_and_evicted_operations_cannot_be_applied_again() {
        let (mut book, card) = issued(Memory::new());
        let writer = Actor::operator("operator").unwrap();
        let mut committed = Vec::new();
        for i in 0..12 {
            let now = NOW + Duration::minutes(i);
            let p = book
                .preview(&writer, request(&card, i % 2 == 0), now)
                .unwrap();
            book.confirm(&writer, &p.operation, None, now).unwrap();
            committed.push(p.operation);
        }
        assert_eq!(
            book.confirm(&writer, &committed[0], None, NOW).err(),
            Some(Refusal::Stale)
        );
        let history: Vec<_> = book.state().card_history[&card]
            .iter()
            .map(|c| &c.operation)
            .collect();
        assert_eq!(history, committed.iter().rev().take(10).collect::<Vec<_>>());
        let records = book.journal().records().unwrap();
        let facts: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "card_changed")
            .collect();
        assert_eq!(facts.len(), 12);
        for fact in facts {
            assert_eq!(fact.actor, writer);
            assert!(!fact.body.contains(UID));
            assert!(!fact.body.contains("DE-MSP-C00000001-W"));
        }
    }

    struct RejectChanges(Memory);

    impl Journal for RejectChanges {
        fn append(&self, kind: &str, actor: &Actor, body: &str) -> Result<Record, JournalError> {
            if kind == "card_changed" {
                return Err(JournalError::Io {
                    path: "test".into(),
                    doing: "appended",
                    source: std::io::Error::other("disk full"),
                });
            }
            self.0.append(kind, actor, body)
        }
        fn records(&self) -> Result<Vec<Record>, JournalError> {
            self.0.records()
        }
    }

    // heute-acts/SR-036. An append failure cannot publish unrecorded standing or history.
    #[test]
    fn append_failure_preserves_standing_and_fails_closed() {
        let (mut book, card) = issued(RejectChanges(Memory::new()));
        let head = book.journal().head().unwrap();
        let writer = Actor::operator("on duty").unwrap();
        let p = book.preview(&writer, request(&card, true), NOW).unwrap();
        assert_eq!(
            book.confirm(&writer, &p.operation, None, NOW).err(),
            Some(Refusal::Unavailable)
        );
        assert!(book.is_halted());
        assert!(!book.state().token(&card).unwrap().blocked);
        assert!(!book.state().card_history.contains_key(&card));
        assert_eq!(book.journal().head().unwrap(), head);
        assert_eq!(
            book.preview(&writer, request(&card, true), NOW).err(),
            Some(Refusal::Unavailable)
        );
        assert!(book.authorize(&card, NOW.date()).is_err());
        let answer = crate::http::decide(&book, UID, NOW.date());
        assert_eq!(answer.status, crate::http::Status::Invalid);
        assert_eq!(answer.reason, "the provider is unavailable");
    }
}
