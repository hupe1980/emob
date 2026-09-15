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
        /// `seqNo`s the station never delivered, when the link lost some.
        ///
        /// Empty on a complete transaction. The record still settles and its
        /// energy is still the signed register's — a gap that swallowed a
        /// signed value would have broken the chain's own pagination and been
        /// refused — but the transaction's **shape** has a hole in it, and the
        /// occupancy `[AFIR Art. 5(4)]` prices per minute is read off that
        /// shape. An operator holding a disputed occupancy fee needs to know
        /// this record is one of these.
        gap: Vec<i32>,
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
    /// A billable event arrived for a transaction that had already ended.
    ///
    /// The operational ledger answers this — `[ocpp_kit::csms::Ingested]`'s
    /// `AfterEnd` — and the answer has to be **read**: dropped, a straggler
    /// re-opens a transaction that had already settled and the chain builds a
    /// second record from whatever that one message carried. On 1.6 that is a
    /// **refusal** naming a metrology fault that does not exist; on 2.x it is a
    /// restated total, which is the one outcome that needs a human (D306).
    ///
    /// Reported rather than dropped, because the signed value it carries is
    /// energy that is **not** on the record that settled: a station that keeps
    /// sending after its own `Ended` is one whose sessions are being billed
    /// short, and the fix is at the station rather than here.
    AfterTheEnd {
        /// Which station.
        identity: String,
        /// Which transaction.
        transaction: String,
    },
    /// A station sent a public key beside its signed value and the key does not
    /// decode.
    ///
    /// Not fatal to billing on its own: the key that decides anything comes
    /// from the registry, out of band `[OCMF §Relation of Serial Numbers]`, and
    /// the chain verifies against that. It is reported because it has its own
    /// fix — a station whose claimed key is unreadable is one whose provisioning
    /// nobody can **check** against the cabinet, so the comparison D186 exists
    /// for silently stops happening and a swapped meter stops being noticed.
    ///
    /// Skipping it is the shape this workspace keeps finding: the rule written
    /// down on the error variant, and the code going `continue` past it.
    UnreadableClaimedKey {
        /// Which station.
        identity: String,
        /// What made it unreadable.
        detail: String,
    },
}

/// A transaction a station has opened and not yet closed, and what the link
/// cost to carry it.
#[derive(Debug)]
struct Open {
    transaction: Transaction,
    /// Messages the operational ledger recognised as retries of ones it had
    /// already taken.
    ///
    /// A different count from `Assembled::duplicates_dropped`, which is the
    /// same signed *record* delivered by two different messages. This is the
    /// same *message* delivered twice, and it is counted here because the event
    /// that carries it never reaches the transaction any more — see
    /// [`Csmsd::accept`]. Both are what a flaky link cost, and
    /// [`Outcome::Settled`] reports their sum.
    retried_messages: usize,
    /// `seqNo`s the operational ledger says never arrived.
    ///
    /// A different fact from a retry, and the opposite one: a retry is a
    /// message delivered twice, this is a message delivered **none**. Almost
    /// always an offline period whose queue overflowed.
    ///
    /// What it does and does not threaten is the whole of why it is kept.
    /// The **energy** is safe: it comes from the signed OCMF register, whose
    /// own pagination refuses a chain with a record missing from it, so a gap
    /// that swallowed a signed value is already a refusal. What is not safe is
    /// the transaction's **shape** — a `SuspendedEV` nobody received is
    /// occupancy `[AFIR Art. 5(4)]` prices per minute and this side never knew
    /// about. So the gap is reported rather than billed on.
    missing_messages: Vec<i32>,
}

impl Open {
    /// A transaction nobody has sent an event into yet.
    fn new(id: &emob_core::SessionId, evse: &emob_core::EvseId) -> Self {
        Self {
            transaction: Transaction::new(id.clone(), evse.clone(), Authorization::ad_hoc()),
            retried_messages: 0,
            missing_messages: Vec::new(),
        }
    }
}

/// The CSMS.
pub struct Csmsd {
    party: PartyId,
    registry: KeyRegistry,
    tariff: Tariff,
    /// Transactions the stations have opened and not yet closed.
    inflight: Mutex<BTreeMap<(Identity, String), Open>>,
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
    ///
    /// # The operational ledger's answer is a rule, not telemetry
    ///
    /// `ingested` is what [`Ledger::ingest`] said about this message, and the
    /// only thing here that knows it has been seen before.
    /// `Ingested::Duplicate`'s own documentation states the rule — *"the station
    /// retried; do not bill it twice"*.
    ///
    /// Discarding it costs a session. A station retransmitting its
    /// `StopTransaction` — the ordinary shape of a link that lost one answer —
    /// finds no transaction under the key on the second delivery, because the
    /// first settled and removed it; a fresh one opened from that single event
    /// is refused for a window it does not have, and a settled session lands in
    /// the operator's failure queue naming a metrology fault that never happened
    /// (D306).
    ///
    /// So a duplicate reaches nothing: it opens no transaction, appends to none
    /// and closes none. It is counted, because a link that retries constantly is
    /// worth knowing about.
    ///
    /// # …and the verdict beside it, which the attribute cannot catch
    ///
    /// `Ingested` is `#[must_use]` since `ocpp-kit` 0.3, which closes the shape
    /// above for good. It does not close `AppliedWithGap`: a verdict **bound**
    /// and swept into a catch-all is a value nothing dropped, so no attribute
    /// fires, and its `missing` list — *"the `seqNo`s that have never
    /// arrived"* — goes with it (D327).
    ///
    /// The energy is safe either way; it was never this ledger's. What a gap
    /// costs is the transaction's **shape**, and the occupancy read off it.
    fn accept(
        &self,
        ctx: &Ctx,
        transaction_id: &str,
        event: TransactionEvent,
        ingested: Option<Ingested>,
    ) {
        let identity = ctx.identity();
        // The charge point this station was provisioned as, hung on the session
        // by the authenticator. A connection that reached a handler without one
        // is a deployment that skipped the binding, and it must not produce a
        // session attributed to a point nobody named.
        let Some(point) = ctx.session::<ChargePoint>() else {
            return;
        };
        let closing = event.kind == emob_ocpp::EventKind::Ended;

        // A transaction the station did not name. The record it would settle is
        // one nothing can be traced back to, and a substitute id would put
        // every such transaction on this station under one key — so it is
        // refused here, where the identity and the raw id are still in hand
        // (D249).
        let Ok(id) = transaction_id.parse::<emob_core::SessionId>() else {
            lock(&self.outcomes).push(Outcome::Refused {
                identity: identity.to_string(),
                transaction: transaction_id.to_owned(),
                reasons: vec![
                    "the station sent a transaction event with no transaction id: a record \
                     settled under a substitute id could not be traced back to the session \
                     it billed"
                        .to_owned(),
                ],
            });
            return;
        };

        let key = (identity.clone(), transaction_id.to_owned());

        match ingested {
            // Already taken. Counted where there is still a transaction to
            // count it on; where there is not, the record has settled and the
            // operational ledger is the one holding the retry.
            Some(Ingested::Duplicate) => {
                if let Some(open) = lock(&self.inflight).get_mut(&key) {
                    open.retried_messages += 1;
                }
                return;
            }
            // The station's own `Ended` has already been through here. A
            // straggler after it is not a second transaction, and the signed
            // value it carries is not on the record that settled.
            Some(Ingested::AfterEnd) => {
                lock(&self.outcomes).push(Outcome::AfterTheEnd {
                    identity: identity.to_string(),
                    transaction: transaction_id.to_owned(),
                });
                return;
            }
            // Recorded, and `seqNo`s before it never arrived. **Not** a
            // refusal — the event in hand is real and the energy under it is
            // signed — but the transaction's shape now has a hole, and
            // `[ocpp_kit::csms::Ingested]` says in as many words that a
            // consumer which discards this bills a transaction with one.
            Some(Ingested::AppliedWithGap { missing }) => {
                lock(&self.inflight)
                    .entry(key.clone())
                    .or_insert_with(|| Open::new(&id, &point.evse_id))
                    .missing_messages
                    .extend(missing);
            }
            // `Applied`, no ledger event at all — and a verdict a later
            // `ocpp-kit` adds, because `Ingested` is `#[non_exhaustive]`. All
            // of them are a fresh delivery here. The upstream documentation
            // names the precondition for falling this way — *"treating it as a
            // fresh delivery risks a double-settlement unless something
            // further on deduplicates"* — and this side has one: `CdrLedger`
            // is idempotent on the record's key. The other direction has no
            // such backstop, because a signed kilowatt-hour dropped on an
            // unrecognised verdict is unbilled energy nothing reports.
            _ => {}
        }

        let mut inflight = lock(&self.inflight);
        let open = inflight
            .entry(key.clone())
            .or_insert_with(|| Open::new(&id, &point.evse_id));
        open.transaction.events.push(event);

        if closing && let Some(finished) = inflight.remove(&key) {
            drop(inflight);
            self.settle(identity, transaction_id, point, &finished);
        }
    }

    /// The chain, exactly as `emob-sim` drives it: assemble, verify, price,
    /// accept.
    fn settle(&self, identity: &Identity, transaction_id: &str, point: &ChargePoint, open: &Open) {
        let transaction = &open.transaction;
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
        // The owned records outlive this borrowed view: a `Record` borrows the
        // bytes its signature covers.
        let borrowed: Vec<ocmf::Record<'_>> = assembled
            .records
            .iter()
            .filter_map(|r| r.record().ok())
            .collect();
        let evidence = Evidence::assemble(&borrowed, &self.registry, assembled.session.started_at);

        // The record's identity is the station's and the transaction's, and it
        // has to stay **unique**: `CdrLedger` is keyed by it, so two records
        // sharing one id are one record, and the second is a restatement that
        // leaves the first standing. A fallback id would make every record it
        // caught collide with every other, which is the one failure a ledger
        // cannot report (D249).
        let Ok(id) = format!("{identity}-{transaction_id}").parse() else {
            return refuse(vec![
                "neither the station identity nor the transaction id names anything, so this \
                 record has no id that would be unique in the ledger"
                    .to_owned(),
            ]);
        };
        let cdr =
            CdrBuilder::from_session(&assembled.session, Direction::Import).and_then(|builder| {
                builder
                    .key(self.party.clone(), id)
                    .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
                    .rated_with(&self.tariff)
                    .build()
            });

        match cdr {
            Ok(cdr) => {
                let energy = cdr.total_energy.to_string();
                let key = cdr.key.to_string();
                // Both halves of what the link cost: records the transport
                // delivered twice, and messages it delivered twice.
                let retries = assembled.duplicates_dropped + open.retried_messages;
                let gap = open.missing_messages.clone();
                let stored = lock(&self.cdrs).accept(cdr);
                {
                    let mut outcomes = lock(&self.outcomes);
                    outcomes.push(outcome_of(
                        &stored,
                        Accepted {
                            key: &key,
                            energy: &energy,
                            retries,
                            gap: &gap,
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
        let Some(Ok(record)) = assembled.records.first().map(ocmf::RecordBuf::record) else {
            return;
        };
        let registered = self
            .registry
            .key_for_record(&record, assembled.session.started_at)
            .ok()
            .map(|key| key.key.clone());

        for event in &transaction.events {
            for reading in &event.signed {
                let claimed = match reading.value.public_key() {
                    Some(Ok(claimed)) => claimed,
                    // The station sent no key at all, which is what most of the
                    // fleet does and says nothing.
                    None => continue,
                    Some(Err(error)) => {
                        lock(&self.outcomes).push(Outcome::UnreadableClaimedKey {
                            identity: identity.to_string(),
                            detail: error.to_string(),
                        });
                        return;
                    }
                };
                // Compared as **keys**, not as bytes. A station may send the
                // same point as a bare SEC1 encoding, wrapped in a
                // `SubjectPublicKeyInfo`, or inside the OCA's
                // `oca:base16:asn1:` envelope — three spellings of one key, and
                // a byte comparison calls two of them a mismatch and sends an
                // operator after a meter nobody swapped.
                let claimed = match ocmf::PublicKey::from_bytes(
                    &claimed.bytes,
                    registered.as_ref().map(ocmf::PublicKey::curve),
                ) {
                    Ok(claimed) => claimed,
                    Err(error) => {
                        lock(&self.outcomes).push(Outcome::UnreadableClaimedKey {
                            identity: identity.to_string(),
                            detail: error.to_string(),
                        });
                        return;
                    }
                };
                if registered.as_ref() == Some(&claimed) {
                    continue;
                }
                lock(&self.outcomes).push(Outcome::KeyMismatch {
                    identity: identity.to_string(),
                    claimed: hex_of(claimed.sec1_bytes()),
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
    fn bill(&self, ctx: &Ctx, transaction: &str, observed: &Observed, ingested: Option<Ingested>) {
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
            self.accept(ctx, transaction, event, ingested);
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

/// Everything an accepted record was identified by, for [`outcome_of`].
struct Accepted<'a> {
    key: &'a str,
    energy: &'a str,
    retries: usize,
    gap: &'a [i32],
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
            gap: record.gap.to_vec(),
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

/// Lock a mutex, recovering from poisoning.
///
/// Every mutex here guards a plain collection — the in-flight transactions, the
/// two ledgers, the outcome list, the 1.6 counter. The poison flag says a thread
/// died holding the lock and nothing about whether a `Vec<Outcome>` still makes
/// sense, so bailing out on it would answer a panic elsewhere by dropping the
/// record of what was billed, which is the worse failure of the two.
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
        let ingested = self.record_operational(ctx.identity(), &observed);

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
            self.bill(ctx, &id.to_string(), &observed, ingested);
        }

        answer_v1_6(ctx, &typed, transaction_id)
    }

    fn on_v2_0_1(&self, ctx: &Ctx, request: &IncomingRequest) -> Result<Box<RawValue>, CallError> {
        let action = v2_0_1::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v2_0_1::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        let observed = observe_v201(&typed);
        let ingested = self.record_operational(ctx.identity(), &observed);

        if let v2_0_1::CsRequest::TransactionEvent(event) = &typed {
            self.bill(
                ctx,
                &event.transaction_info.transaction_id,
                &observed,
                ingested,
            );
        }
        answer_v2_0_1(ctx, &typed)
    }

    fn on_v2_1(&self, ctx: &Ctx, request: &IncomingRequest) -> Result<Box<RawValue>, CallError> {
        let action = v2_1::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v2_1::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        let observed = observe_v21(&typed);
        let ingested = self.record_operational(ctx.identity(), &observed);

        if let v2_1::CsRequest::TransactionEvent(event) = &typed {
            self.bill(
                ctx,
                &event.transaction_info.transaction_id,
                &observed,
                ingested,
            );
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
            gap: &[],
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

    /// The verdict `[ocpp_kit::csms::Ingested]` says a consumer must not
    /// discard, and the reason it is not a refusal.
    ///
    /// *"A consumer that discards it bills a transaction with a hole in it."*
    /// The hole is not in the **energy** — that is the signed register's, and a
    /// gap that swallowed a signed value breaks the chain's own pagination and
    /// is refused long before here. It is in the transaction's **shape**: a
    /// `SuspendedEV` nobody received is occupancy `[AFIR Art. 5(4)]` prices per
    /// minute and this side never knew about. So the record settles and the
    /// gap travels with it.
    #[test]
    fn a_transaction_with_a_hole_in_it_still_settles_and_says_where_the_hole_is() {
        let outcome = outcome_of(
            &Acceptance::Stored,
            Accepted {
                gap: &[7, 8],
                ..record()
            },
        );
        let Outcome::Settled { gap, energy, .. } = &outcome else {
            panic!("a gap is not a refusal: the event in hand is real: {outcome:?}");
        };
        assert_eq!(gap, &[7, 8], "the seqNos that never arrived travel with it");
        assert_eq!(
            energy, "0.636 kWh",
            "and the energy is unchanged, because it was never the ledger's to begin with"
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
