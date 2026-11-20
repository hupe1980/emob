//! What `csmsd` decided, as facts, and the state that is their fold.
//!
//! Before this module the CDR ledger, the outcome list and the 1.6 counter were
//! `Mutex`es a restart emptied: a session settled at 23:59 and a crash at
//! 00:00 was a kilowatt-hour nobody would ever bill, and the next 1.6 station to
//! start a transaction got id `1` again. Now each is a fact appended before the
//! station is answered, and the state is what the journal says it is.
//!
//! The one view that is **not** journalled is `ocpp-kit`'s operational ledger:
//! arrival, sequence and retries, with meter values as `f64`. It answers a
//! question about the link, not about money, and restarting it empty only
//! loses duplicate detection for messages in flight — which `CdrLedger`'s
//! idempotence on the record key backs up.

use std::collections::{BTreeMap, BTreeSet};

use emob_cdr::{Cdr, CdrLedger};
use emob_core::{ContractId, EvseId, PartyId};
use emob_ocpp::{Transaction, TransactionEvent};
use emob_service::journal::Kinded;
use serde::{Deserialize, Serialize};

use emob_core::TimeZone;
use emob_session::success::{Cause, Daily, Ending, cause_of_refusal};

use crate::Outcome;
use crate::authorize::Answer;

/// A transaction, by the station that runs it and the id it carries.
pub type Key = (String, String);

/// One fact `csmsd` decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum Event {
    /// A 1.6 transaction id was handed out.
    Allocated {
        /// The id.
        id: i32,
    },
    /// `empd` answered whether a token may charge — or did not, which is
    /// recorded as the `Invalid` it was taken to mean.
    Authorized {
        /// Which station asked.
        station: String,
        /// The transaction it was asked for, when it was asked at a start.
        transaction: Option<String>,
        /// What was answered, with its reason.
        answer: Answer,
        /// When `csmsd` decided it — the instant that dates the attempt it
        /// ends (spec 007).
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A billable transaction event was accepted.
    Observed {
        /// Which station.
        station: String,
        /// Which transaction.
        transaction: String,
        /// The point it was provisioned as.
        evse: EvseId,
        /// The event.
        event: TransactionEvent,
        /// `seqNo`s the operational ledger said never arrived before it.
        #[serde(default)]
        missing: Vec<i32>,
    },
    /// A message the station had already delivered arrived again.
    Retried {
        /// Which station.
        station: String,
        /// Which transaction.
        transaction: String,
    },
    /// A transaction reached a record — and, when somebody pays for it, the
    /// deliveries it owes, in the same fact so no restart can separate them.
    Settled {
        /// Which station.
        station: String,
        /// Which transaction.
        transaction: String,
        /// The record.
        cdr: Box<Cdr>,
        /// What it meant for the operator queue.
        outcome: Outcome,
        /// The deliveries the record owes.
        deliveries: Vec<Delivery>,
        /// When `csmsd` decided it — the instant that dates the attempt it
        /// ends (spec 007).
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A receipt was issued for a transaction about to settle or be refused
    /// (spec 003).
    ///
    /// Recorded **before** the verdict fact, and idempotent: the token is a
    /// function of the transaction's key, so a settlement the station retries
    /// issues the same receipt again rather than a second one.
    Receipted {
        /// Which station.
        station: String,
        /// Which transaction.
        transaction: String,
        /// What the page rests on.
        receipt: Receipt,
    },
    /// Something an operator has to read that settled nothing.
    Noted {
        /// The transaction it closes, when it closes one.
        closes: Option<Key>,
        /// What happened.
        outcome: Outcome,
        /// When `csmsd` decided it — the instant that dates the attempt it
        /// ends (spec 007).
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A receiver answered `2xx`.
    Delivered {
        /// The delivery id (the CDR key).
        id: String,
        /// To whom.
        to: Target,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A receiver refused the delivery for good; a person reads why.
    Refused {
        /// The delivery id.
        id: String,
        /// To whom.
        to: Target,
        /// What the receiver said.
        reason: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
}

impl Kinded for Event {
    fn kind(&self) -> &'static str {
        match self {
            Self::Allocated { .. } => "allocated",
            Self::Authorized { .. } => "authorized",
            Self::Observed { .. } => "observed",
            Self::Retried { .. } => "retried",
            Self::Settled { .. } => "settled",
            Self::Receipted { .. } => "receipted",
            Self::Noted { .. } => "noted",
            Self::Delivered { .. } => "delivered",
            Self::Refused { .. } => "refused",
        }
    }
}

/// What a driver's receipt rests on (spec 003): the signed records the chain
/// read, and the SHA-256 of the token that opens it — never the token (SR-060).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// `hex(SHA-256(token))`.
    pub digest: String,
    /// The point the session ran at.
    pub evse: EvseId,
    /// The instant the key registry is consulted for — the session's start —
    /// or `None` when no signed value reached the chain.
    #[serde(with = "time::serde::rfc3339::option")]
    pub at: Option<time::OffsetDateTime>,
    /// The OCMF records the chain read, verbatim.
    pub signed: Vec<String>,
}

/// What a receipt says about its session, once the session is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Billed: the record in the ledger.
    Billed(emob_cdr::CdrKey),
    /// Refused, with the chain's sentences and its typed findings.
    Refused {
        /// One sentence per problem.
        reasons: Vec<String>,
        /// The findings, each with the paragraph it rests on.
        findings: Vec<emob_eichrecht::Finding>,
    },
}

/// One issued receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    /// The transaction.
    pub key: Key,
    /// What it rests on.
    pub receipt: Receipt,
    /// `None` between the receipt and the verdict; such a receipt is not shown.
    pub verdict: Option<Verdict>,
}

/// Whom a delivery is owed to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// The invoice.
    Billd,
    /// The partner.
    Roamd,
    /// A system outside the estate, by its `[[subscribers]] name`.
    Subscriber(String),
}

impl Target {
    /// The two receivers inside the estate, in the order they are queued.
    pub const ESTATE: [Self; 2] = [Self::Billd, Self::Roamd];

    /// Its name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Billd => "billd",
            Self::Roamd => "roamd",
            Self::Subscriber(name) => name,
        }
    }
}

/// One record owed to one daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    /// The CDR key — the event's `id`, the `webhook-id` and the receiver's
    /// idempotency key.
    pub id: String,
    /// To whom.
    pub to: Target,
    /// The `CloudEvent`, already encoded, so every attempt sends the same bytes.
    pub body: String,
}

/// Where a delivery stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeliveryState {
    /// Owed, and retried until answered.
    Pending,
    /// Answered `2xx`.
    Delivered {
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// Refused for good.
    Refused {
        /// Why.
        reason: String,
    },
}

/// The token a transaction bills against, as `empd` presented it.
///
/// Travels to `billd` and `roamd`, which rebuild `emob_roam::RoamingToken`
/// through its validating constructor rather than trusting this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presented {
    /// The provider that issued it.
    pub issuer: PartyId,
    /// The uid the card carries.
    pub uid: String,
    /// What was presented.
    pub token_type: emob_roam::TokenType,
    /// The contract it bills against.
    pub contract_id: ContractId,
}

/// A transaction a station has opened and not yet closed.
#[derive(Debug, Clone)]
pub struct Open {
    /// What `emob-ocpp` assembles.
    pub transaction: Transaction,
    /// Messages delivered twice.
    pub retried_messages: usize,
    /// `seqNo`s that never arrived.
    pub missing_messages: Vec<i32>,
}

/// `csmsd`'s state: the fold of its journal.
#[derive(Debug)]
pub struct State {
    /// Transactions open now.
    pub inflight: BTreeMap<Key, Open>,
    /// The billable view.
    pub cdrs: CdrLedger,
    /// Everything that happened, in order.
    pub outcomes: Vec<Outcome>,
    /// The next 1.6 transaction id.
    pub next_1_6: i32,
    /// The answer each transaction started under.
    pub authorized: BTreeMap<Key, Answer>,
    /// Transactions whose start was refused: no event of theirs is billed.
    pub refused: BTreeSet<Key>,
    /// Every authorisation answer, in order — the operator's "why was this
    /// card turned away".
    pub answers: Vec<(String, Answer)>,
    /// The outbox.
    pub outbox: BTreeMap<(String, Target), (Delivery, DeliveryState)>,
    /// The zone an attempt's day is read in — the estate's `[desk]
    /// time_zone` (spec 007).
    pub zone: TimeZone,
    /// The charging success rate per day in [`State::zone`], folded from the
    /// same facts that bill.
    pub success: Daily,
    /// The position in [`State::outcomes`] of every refusal, ascending — what
    /// the operator view pages without scanning the outcomes (SR-091).
    pub refusals: Vec<u64>,
    /// The transaction each billed record's receipt is filed under, by CDR
    /// key (FR-003).
    pub receipt_for: BTreeMap<String, Key>,
    /// Receipts by the digest of their token (spec 003).
    pub receipts: BTreeMap<String, Issued>,
    /// The digest each transaction's receipt is filed under.
    pub receipt_of: BTreeMap<Key, String>,
}

impl State {
    /// An empty state — what a fresh journal folds from — whose days are
    /// read in `zone`.
    #[must_use]
    pub fn new(zone: TimeZone) -> Self {
        Self {
            inflight: BTreeMap::new(),
            cdrs: CdrLedger::new(),
            outcomes: Vec::new(),
            next_1_6: 1,
            authorized: BTreeMap::new(),
            refused: BTreeSet::new(),
            answers: Vec::new(),
            outbox: BTreeMap::new(),
            zone,
            success: Daily::default(),
            refusals: Vec::new(),
            receipts: BTreeMap::new(),
            receipt_of: BTreeMap::new(),
            receipt_for: BTreeMap::new(),
        }
    }

    /// Record one attempt, on the day `at` falls on in the estate's zone.
    fn attempt(&mut self, at: time::OffsetDateTime, point: &str, ending: Ending) {
        let day = self.zone.local(at).date;
        self.success.record(day, point, ending);
    }

    /// Keep an outcome, indexing it when it is a refusal.
    fn keep(&mut self, outcome: Outcome) {
        if matches!(outcome, Outcome::Refused { .. }) {
            self.refusals.push(self.outcomes.len() as u64);
        }
        self.outcomes.push(outcome);
    }

    /// Deliveries still owed.
    pub fn pending(&self) -> impl Iterator<Item = &Delivery> {
        self.outbox
            .values()
            .filter(|(_, state)| *state == DeliveryState::Pending)
            .map(|(delivery, _)| delivery)
    }
}

/// Fold one fact.
pub fn apply(state: &mut State, event: Event) {
    match event {
        Event::Allocated { id } => state.next_1_6 = id.saturating_add(1),
        Event::Authorized {
            station,
            transaction,
            answer,
            at,
        } => {
            if let Some(transaction) = transaction {
                let key = (station.clone(), transaction);
                if answer.is_accepted() {
                    // A 2.x transaction may open before its token is
                    // presented; the authorisation reaches it either way.
                    if let Some(open) = state.inflight.get_mut(&key) {
                        open.transaction.authorization = answer.authorization();
                    }
                    state.authorized.insert(key, answer.clone());
                } else {
                    // One attempt per transaction the provider turned away. The
                    // 1.6 pre-start `Authorize` carries no transaction and is not
                    // counted: the start that follows asks again (SR-058).
                    state.attempt(at, &key.0, Ending::Failed(Cause::Authorisation));
                    state.refused.insert(key);
                }
            }
            state.answers.push((station, answer));
        }
        Event::Observed {
            station,
            transaction,
            evse,
            event,
            missing,
        } => {
            let key = (station, transaction);
            // Journalled only after the id parsed; a record that does not is
            // not one this build wrote, and the chain check has already
            // refused an edited file.
            let Ok(id) = key.1.parse::<emob_core::SessionId>() else {
                return;
            };
            let authorization = state
                .authorized
                .get(&key)
                .map_or_else(emob_session::Authorization::ad_hoc, Answer::authorization);
            let open = state.inflight.entry(key.clone()).or_insert_with(|| Open {
                transaction: Transaction::new(id, evse, authorization),
                retried_messages: 0,
                missing_messages: Vec::new(),
            });
            open.missing_messages.extend(missing);
            open.transaction.events.push(event);
        }
        Event::Retried {
            station,
            transaction,
        } => {
            if let Some(open) = state.inflight.get_mut(&(station, transaction)) {
                open.retried_messages += 1;
            }
        }
        Event::Receipted {
            station,
            transaction,
            receipt,
        } => {
            let key = (station, transaction);
            state.receipt_of.insert(key.clone(), receipt.digest.clone());
            let verdict = state
                .receipts
                .get(&receipt.digest)
                .and_then(|held| held.verdict.clone());
            state.receipts.insert(
                receipt.digest.clone(),
                Issued {
                    key,
                    receipt,
                    verdict,
                },
            );
        }
        Event::Settled {
            station,
            transaction,
            cdr,
            outcome,
            deliveries,
            at,
        } => {
            let key = (station.clone(), transaction.clone());
            if let Some(issued) = state
                .receipt_of
                .get(&key)
                .and_then(|digest| state.receipts.get_mut(digest))
            {
                issued.verdict = Some(Verdict::Billed(cdr.key.clone()));
                state.receipt_for.insert(cdr.key.to_string(), key);
            }
            state.inflight.remove(&(station.clone(), transaction));
            // A record stored is a success; a retry the ledger already held is
            // not a second attempt; a conflict is a record nobody may bill.
            match &outcome {
                Outcome::Settled { .. } => state.attempt(at, &station, Ending::Billed),
                Outcome::Refused { .. } => {
                    state.attempt(at, &station, Ending::Failed(Cause::Evidence));
                }
                _ => {}
            }
            let _ = state.cdrs.accept(*cdr);
            state.keep(outcome);
            for delivery in deliveries {
                state.outbox.insert(
                    (delivery.id.clone(), delivery.to.clone()),
                    (delivery, DeliveryState::Pending),
                );
            }
        }
        Event::Noted {
            closes,
            outcome,
            at,
        } => {
            if let (
                Some(key),
                Outcome::Refused {
                    reasons, findings, ..
                },
            ) = (&closes, &outcome)
                && let Some(issued) = state
                    .receipt_of
                    .get(key)
                    .and_then(|digest| state.receipts.get_mut(digest))
            {
                issued.verdict = Some(Verdict::Refused {
                    reasons: reasons.clone(),
                    findings: findings.clone(),
                });
            }
            if let Some(key) = closes {
                let open = state.inflight.remove(&key);
                if let Outcome::Refused { findings, .. } = &outcome {
                    let events = open
                        .as_ref()
                        .map_or(&[][..], |open| &open.transaction.events[..]);
                    let stopped = events.iter().rev().find_map(|event| event.stopped_because);
                    let nothing_measured = events.iter().all(|event| event.signed.is_empty())
                        || (!findings.is_empty()
                            && findings
                                .iter()
                                .all(|f| f.kind == emob_eichrecht::FindingKind::NoEnergy));
                    let cause = cause_of_refusal(stopped, nothing_measured);
                    state.attempt(at, &key.0, Ending::Failed(cause));
                }
            }
            state.keep(outcome);
        }
        Event::Delivered { id, to, at } => {
            if let Some(entry) = state.outbox.get_mut(&(id, to)) {
                entry.1 = DeliveryState::Delivered { at };
            }
        }
        Event::Refused { id, to, reason, .. } => {
            if let Some(entry) = state.outbox.get_mut(&(id, to)) {
                entry.1 = DeliveryState::Refused { reason };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    const AT: time::OffsetDateTime = datetime!(2026-11-18 10:00 UTC);

    fn berlin() -> State {
        State::new(TimeZone::new("Europe/Berlin").unwrap())
    }

    fn refused(closes: Option<Key>, at: time::OffsetDateTime) -> Event {
        Event::Noted {
            closes,
            outcome: Outcome::Refused {
                identity: "CP-1".into(),
                transaction: "7".into(),
                reasons: vec!["record 2: the signature does not verify".into()],
                findings: Vec::new(),
            },
            at,
        }
    }

    fn today(state: &State) -> emob_session::success::Tally {
        state
            .success
            .window(date!(2026 - 11 - 18), date!(2026 - 11 - 18))
    }

    #[test]
    fn sr_058_one_driver_turned_away_is_one_attempt_and_a_note_is_none() {
        let mut state = berlin();
        // 1.6 asks before the start and again at it; one driver, one attempt.
        apply(
            &mut state,
            Event::Authorized {
                station: "CP-1".into(),
                transaction: None,
                answer: Answer::invalid("blocked"),
                at: AT,
            },
        );
        apply(
            &mut state,
            Event::Authorized {
                station: "CP-1".into(),
                transaction: Some("6".into()),
                answer: Answer::invalid("blocked"),
                at: AT,
            },
        );
        // A refusal noted mid-transaction closes nothing and is no attempt…
        apply(&mut state, refused(None, AT));
        assert_eq!(today(&state).estate.attempted, 1);
        // …and the one that closes a transaction is.
        apply(&mut state, refused(Some(("CP-1".into(), "7".into())), AT));
        let tally = today(&state);
        let rate = &tally.points["CP-1"];
        assert_eq!(rate.attempted, 2);
        assert_eq!(rate.billed, 0);
        assert_eq!(rate.failed_by(Cause::Authorisation), 1);
        // No events were observed for it, so nothing was measured — but nobody
        // said they stopped it, so it is the evidence's.
        assert_eq!(rate.failed_by(Cause::Evidence), 1);
        // Both refusals are indexed; the authorisation answer is no outcome.
        assert_eq!(state.refusals, [0, 1]);
    }

    #[test]
    fn sr_093_an_attempt_counts_once_on_its_day_in_the_estate_zone_and_replays_the_same() {
        // 23:30 UTC on the 18th is 00:30 on the 19th in Berlin.
        let late = datetime!(2026-11-18 23:30 UTC);
        let facts = vec![
            refused(Some(("CP-1".into(), "7".into())), AT),
            refused(Some(("CP-1".into(), "8".into())), late),
        ];
        let mut state = berlin();
        for fact in facts.clone() {
            apply(&mut state, fact);
        }
        assert_eq!(today(&state).estate.attempted, 1);
        let next = state
            .success
            .window(date!(2026 - 11 - 19), date!(2026 - 11 - 19));
        assert_eq!(next.estate.attempted, 1);
        let both = state
            .success
            .window(date!(2026 - 11 - 13), date!(2026 - 11 - 19));
        assert_eq!(
            both.estate.attempted, 2,
            "no attempt is counted in two days"
        );
        // In UTC the same facts fall on one day: the zone, not the host, decides.
        let mut utc = State::new(TimeZone::utc());
        let mut replay = berlin();
        for fact in facts {
            apply(&mut utc, fact.clone());
            apply(&mut replay, fact);
        }
        assert_eq!(utc.success.len(), 1);
        assert_eq!(replay.success, state.success);
    }
}
