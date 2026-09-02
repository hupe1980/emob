//! The CSMS: what a charging station connects to.
//!
//! # Two ledgers, side by side, doing different jobs
//!
//! A CSMS has to answer two questions about the same traffic, and conflating
//! them is how a platform bills a number nothing signed.
//!
//! **Did the traffic arrive?** — every event accounted for, the sequence
//! complete, a retry recognised as a retry. That is `ocpp-kit`'s
//! [`ocpp_kit::csms::ledger::Ledger`], fed from its version-neutral
//! [`ocpp_kit::csms::events`] view. It carries meter values as `f64`,
//! which is exactly right for the question it answers.
//!
//! **What may be billed?** — the signed OCMF register, in exact decimal,
//! through the Eichrecht chain. That is `emob-ocpp` into `emob-eichrecht` into
//! `emob-cdr`.
//!
//! The two run **beside** each other here, never one instead of the other, and
//! the type system keeps them apart: nothing in the billing path can see an
//! `f64`, because [`emob_ocpp::TransactionEvent`] has no field for one.
//!
//! # What is thin, and why that is the point
//!
//! Everything below is sockets, routing and bookkeeping. The parts that could
//! be *wrong* — which field holds the signed data in 1.6, whether a reading is
//! clock-aligned, what a `chargingState` means, whether a retry is a second
//! reading — are in `emob-ocpp` and under test there. A daemon is the worst
//! place to keep a rule, because CI does not run it.
//!
//! # The one piece of protocol the daemon owns
//!
//! OCPP 1.6 assigns a transaction id in the **response** to `StartTransaction`,
//! not in the request — so the CSMS allocates it, and every later message
//! carries it back. 2.x has the station allocate it instead. That asymmetry has
//! to live somewhere with state, which is here.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use emob_cdr::{Acceptance, CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::{Direction, PartyId};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_ocpp::{Transaction, TransactionEvent};
use emob_session::Authorization;
use emob_tariff::Tariff;
use ocpp_kit::csms::events::{Observed, WarningKind, observe_v16, observe_v21, observe_v201};
use ocpp_kit::csms::ledger::{Ingested, Ledger};
use ocpp_kit::engine::IncomingRequest;
use ocpp_kit::rpc::CallError;
use ocpp_kit::transport::{BoxFuture, Ctx, Handler};
use ocpp_kit::types::{DateTime, Identity};
use ocpp_kit::{RawValue, Version, v1_6, v2_0_1, v2_1};

pub mod provisioning;

pub use provisioning::{ChargePoint, Provisioning};

/// What became of one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A CDR was built and the ledger accepted it.
    Settled {
        /// Which record.
        key: String,
        /// How much it billed.
        energy: String,
        /// How many duplicate records the transport delivered on the way.
        ///
        /// Zero on a quiet link. A retry is how OCPP guarantees delivery and is
        /// not a fault — but a link that retries constantly is one an operator
        /// wants to know about, and the number is invisible once the duplicates
        /// are dropped.
        retries: usize,
    },
    /// The transaction reached no billable record, and why.
    ///
    /// Never a silent drop: the fleet simulator's rule applies to the daemon
    /// too — every kilowatt-hour a meter moved either reaches a settled record
    /// or is refused with a reason somebody can act on.
    Refused {
        /// Which station.
        identity: String,
        /// Which transaction.
        transaction: String,
        /// What the chain said, in the order it said it.
        reasons: Vec<String>,
    },
    /// The ledger already held this record, so nothing changed and nothing is
    /// billed twice.
    ///
    /// A **success**, and reported apart from [`Self::Settled`] rather than
    /// folded into it: the money was recognised at the first offer, so counting
    /// this one would double it, and reporting it as a refusal would put a
    /// settled session in the operator's failure queue. OCPP guarantees
    /// delivery by retrying, so this is the ordinary shape of a flaky link
    /// rather than a fault.
    AlreadySettled {
        /// Which record.
        key: String,
        /// What it billed, when it was first accepted.
        energy: String,
    },
    /// A station is signing with a key it was not provisioned with.
    ///
    /// Not a refusal on its own — the chain will refuse the session anyway, by
    /// its own rules and against the registry. It is reported separately
    /// because it has a **different fix**: a meter was swapped and nobody told
    /// the registry, and every session from this station will be unbillable
    /// until somebody does. Catching that on the first signed value rather than
    /// after a week of them is the difference between an alert and an audit.
    KeyMismatch {
        /// Which station.
        identity: String,
        /// What it claims to sign with.
        claimed: String,
    },
}

/// The CSMS.
pub struct Csmsd {
    party: PartyId,
    registry: KeyRegistry,
    tariff: Tariff,
    /// Transactions the stations have opened and not yet closed.
    inflight: Mutex<BTreeMap<(Identity, String), Transaction>>,
    /// The operational view: arrival, sequence, retries. Meter values as `f64`,
    /// which is right for the question it answers and never leaves this field.
    operational: Mutex<Ledger>,
    /// The billable view.
    cdrs: Mutex<CdrLedger>,
    outcomes: Mutex<Vec<Outcome>>,
    /// The next transaction id to hand a 1.6 station.
    next_1_6_transaction: Mutex<i32>,
}

impl Csmsd {
    /// A CSMS for a provisioned fleet.
    #[must_use]
    pub fn new(party: PartyId, registry: KeyRegistry, tariff: Tariff) -> Self {
        Self {
            party,
            registry,
            tariff,
            inflight: Mutex::new(BTreeMap::new()),
            operational: Mutex::new(Ledger::default()),
            cdrs: Mutex::new(CdrLedger::new()),
            outcomes: Mutex::new(Vec::new()),
            next_1_6_transaction: Mutex::new(1),
        }
    }

    /// The records that settled.
    #[must_use]
    pub fn settled(&self) -> usize {
        lock(&self.cdrs).len()
    }

    /// Everything that happened, in order.
    #[must_use]
    pub fn outcomes(&self) -> Vec<Outcome> {
        lock(&self.outcomes).clone()
    }

    /// The CDR ledger, for a caller that wants the records themselves.
    ///
    /// Infallible: a poisoned mutex is recovered from rather than reported as an
    /// absent ledger. See [`lock`](fn.lock.html).
    pub fn with_cdrs<T>(&self, f: impl FnOnce(&CdrLedger) -> T) -> T {
        f(&lock(&self.cdrs))
    }

    /// Route one billable event into its transaction, settling it when the
    /// station says the transaction is over.
    fn accept(&self, ctx: &Ctx, transaction_id: &str, event: TransactionEvent) {
        let identity = ctx.identity();
        // The charge point this station was provisioned as, hung on the session
        // by the authenticator. A connection that reached a handler without one
        // is a deployment that skipped the binding, and it must not produce a
        // session attributed to a point nobody named.
        let Some(point) = ctx.session::<ChargePoint>() else {
            return;
        };
        let closing = event.kind == emob_ocpp::EventKind::Ended;

        let mut inflight = lock(&self.inflight);
        let key = (identity.clone(), transaction_id.to_owned());
        let transaction = inflight.entry(key.clone()).or_insert_with(|| {
            Transaction::new(
                transaction_id.parse().unwrap_or_else(|_| {
                    // A station id this build cannot parse still has to reach a
                    // named refusal rather than a panic.
                    "unparseable-transaction".parse().expect("a literal id")
                }),
                point.evse_id.clone(),
                Authorization::ad_hoc(),
            )
        });
        transaction.events.push(event);

        if closing && let Some(finished) = inflight.remove(&key) {
            drop(inflight);
            self.settle(identity, transaction_id, point, &finished);
        }
    }

    /// The chain, exactly as `emob-sim` drives it: assemble, verify, price,
    /// accept.
    fn settle(
        &self,
        identity: &Identity,
        transaction_id: &str,
        point: &ChargePoint,
        transaction: &Transaction,
    ) {
        let refuse = |reasons: Vec<String>| {
            lock(&self.outcomes).push(Outcome::Refused {
                identity: identity.to_string(),
                transaction: transaction_id.to_owned(),
                reasons,
            });
        };

        // A tariff the point may not offer is refused before it prices
        // anything. `[AFIR Art. 5(4)]` is a rule about the *pairing* of a
        // tariff with a charge point, and a backend that rates first and checks
        // later has already produced the number it may not charge.
        let conformance = emob_tariff::check_afir(&self.tariff, point.rated_power_kw);
        if !conformance.is_lawful() {
            return refuse(conformance.reasons().collect());
        }

        let assembled = match transaction.assemble(Direction::Import) {
            Ok(assembled) => assembled,
            Err(error) => return refuse(vec![error.to_string()]),
        };

        // What the station *claims* it signs with, against what it was
        // provisioned with. Neither decides anything — the verification below
        // uses the registry — but a mismatch has its own fix and its own
        // urgency.
        self.check_claimed_keys(identity, transaction, &assembled);

        // Against the **registry**, never against the key the station sent
        // beside the record: a key arriving on the same socket proves only that
        // whoever holds the socket owns a private key.
        let evidence = Evidence::assemble(
            &assembled.records,
            &self.registry,
            assembled.session.started_at,
        );

        let cdr =
            CdrBuilder::from_session(&assembled.session, Direction::Import).and_then(|builder| {
                builder
                    .key(
                        self.party.clone(),
                        format!("{identity}-{transaction_id}")
                            .parse()
                            .unwrap_or_else(|_| "cdr".parse().expect("a literal id")),
                    )
                    .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
                    .rated_with(&self.tariff)
                    .build()
            });

        match cdr {
            Ok(cdr) => {
                let energy = cdr.total_energy.to_string();
                let key = cdr.key.to_string();
                let retries = assembled.duplicates_dropped;
                let stored = lock(&self.cdrs).accept(cdr);
                {
                    let mut outcomes = lock(&self.outcomes);
                    outcomes.push(outcome_of(
                        &stored,
                        Accepted {
                            key: &key,
                            energy: &energy,
                            retries,
                            identity: &identity.to_string(),
                            transaction: transaction_id,
                        },
                    ));
                }
            }
            Err(error) => {
                let mut reasons = vec![error.to_string()];
                reasons.extend(evidence.reasons());
                refuse(reasons);
            }
        }
    }

    /// Compare every claimed key against the registry, and report a mismatch.
    ///
    /// `[OCA SMV §3.2.2]` has a station send its own public key beside each
    /// signed value, and `[OCMF §Relation of Serial Numbers]` says the binding
    /// travels out of band — so the claim decides nothing. It is still worth
    /// reading: the two agreeing narrows a dispute to the numbers, and the two
    /// disagreeing names a meter that was swapped without the registry hearing
    /// about it.
    fn check_claimed_keys(
        &self,
        identity: &Identity,
        transaction: &Transaction,
        assembled: &emob_ocpp::Assembled,
    ) {
        let Some(record) = assembled.records.first() else {
            return;
        };
        let registered = self
            .registry
            .key_for_record(record, assembled.session.started_at)
            .ok()
            .map(|key| key.key.bytes.clone());

        for event in &transaction.events {
            for reading in &event.signed {
                let Some(Ok(claimed)) = reading.value.public_key() else {
                    continue;
                };
                if registered
                    .as_ref()
                    .is_some_and(|bytes| *bytes == claimed.bytes)
                {
                    continue;
                }
                lock(&self.outcomes).push(Outcome::KeyMismatch {
                    identity: identity.to_string(),
                    claimed: hex_of(&claimed.bytes),
                });
                return;
            }
        }
    }

    /// The billing half, for all three versions at once.
    ///
    /// One funnel: [`observe_v16`], [`observe_v201`] and [`observe_v21`] all
    /// produce the same [`Observed`], and `emob-ocpp` reads the billable event
    /// out of it. Nothing here is version-specific except the transaction key,
    /// which 1.6 makes the CSMS's problem.
    ///
    /// A [`WarningKind::UnreadableSignedData`] is refused before anything else
    /// looks at the event: a station that declares `format: SignedData` and
    /// sends a document that does not parse is otherwise **indistinguishable
    /// from one sending no signed data at all**, and the operator would find out
    /// when a month of sessions turns out to be unbillable. The message is still
    /// answered — refusing the RPC would only make the station retry.
    ///
    /// The other warning kinds are all about the numeric energy register, which
    /// is telemetry on this side of the seam and never billed here. They reach
    /// the operational ledger's question, not this one.
    fn bill(&self, ctx: &Ctx, transaction: &str, observed: &Observed) {
        let identity = ctx.identity();
        let unreadable: Vec<String> = observed
            .warnings
            .iter()
            .filter(|warning| warning.kind == WarningKind::UnreadableSignedData)
            .map(ToString::to_string)
            .collect();
        if !unreadable.is_empty() {
            lock(&self.outcomes).push(Outcome::Refused {
                identity: identity.to_string(),
                transaction: transaction.to_owned(),
                reasons: unreadable,
            });
        }

        if let Some(event) = emob_ocpp::kit::event_from(&observed.event) {
            self.accept(ctx, transaction, event);
        }
    }

    /// The operational half: arrival, sequence, retries. Kept because it is a
    /// different question, and reported because a link that retries constantly
    /// is one an operator wants to know about.
    fn record_operational(&self, identity: &Identity, observed: &Observed) -> Option<Ingested> {
        let event = ocpp_kit::csms::events::to_ledger_event(identity, observed)?;
        let mut ledger = lock(&self.operational);
        Some(if observed.version == Version::V1_6 {
            ledger.ingest_unsequenced(&event)
        } else {
            ledger.ingest(&event)
        })
    }

    /// The next transaction id for a 1.6 station.
    ///
    /// 1.6 assigns it in the response, so the CSMS owns it. 2.x has the station
    /// allocate one instead, which is why this is the only counter here.
    fn allocate_1_6_transaction(&self) -> i32 {
        let mut next = lock(&self.next_1_6_transaction);
        let id = *next;
        *next = next.saturating_add(1);
        id
    }
}

/// Lock a mutex, recovering from poisoning.
///
/// Every mutex here guards a plain collection — the in-flight transactions, the
/// two ledgers, the outcome list, the 1.6 counter. The poison flag says a thread
/// died holding the lock and nothing about whether a `Vec<Outcome>` still makes
/// sense, so bailing out on it would answer a panic elsewhere by dropping the
/// record of what was billed, which is the worse failure of the two.
/// Everything an accepted record was identified by, for [`outcome_of`].
struct Accepted<'a> {
    key: &'a str,
    energy: &'a str,
    retries: usize,
    identity: &'a str,
    transaction: &'a str,
}

/// What the ledger's answer means for the operator queue.
///
/// Three answers, three different facts, and collapsing them into "the ledger
/// did not store it" reports a **retry** — the case OCPP guarantees delivery
/// with, and the case [`CdrLedger`] is idempotent for — as a refused session.
/// That is the opposite of what happened: the record is settled, and has been
/// since the first offer. An operator queue that shows it as a failure sends
/// somebody to investigate a success, and any reconciliation built on the
/// queue counts the energy twice.
///
/// A pure function so the mapping is tested rather than exercised: a daemon is
/// the worst place to keep a rule, because CI does not run it.
fn outcome_of(acceptance: &Acceptance, record: Accepted<'_>) -> Outcome {
    let refused = |reason: String| Outcome::Refused {
        identity: record.identity.to_owned(),
        transaction: record.transaction.to_owned(),
        reasons: vec![reason],
    };
    match acceptance {
        Acceptance::Stored => Outcome::Settled {
            key: record.key.to_owned(),
            energy: record.energy.to_owned(),
            retries: record.retries,
        },
        Acceptance::Duplicate => Outcome::AlreadySettled {
            key: record.key.to_owned(),
            energy: record.energy.to_owned(),
        },
        Acceptance::Conflict { difference } => refused(format!(
            "a different record is already settled under {}: {difference}. A partner restating a \
             settled number needs a human, not an upsert",
            record.key
        )),
        // `Acceptance` is `#[non_exhaustive]`; an answer this build does not
        // know is not one it may read as success.
        other => refused(format!(
            "the ledger answered {other:?}, which this build cannot interpret"
        )),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Handler for Csmsd {
    fn on_request(
        &self,
        ctx: Ctx,
        request: IncomingRequest,
    ) -> BoxFuture<'_, Result<Box<RawValue>, CallError>> {
        Box::pin(async move {
            match ctx.version() {
                Version::V1_6 => self.on_v1_6(&ctx, &request),
                Version::V2_0_1 => self.on_v2_0_1(&ctx, &request),
                _ => self.on_v2_1(&ctx, &request),
            }
        })
    }
}

impl Csmsd {
    fn on_v1_6(&self, ctx: &Ctx, request: &IncomingRequest) -> Result<Box<RawValue>, CallError> {
        let action = v1_6::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v1_6::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        let observed = observe_v16(&typed);
        self.record_operational(ctx.identity(), &observed);

        // 1.6's transaction id lives in the response for `StartTransaction` and
        // in the request for everything after it, so the funnel cannot supply
        // it and this is the one place that knows it.
        let transaction_id = match &typed {
            v1_6::CsRequest::StartTransaction(_) => Some(self.allocate_1_6_transaction()),
            v1_6::CsRequest::MeterValues(values) => values.transaction_id,
            v1_6::CsRequest::StopTransaction(stop) => Some(stop.transaction_id),
            _ => None,
        };

        if let Some(id) = transaction_id {
            self.bill(ctx, &id.to_string(), &observed);
        }

        answer_v1_6(ctx, &typed, transaction_id)
    }

    fn on_v2_0_1(&self, ctx: &Ctx, request: &IncomingRequest) -> Result<Box<RawValue>, CallError> {
        let action = v2_0_1::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v2_0_1::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        let observed = observe_v201(&typed);
        self.record_operational(ctx.identity(), &observed);

        if let v2_0_1::CsRequest::TransactionEvent(event) = &typed {
            self.bill(ctx, &event.transaction_info.transaction_id, &observed);
        }
        answer_v2_0_1(ctx, &typed)
    }

    fn on_v2_1(&self, ctx: &Ctx, request: &IncomingRequest) -> Result<Box<RawValue>, CallError> {
        let action = v2_1::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v2_1::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        let observed = observe_v21(&typed);
        self.record_operational(ctx.identity(), &observed);

        if let v2_1::CsRequest::TransactionEvent(event) = &typed {
            self.bill(ctx, &event.transaction_info.transaction_id, &observed);
        }
        answer_v2_1(ctx, &typed)
    }
}

/// A key as hex, for an operator comparing it against a cabinet.
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The 1.6 replies a station needs to make progress.
fn answer_v1_6(
    ctx: &Ctx,
    request: &v1_6::CsRequest,
    transaction_id: Option<i32>,
) -> Result<Box<RawValue>, CallError> {
    let accepted = v1_6::IdTagInfo::new(v1_6::AuthorizationStatus::Accepted);
    match request {
        v1_6::CsRequest::BootNotification(_) => ctx.reply(&v1_6::BootNotificationResponse::new(
            v1_6::RegistrationStatus::Accepted,
            DateTime::UNIX_EPOCH,
            300,
        )),
        v1_6::CsRequest::Heartbeat(_) => {
            ctx.reply(&v1_6::HeartbeatResponse::new(DateTime::UNIX_EPOCH))
        }
        v1_6::CsRequest::StatusNotification(_) => {
            ctx.reply(&v1_6::StatusNotificationResponse::new())
        }
        v1_6::CsRequest::Authorize(_) => ctx.reply(&v1_6::AuthorizeResponse::new(accepted)),
        v1_6::CsRequest::StartTransaction(_) => ctx.reply(&v1_6::StartTransactionResponse::new(
            accepted,
            transaction_id.unwrap_or_default(),
        )),
        v1_6::CsRequest::MeterValues(_) => ctx.reply(&v1_6::MeterValuesResponse::new()),
        v1_6::CsRequest::StopTransaction(_) => ctx.reply(&v1_6::StopTransactionResponse::new()),
        other => Err(CallError::not_supported(other.action().as_str())),
    }
}

/// The 2.0.1 replies.
fn answer_v2_0_1(ctx: &Ctx, request: &v2_0_1::CsRequest) -> Result<Box<RawValue>, CallError> {
    match request {
        v2_0_1::CsRequest::BootNotification(_) => {
            ctx.reply(&v2_0_1::BootNotificationResponse::new(
                DateTime::UNIX_EPOCH,
                300,
                v2_0_1::RegistrationStatus::Accepted,
            ))
        }
        v2_0_1::CsRequest::Heartbeat(_) => {
            ctx.reply(&v2_0_1::HeartbeatResponse::new(DateTime::UNIX_EPOCH))
        }
        v2_0_1::CsRequest::StatusNotification(_) => {
            ctx.reply(&v2_0_1::StatusNotificationResponse::new())
        }
        v2_0_1::CsRequest::Authorize(_) => ctx.reply(&v2_0_1::AuthorizeResponse::new(
            v2_0_1::IdTokenInfo::new(v2_0_1::AuthorizationStatus::Accepted),
        )),
        v2_0_1::CsRequest::TransactionEvent(_) => {
            ctx.reply(&v2_0_1::TransactionEventResponse::new())
        }
        other => Err(CallError::not_supported(other.action().as_str())),
    }
}

/// The 2.1 replies.
fn answer_v2_1(ctx: &Ctx, request: &v2_1::CsRequest) -> Result<Box<RawValue>, CallError> {
    match request {
        v2_1::CsRequest::BootNotification(_) => ctx.reply(&v2_1::BootNotificationResponse::new(
            DateTime::UNIX_EPOCH,
            300,
            v2_1::RegistrationStatus::Accepted,
        )),
        v2_1::CsRequest::Heartbeat(_) => {
            ctx.reply(&v2_1::HeartbeatResponse::new(DateTime::UNIX_EPOCH))
        }
        v2_1::CsRequest::StatusNotification(_) => {
            ctx.reply(&v2_1::StatusNotificationResponse::new())
        }
        v2_1::CsRequest::Authorize(_) => ctx.reply(&v2_1::AuthorizeResponse::new(
            v2_1::IdTokenInfo::new(v2_1::AuthorizationStatus::Accepted),
        )),
        v2_1::CsRequest::TransactionEvent(_) => ctx.reply(&v2_1::TransactionEventResponse::new()),
        other => Err(CallError::not_supported(other.action().as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record<'a>() -> Accepted<'a> {
        Accepted {
            key: "DE*ABC/CP-1-42",
            energy: "0.636 kWh",
            retries: 2,
            identity: "CP-1",
            transaction: "42",
        }
    }

    #[test]
    fn a_retry_is_a_settled_record_rather_than_a_refused_session() {
        // OCPP guarantees delivery by retrying, so a record the ledger already
        // holds is the ordinary shape of a flaky link. Reporting it as a
        // refusal sends somebody to investigate a success — and doubles the
        // energy in any reconciliation built on this queue.
        let outcome = outcome_of(&Acceptance::Duplicate, record());
        assert!(
            matches!(
                outcome,
                Outcome::AlreadySettled { ref key, ref energy }
                    if key == "DE*ABC/CP-1-42" && energy == "0.636 kWh"
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_first_offer_settles_and_carries_what_the_link_cost() {
        let outcome = outcome_of(&Acceptance::Stored, record());
        assert!(
            matches!(outcome, Outcome::Settled { retries: 2, .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_restated_number_is_the_one_answer_that_needs_a_human() {
        let outcome = outcome_of(
            &Acceptance::Conflict {
                difference: "total_energy 0.636 kWh vs 1.200 kWh".to_owned(),
            },
            record(),
        );
        let Outcome::Refused { reasons, .. } = outcome else {
            panic!("a conflict is the refusal: {outcome:?}");
        };
        assert!(reasons[0].contains("needs a human"), "{reasons:?}");
        assert!(reasons[0].contains("0.636 kWh vs 1.200 kWh"), "{reasons:?}");
    }
}
