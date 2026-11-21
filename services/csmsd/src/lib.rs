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

use emob_service::cloudevent::CloudEvent;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use emob_cdr::{Acceptance, Cdr, CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::{Direction, PartyId};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_ocpp::{Transaction, TransactionEvent};
use emob_service::Actor;
use emob_service::journal::{Folded, Journal, JournalError};
use ocpp_kit::csms::events::{Observed, WarningKind, observe_v16, observe_v21, observe_v201};
use ocpp_kit::csms::ledger::{Ingested, Ledger};
use ocpp_kit::engine::IncomingRequest;
use ocpp_kit::rpc::CallError;
use ocpp_kit::transport::{BoxFuture, Ctx, Handler};
use ocpp_kit::types::{DateTime, Identity};
use ocpp_kit::{RawValue, Version, v1_6, v2_0_1, v2_1};
use tracing::Instrument as _;

pub mod authorize;
pub mod carrier;
pub mod daemon;
pub mod display;
pub mod edge;
pub mod estate;
pub mod journal;
pub mod outbox;
pub mod price;
pub mod provisioning;
pub mod read;
pub mod receipt;
pub mod registry;
pub mod station;

pub use authorize::{Answer, Authorize, Status};
pub use journal::{Delivery, DeliveryState, Event, Key, Open, Presented, State, Target};
pub use provisioning::{ChargePoint, Provisioning};

/// What became of one transaction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
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
        /// The evidence chain's findings, typed and cited — empty when the
        /// refusal came from somewhere other than the evidence (a tariff the
        /// point may not offer, a transaction with no id, a ledger conflict).
        /// What an operator page words as *"warum?"*.
        findings: Vec<emob_eichrecht::Finding>,
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

/// The `data` of a `de.emob.cdr.issued` event, as `billd`, `roamd` and every
/// subscriber read it (`specs/archive/001-the-estate-runs/contracts/delivery.md`).
#[derive(Debug, serde::Serialize)]
pub struct CdrDelivery<'a> {
    /// The record, whole — the same value both receivers see.
    pub cdr: &'a Cdr,
    /// Whom it bills.
    pub token: &'a Presented,
    /// The signed OCMF records it rests on, as the station sent them.
    pub signed: Vec<String>,
    /// The station that ran it.
    pub station: &'a str,
    /// The point and its place, from the registry (spec 021).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point: Option<CdrPoint>,
}

/// What a CDR's location is built from (spec 021).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CdrPoint {
    /// The EVSE id.
    pub evse: emob_core::EvseId,
    /// The connector.
    pub connector: emob_core::ConnectorType,
    /// Rated power, kW.
    pub rated_power_kw: rust_decimal::Decimal,
    /// The place.
    pub location: emob_service::estate::Location,
}

/// The heartbeat interval every `BootNotificationResponse` hands a station, in
/// seconds — and the silence after which the operator page calls it silent.
pub const HEARTBEAT_SECS: i32 = 300;

/// The CSMS.
pub struct Csmsd<J: Journal> {
    party: PartyId,
    registry: KeyRegistry,
    authority: Arc<dyn Authorize>,
    /// The operational view: arrival, sequence, retries. Meter values as `f64`,
    /// which is right for the question it answers and never leaves this field.
    operational: Mutex<Ledger>,
    /// Everything else, folded from the journal.
    folded: Mutex<Folded<State, Event, J>>,
    /// When each station was last heard from — about the link. Only the
    /// *transitions* are journalled (`Online`, `Offline`, spec 012); a
    /// heartbeat a minute is not a fact.
    presence: Mutex<std::collections::BTreeMap<String, time::OffsetDateTime>>,
    /// When this process started; nothing heard before it is known.
    started: time::OffsetDateTime,
    /// The key receipts are minted under (spec 003). `None` is a CSMS that
    /// issues none — a test that does not look at them; the daemon always
    /// sets one.
    receipts: Option<receipt::Receipts>,
    /// The stations that show their receipt link, and the CSMS that reaches
    /// them (spec 011). Set once the CSMS is built, after this value is.
    display: std::sync::OnceLock<(display::Display, ocpp_kit::transport::CsmsHandle)>,
    /// Where `at` on the facts that end an attempt comes from (spec 007).
    clock: emob_service::Clock,
    /// The `[[subscribers]]` that take `de.emob.cdr.issued`, by name.
    subscribers: Vec<String>,
    /// The estate's tariffs, which a registered station names (spec 021).
    tariffs: std::collections::BTreeMap<String, emob_tariff::TariffHistory>,
}

impl<J: Journal> core::fmt::Debug for Csmsd<J> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Csmsd")
            .field("party", &self.party)
            .finish_non_exhaustive()
    }
}

impl<J: Journal> Csmsd<J> {
    /// A CSMS whose state is what `journal` says it is.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when the journal does not chain or holds a fact this
    /// build cannot read — a CSMS that started anyway would be billing from a
    /// ledger nobody can account for.
    /// Each attempt is counted on its day in `zone` — the estate's `[desk]
    /// time_zone`.
    pub fn open(
        party: PartyId,
        registry: KeyRegistry,
        authority: Arc<dyn Authorize>,
        journal: J,
        zone: emob_core::TimeZone,
    ) -> Result<Self, JournalError> {
        Ok(Self {
            party,
            registry,
            authority,
            operational: Mutex::new(Ledger::default()),
            folded: Mutex::new(Folded::open(State::new(zone), journal, journal::apply)?),
            presence: Mutex::new(std::collections::BTreeMap::new()),
            started: time::OffsetDateTime::now_utc(),
            receipts: None,
            display: std::sync::OnceLock::new(),
            clock: emob_service::system_clock(),
            subscribers: Vec::new(),
            tariffs: std::collections::BTreeMap::new(),
        })
    }

    /// Owe every record issued from now on to these subscribers as well.
    #[must_use]
    pub fn delivering_to(mut self, subscribers: Vec<String>) -> Self {
        self.subscribers = subscribers;
        self
    }

    /// Date the facts it decides by `clock` rather than the wall clock — a
    /// simulated estate's, so its "today" is the page's.
    #[must_use]
    pub fn with_clock(mut self, clock: emob_service::Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Now, by this CSMS's clock.
    #[must_use]
    pub fn now(&self) -> time::OffsetDateTime {
        (self.clock)()
    }

    /// Issue a receipt for every transaction that settles or is refused at
    /// settlement, under `receipts` (spec 003).
    #[must_use]
    pub fn issuing_receipts(mut self, receipts: receipt::Receipts) -> Self {
        self.receipts = Some(receipts);
        self
    }

    /// Show each receipt link on the stations `display` names, through
    /// `handle` (spec 011). A second call is ignored.
    pub fn showing_receipts(
        &self,
        display: display::Display,
        handle: ocpp_kit::transport::CsmsHandle,
    ) {
        let _ = self.display.set((display, handle));
    }

    /// The price on a booted station's own display, when the estate asks for
    /// it (spec 015): every tariff version still to come, over its window, on
    /// its own task after the boot is answered — a courtesy like the receipt
    /// link, never journalled, logged with the station and the outcome only.
    fn show_price(&self, identity: &Identity) {
        let (Some((_, handle)), Some(public)) = (self.display.get(), self.public_url()) else {
            return;
        };
        let Some(station) = self.registered(identity.as_str()) else {
            return;
        };
        if !station.show_price {
            return;
        }
        let Some(history) = self.tariff(&station.tariff).cloned() else {
            return;
        };
        let url = format!("{public}{}", price::path(&station.evse));
        let (handle, target, now) = (handle.clone(), identity.clone(), self.now());
        let ocpp = station.ocpp;
        tokio::spawn(async move {
            // After the `BootNotificationResponse`: a station takes no call
            // before it is accepted.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let mut statuses = Vec::new();
            match ocpp {
                emob_service::estate::Ocpp::V1_6 => return,
                emob_service::estate::Ocpp::V2_0_1 => {
                    for request in price::messages_v201(target.as_str(), &history, &url, now) {
                        statuses.push(display_status(
                            tokio::time::timeout(display::TIMEOUT, handle.call(&target, request))
                                .await
                                .map(|answer| answer.map(|answer| display::wire(&answer.status))),
                        ));
                    }
                }
                emob_service::estate::Ocpp::V2_1 => {
                    for request in price::messages_v21(target.as_str(), &history, &url, now) {
                        statuses.push(display_status(
                            tokio::time::timeout(display::TIMEOUT, handle.call(&target, request))
                                .await
                                .map(|answer| answer.map(|answer| display::wire(&answer.status))),
                        ));
                    }
                }
            }
            tracing::info!(station = %target, statuses = ?statuses, "price on the display");
        });
    }

    /// The receipt link for a transaction — `/r/{token}` — when this CSMS
    /// issues receipts. Minted again from the key, never read from the journal.
    #[must_use]
    pub fn receipt_path(&self, key: &Key) -> Option<String> {
        self.receipts.as_ref().map(|receipts| receipts.path(key))
    }

    /// The absolute receipt link `billd` puts on the invoice line, for the
    /// settled record `cdr`.
    #[must_use]
    pub fn receipt_url_for_record(&self, cdr: &str) -> Option<String> {
        let receipts = self.receipts.as_ref()?;
        // By key, never by a search over every receipt issued (FR-003).
        let key = self.with_state(|state| state.receipt_for.get(cdr).cloned())?;
        Some(receipts.url(&key))
    }

    /// The `Receipted` fact for a transaction, when this CSMS issues receipts.
    fn receipted(
        &self,
        key: &Key,
        point: &ChargePoint,
        at: Option<time::OffsetDateTime>,
        signed: Vec<String>,
    ) -> Option<Event> {
        self.receipts
            .as_ref()
            .map(|receipts| receipts.issue(key, point.evse_id.clone(), at, signed))
    }

    /// The receipt link of `key` on its own station's display, when it
    /// shows one (SR-128, SR-129).
    fn show_receipt(&self, key: &Key) {
        let (Some((display, handle)), Some(receipts)) = (self.display.get(), &self.receipts) else {
            return;
        };
        if display.shows(&key.0) {
            display.send(handle, &key.0, &key.1, receipts.url(key));
        } else if let Some(station) = self.registered(&key.0) {
            display::Display::send_registered(handle, &station, &key.1, receipts.url(key));
        }
    }

    /// A station was heard from now — and, when the fold holds it offline or
    /// has never seen it, that is a fact (spec 012, SR-138).
    fn heard(&self, by: &Actor, identity: &Identity) -> Result<(), CallError> {
        let station = identity.to_string();
        let now = self.now();
        lock(&self.presence).insert(station.clone(), now);
        let online = lock(&self.folded)
            .state()
            .stations
            .get(&station)
            .is_some_and(|known| matches!(known.presence, Some(journal::Presence::Online(_))));
        if online {
            return Ok(());
        }
        self.record(by, Event::Online { station, at: now })
    }

    /// Every station heard from and silent past the heartbeat interval becomes
    /// `Offline` — once, by the transition (spec 012, SR-138). A station the
    /// fold holds online that this process has not heard from is measured from
    /// the later of its last fact and this process's start.
    ///
    /// # Errors
    ///
    /// When the journal refuses the fact.
    pub fn sweep(&self, now: time::OffsetDateTime) -> Result<usize, CallError> {
        let threshold = time::Duration::seconds(i64::from(HEARTBEAT_SECS));
        let heard = lock(&self.presence).clone();
        let silent: Vec<(String, time::OffsetDateTime)> = lock(&self.folded)
            .state()
            .stations
            .iter()
            .filter_map(|(station, known)| match known.presence {
                Some(journal::Presence::Online(since)) => {
                    let last = heard
                        .get(station)
                        .copied()
                        .unwrap_or_else(|| since.max(self.started));
                    (now - last > threshold).then(|| (station.clone(), last))
                }
                _ => None,
            })
            .collect();
        for (station, last_heard) in &silent {
            self.record(
                &Actor::Timer,
                Event::Offline {
                    station: station.clone(),
                    last_heard: *last_heard,
                    at: now,
                },
            )?;
        }
        Ok(silent.len())
    }

    /// Journal a boot (spec 012).
    fn booted(
        &self,
        by: &Actor,
        identity: &Identity,
        reason: String,
        said: station::StationIdentity,
    ) -> Result<(), CallError> {
        self.record(
            by,
            Event::Booted {
                station: identity.to_string(),
                at: self.now(),
                reason,
                identity: said,
            },
        )
    }

    /// Journal a station's own report (spec 012, SR-140).
    fn reported(
        &self,
        by: &Actor,
        identity: &Identity,
        report: station::Report,
    ) -> Result<(), CallError> {
        self.record(
            by,
            Event::Reported {
                station: identity.to_string(),
                at: self.now(),
                report,
            },
        )
    }

    /// The tariffs a registered station may name (spec 021).
    #[must_use]
    pub fn pricing(
        mut self,
        tariffs: std::collections::BTreeMap<String, emob_tariff::TariffHistory>,
    ) -> Self {
        self.tariffs = tariffs;
        self
    }

    /// The registered station under `identity`.
    #[must_use]
    pub fn registered(&self, identity: &str) -> Option<emob_service::estate::Station> {
        lock(&self.folded).state().registry.get(identity).cloned()
    }

    /// How many stations are registered.
    #[must_use]
    pub fn registered_count(&self) -> usize {
        lock(&self.folded).state().registry.len()
    }

    /// Read the registry under one lock — what a search over ten thousand
    /// stations does, rather than a lock per row (spec 021).
    pub fn with_registry<T>(
        &self,
        f: impl FnOnce(
            &std::collections::BTreeMap<String, emob_service::estate::Station>,
            &journal::State,
        ) -> T,
    ) -> T {
        let folded = lock(&self.folded);
        let state = folded.state();
        f(&state.registry, state)
    }

    /// Register every estate station the registry does not hold, as the
    /// estate (spec 021). The estate seeds; afterwards the registry rules.
    ///
    /// # Errors
    ///
    /// When the journal refuses, or a seed fails the registry's own checks.
    pub fn seed(&self, stations: &[emob_service::estate::Station]) -> Result<usize, String> {
        let mut seeded = 0;
        for station in stations {
            if self.registered(&station.identity).is_some() {
                continue;
            }
            {
                let folded = lock(&self.folded);
                registry::check(station, &folded.state().registry, &self.tariffs, None)
                    .map_err(|e| e.to_string())?;
            }
            self.record(
                &Actor::Estate,
                Event::StationRegistered {
                    station: Box::new(station.clone()),
                    at: self.now(),
                },
            )
            .map_err(|e| format!("{e:?}"))?;
            seeded += 1;
        }
        Ok(seeded)
    }

    /// Register a new station for `by`, minting its password when its profile
    /// uses one. The password is returned here and nowhere else (SR-144).
    ///
    /// # Errors
    ///
    /// The [`registry::Refusal`]; nothing is journalled.
    pub fn register(
        &self,
        by: &Actor,
        mut station: emob_service::estate::Station,
    ) -> Result<Option<String>, registry::Refusal> {
        let password = if matches!(
            station.security_profile,
            emob_service::estate::SecurityProfile::TlsClientCertificate
        ) {
            station.password_hash = None;
            None
        } else {
            let (password, hash) = registry::mint(&station)?;
            station.password_hash = Some(hash);
            Some(password)
        };
        {
            let folded = lock(&self.folded);
            registry::check(&station, &folded.state().registry, &self.tariffs, None)?;
        }
        self.record(
            by,
            Event::StationRegistered {
                station: Box::new(station),
                at: self.now(),
            },
        )
        .map_err(|_| registry::Refusal {
            field: "journal".to_owned(),
            reason: "the journal refused the fact; nothing was registered".to_owned(),
        })?;
        Ok(password)
    }

    /// Replace a registered station's record, keeping its credential unless
    /// `remint` asks for a new password (returned once).
    ///
    /// # Errors
    ///
    /// The [`registry::Refusal`]; nothing is journalled.
    pub fn update(
        &self,
        by: &Actor,
        mut station: emob_service::estate::Station,
        reason: &str,
        remint: bool,
    ) -> Result<Option<String>, registry::Refusal> {
        let Some(held) = self.registered(&station.identity) else {
            return Err(registry::Refusal {
                field: "identity".to_owned(),
                reason: format!("{} is not registered", station.identity),
            });
        };
        if reason.trim().chars().count() < 10 {
            return Err(registry::Refusal {
                field: "reason".to_owned(),
                reason: "say why, in ten characters or more — it is what Verlauf shows".to_owned(),
            });
        }
        let password = if remint
            && !matches!(
                station.security_profile,
                emob_service::estate::SecurityProfile::TlsClientCertificate
            ) {
            let (password, hash) = registry::mint(&station)?;
            station.password_hash = Some(hash);
            Some(password)
        } else {
            station.password_hash = if matches!(
                station.security_profile,
                emob_service::estate::SecurityProfile::TlsClientCertificate
            ) {
                None
            } else {
                held.password_hash
            };
            None
        };
        {
            let folded = lock(&self.folded);
            registry::check(
                &station,
                &folded.state().registry,
                &self.tariffs,
                Some(held.identity.as_str()),
            )?;
        }
        self.record(
            by,
            Event::StationUpdated {
                station: Box::new(station),
                reason: reason.trim().to_owned(),
                at: self.now(),
            },
        )
        .map_err(|_| registry::Refusal {
            field: "journal".to_owned(),
            reason: "the journal refused the fact; nothing changed".to_owned(),
        })?;
        Ok(password)
    }

    /// Retire a station: its next connection is refused (SR-146).
    ///
    /// # Errors
    ///
    /// The [`registry::Refusal`]; nothing is journalled.
    pub fn retire(
        &self,
        by: &Actor,
        identity: &str,
        reason: &str,
    ) -> Result<(), registry::Refusal> {
        if self.registered(identity).is_none() {
            return Err(registry::Refusal {
                field: "identity".to_owned(),
                reason: format!("{identity} is not registered"),
            });
        }
        if reason.trim().chars().count() < 10 {
            return Err(registry::Refusal {
                field: "reason".to_owned(),
                reason: "say why, in ten characters or more — it is what Verlauf shows".to_owned(),
            });
        }
        self.record(
            by,
            Event::StationRetired {
                identity: identity.to_owned(),
                reason: reason.trim().to_owned(),
                at: self.now(),
            },
        )
        .map_err(|_| registry::Refusal {
            field: "journal".to_owned(),
            reason: "the journal refused the fact; the station is still registered".to_owned(),
        })
    }

    /// Write a note on a registered station under `by` (spec 023); its
    /// number on that station.
    ///
    /// # Errors
    ///
    /// A [`registry::Refusal`] naming `identity` for a station not registered,
    /// `text` for an empty or overlong note (SR-154), `journal` when the
    /// journal refuses.
    pub fn note(&self, by: &Actor, identity: &str, text: &str) -> Result<u64, registry::Refusal> {
        if self.registered(identity).is_none() {
            return Err(registry::Refusal {
                field: "identity".to_owned(),
                reason: format!("{identity} is not registered"),
            });
        }
        let text = station::note_text(text).ok_or_else(|| registry::Refusal {
            field: "text".to_owned(),
            reason: format!(
                "a note is 1 to {} characters — what the next person needs to know",
                station::NOTE_LEN
            ),
        })?;
        let at = self.now();
        // Numbered and appended under one lock, so two notes written at once
        // never share a number — removing one must not remove the other.
        let mut folded = lock(&self.folded);
        let number = folded
            .state()
            .stations
            .get(identity)
            .map_or(0, |known| known.notes_written)
            + 1;
        folded
            .record(
                by,
                Event::StationNoted {
                    identity: identity.to_owned(),
                    number,
                    by: by.to_string(),
                    text,
                    at,
                },
            )
            .map_err(|_| registry::Refusal {
                field: "journal".to_owned(),
                reason: "the journal refused the note".to_owned(),
            })?;
        Ok(number)
    }

    /// Take a note off a station; `false` when it has no such note.
    ///
    /// # Errors
    ///
    /// A [`registry::Refusal`] naming `journal` when the journal refuses.
    pub fn remove_note(
        &self,
        by: &Actor,
        identity: &str,
        number: u64,
    ) -> Result<bool, registry::Refusal> {
        let held = self
            .known(identity)
            .is_some_and(|known| known.notes.iter().any(|note| note.number == number));
        if !held {
            return Ok(false);
        }
        self.record(
            by,
            Event::NoteRemoved {
                identity: identity.to_owned(),
                number,
                at: self.now(),
            },
        )
        .map_err(|_| registry::Refusal {
            field: "journal".to_owned(),
            reason: "the journal refused the removal".to_owned(),
        })?;
        Ok(true)
    }

    /// Every version of a tariff, by its id (spec 015).
    #[must_use]
    pub fn tariff(&self, id: &str) -> Option<&emob_tariff::TariffHistory> {
        self.tariffs.get(id)
    }

    /// Where the public pages — receipts and prices — are served, when they
    /// are.
    #[must_use]
    pub fn public_url(&self) -> Option<&str> {
        self.receipts.as_ref().map(receipt::Receipts::public_url)
    }

    /// What the facts say about one station (spec 012).
    #[must_use]
    pub fn known(&self, identity: &str) -> Option<journal::Known> {
        lock(&self.folded).state().stations.get(identity).cloned()
    }

    /// When `identity` was last heard from, since this process started.
    #[must_use]
    pub fn last_heard(&self, identity: &str) -> Option<time::OffsetDateTime> {
        lock(&self.presence).get(identity).copied()
    }

    /// When this process started.
    #[must_use]
    pub const fn started(&self) -> time::OffsetDateTime {
        self.started
    }

    /// The key registry verification runs against.
    pub const fn registry(&self) -> &KeyRegistry {
        &self.registry
    }

    /// The records that settled.
    #[must_use]
    pub fn settled(&self) -> usize {
        lock(&self.folded).state().cdrs.len()
    }

    /// Everything that happened, in order.
    #[must_use]
    pub fn outcomes(&self) -> Vec<Outcome> {
        lock(&self.folded).state().outcomes.clone()
    }

    /// Read the folded state.
    pub fn with_state<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(lock(&self.folded).state())
    }

    /// The CDR ledger, for a caller that wants the records themselves.
    pub fn with_cdrs<T>(&self, f: impl FnOnce(&CdrLedger) -> T) -> T {
        f(&lock(&self.folded).state().cdrs)
    }

    /// The journal, for a caller that reads what was decided.
    pub fn with_journal<T>(&self, f: impl FnOnce(&J) -> T) -> T {
        f(lock(&self.folded).journal())
    }

    /// Append a fact and fold it.
    ///
    /// A journal that refuses is a decision not made: the station is answered
    /// with an error, so it retries, rather than with an acceptance nothing
    /// recorded. `by` is who caused it: the station whose authenticated
    /// socket sent the message, the receiver whose answer ended a delivery, or
    /// this daemon's own loop — never what a message says about itself.
    pub(crate) fn record(&self, by: &Actor, event: Event) -> Result<(), CallError> {
        lock(&self.folded)
            .record(by, event)
            .map(|_| ())
            .map_err(|error| {
                tracing::error!(%error, "the journal refused a fact; the station will retry");
                CallError::internal(format!("this CSMS could not record the message: {error}"))
            })
    }

    /// Ask the provider about a token, and journal what it said.
    async fn authorize(
        &self,
        by: &Actor,
        identity: &Identity,
        transaction: Option<String>,
        uid: &str,
    ) -> Result<Answer, CallError> {
        let answer = self.authority.ask(uid).await;
        if answer.status != Status::Accepted {
            tracing::info!(station = %identity, status = ?answer.status, reason = %answer.reason, "a token was turned away");
        }
        self.record(
            by,
            Event::Authorized {
                at: self.now(),
                station: identity.to_string(),
                transaction,
                answer: answer.clone(),
            },
        )?;
        Ok(answer)
    }

    /// Route one billable event into its transaction, settling it when the
    /// station says the transaction is over.
    ///
    /// # The operational ledger's answer is a rule, not telemetry
    ///
    /// `ingested` is what [`Ledger::ingest`] said about this message, and the
    /// only thing here that knows it has been seen before.
    /// `Ingested::Duplicate`'s own documentation states the rule — *"the station
    /// retried; do not bill it twice"* — so a duplicate opens no transaction,
    /// appends to none and closes none (D306). `AppliedWithGap` is bound and
    /// its `missing` list travels with the transaction, because a verdict swept
    /// into a catch-all is the one no attribute catches (D327).
    ///
    /// # The fact before the answer
    ///
    /// Every accepted event is journalled before the station hears back, so a
    /// transaction open across a restart settles after it.
    fn accept(
        &self,
        by: &Actor,
        ctx: &Ctx,
        transaction_id: &str,
        event: TransactionEvent,
        ingested: Option<Ingested>,
    ) -> Result<(), CallError> {
        let identity = ctx.identity();
        // The charge point this station was provisioned as, hung on the session
        // by the authenticator. A connection that reached a handler without one
        // is a deployment that skipped the binding, and it must not produce a
        // session attributed to a point nobody named.
        let Some(point) = ctx.session::<ChargePoint>() else {
            return Ok(());
        };
        let closing = event.kind == emob_ocpp::EventKind::Ended;

        // A transaction the station did not name. The record it would settle is
        // one nothing can be traced back to (D249).
        if transaction_id.parse::<emob_core::SessionId>().is_err() {
            return self.record(
                by,
                Event::Noted {
                    at: self.now(),
                    closes: None,
                    outcome: Outcome::Refused {
                        identity: identity.to_string(),
                        transaction: transaction_id.to_owned(),
                        reasons: vec![
                        "the station sent a transaction event with no transaction id: a record \
                         settled under a substitute id could not be traced back to the session \
                         it billed"
                            .to_owned(),
                    ],
                        findings: Vec::new(),
                    },
                },
            );
        }

        let key: Key = (identity.to_string(), transaction_id.to_owned());
        // A start the provider refused opened no transaction, and nothing the
        // station sends under its id afterwards is billed.
        if lock(&self.folded).state().refused.contains(&key) {
            return Ok(());
        }

        let missing = match ingested {
            Some(Ingested::Duplicate) => {
                if lock(&self.folded).state().inflight.contains_key(&key) {
                    return self.record(
                        by,
                        Event::Retried {
                            station: key.0,
                            transaction: key.1,
                        },
                    );
                }
                return Ok(());
            }
            Some(Ingested::AfterEnd) => {
                return self.record(
                    by,
                    Event::Noted {
                        at: self.now(),
                        closes: None,
                        outcome: Outcome::AfterTheEnd {
                            identity: identity.to_string(),
                            transaction: transaction_id.to_owned(),
                        },
                    },
                );
            }
            Some(Ingested::AppliedWithGap { missing }) => missing,
            // `Applied`, no ledger event, or a verdict a later `ocpp-kit` adds
            // (`#[non_exhaustive]`): a fresh delivery, with `CdrLedger`'s
            // idempotence as the backstop against a double settlement.
            _ => Vec::new(),
        };

        self.record(
            by,
            Event::Observed {
                station: key.0.clone(),
                transaction: key.1.clone(),
                evse: point.evse_id.clone(),
                event,
                missing,
            },
        )?;

        if closing {
            let open = lock(&self.folded).state().inflight.get(&key).cloned();
            if let Some(open) = open {
                let settled = self.settle(identity, transaction_id, point, &open);
                let receipted = settled
                    .iter()
                    .any(|event| matches!(event, Event::Receipted { .. }));
                for event in settled {
                    self.record(by, event)?;
                }
                // After the receipt and the verdict are journalled, and never
                // waited for (SR-131).
                if receipted {
                    self.show_receipt(&key);
                }
            }
        }
        Ok(())
    }

    /// The chain, exactly as `emob-sim` drives it: assemble, verify, price,
    /// accept — returned as the facts to record.
    fn settle(
        &self,
        identity: &Identity,
        transaction_id: &str,
        point: &ChargePoint,
        open: &Open,
    ) -> Vec<Event> {
        let key: Key = (identity.to_string(), transaction_id.to_owned());
        let transaction = &open.transaction;
        let at = self.now();
        let refuse = |reasons: Vec<String>, findings: Vec<emob_eichrecht::Finding>| Event::Noted {
            at,
            closes: Some(key.clone()),
            outcome: Outcome::Refused {
                identity: identity.to_string(),
                transaction: transaction_id.to_owned(),
                reasons,
                findings,
            },
        };

        let assembled = match transaction.assemble(Direction::Import) {
            Ok(assembled) => assembled,
            Err(error) => {
                let receipt = self.receipted(&key, point, None, Vec::new());
                return receipt
                    .into_iter()
                    .chain([refuse(vec![error.to_string()], Vec::new())])
                    .collect();
            }
        };

        // The version in force when the session started is the one the driver
        // was shown, so it is the one that rates `[AFIR Art. 5(4)]` (spec 015)
        // — and a tariff the point may not offer is refused before it prices
        // anything.
        let started = assembled.session.started_at;
        let Some(tariff) = point.tariff.in_force_at(started) else {
            let receipt = self.receipted(&key, point, None, Vec::new());
            return receipt
                .into_iter()
                .chain([refuse(
                    vec![format!(
                        "no version of tariff {} was in force when the session started",
                        point.tariff.id()
                    )],
                    Vec::new(),
                )])
                .collect();
        };
        let conformance = emob_tariff::check_afir(tariff, point.rated_power_kw);
        if !conformance.is_lawful() {
            let receipt = self.receipted(&key, point, None, Vec::new());
            return receipt
                .into_iter()
                .chain([refuse(conformance.reasons().collect(), Vec::new())])
                .collect();
        }

        // The receipt first: the verdict fact that follows completes it, and a
        // settlement the station retries files the same receipt again.
        let mut facts: Vec<Event> = self
            .receipted(
                &key,
                point,
                Some(assembled.session.started_at),
                assembled
                    .records
                    .iter()
                    .map(|record| record.as_str().to_owned())
                    .collect(),
            )
            .into_iter()
            .collect();
        if let Some(finding) = self.check_claimed_keys(identity, transaction, &assembled) {
            facts.push(Event::Noted {
                at,
                closes: None,
                outcome: finding,
            });
        }

        // Against the **registry**, never against the key the station sent
        // beside the record.
        let borrowed: Vec<ocmf::Record<'_>> = assembled
            .records
            .iter()
            .filter_map(|r| r.record().ok())
            .collect();
        let evidence = Evidence::assemble(&borrowed, &self.registry, assembled.session.started_at);

        // The record's identity is the station's and the transaction's, and it
        // has to stay **unique** (D249).
        let Ok(id) = format!("{identity}-{transaction_id}").parse() else {
            facts.push(refuse(
                vec![
                    "neither the station identity nor the transaction id names anything, so this \
                 record has no id that would be unique in the ledger"
                        .to_owned(),
                ],
                Vec::new(),
            ));
            return facts;
        };
        let cdr =
            CdrBuilder::from_session(&assembled.session, Direction::Import).and_then(|builder| {
                builder
                    .key(self.party.clone(), id)
                    .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
                    .rated_with(tariff)
                    .build()
            });

        let cdr = match cdr {
            Ok(cdr) => cdr,
            Err(error) => {
                let mut reasons = vec![error.to_string()];
                reasons.extend(evidence.reasons());
                facts.push(refuse(reasons, evidence.findings()));
                return facts;
            }
        };

        let energy = cdr.total_energy.to_string();
        let record_key = cdr.key.to_string();
        let retries = assembled.duplicates_dropped + open.retried_messages;
        // What the ledger would answer — read here, decided by the fold, and
        // the same answer both times because the ledger is a function of the
        // facts before this one.
        let acceptance = {
            let folded = lock(&self.folded);
            folded
                .state()
                .cdrs
                .get(&cdr.key)
                .map_or(Acceptance::Stored, |held| {
                    let mut probe = CdrLedger::new();
                    let _ = probe.accept(held.clone());
                    probe.accept(cdr.clone())
                })
        };
        let outcome = outcome_of(
            &acceptance,
            Accepted {
                key: &record_key,
                energy: &energy,
                retries,
                gap: &open.missing_messages,
                identity: &identity.to_string(),
                transaction: transaction_id,
            },
        );

        // What the meter signed about itself, from a record that just
        // billed — so its signature verified against the registry — and only
        // when it differs from what the fold holds for this point (SR-136).
        if matches!(acceptance, Acceptance::Stored)
            && let Some(meter) = borrowed
                .first()
                .and_then(|record| station::MeterIdentity::of(record.payload()))
        {
            let evse = point.evse_id.to_string();
            let known = lock(&self.folded)
                .state()
                .stations
                .get(identity.as_str())
                .and_then(|known| known.meters.get(&evse))
                .map(|(held, _)| held.clone());
            if known.as_ref() != Some(&meter) {
                facts.push(Event::Metered {
                    station: identity.to_string(),
                    evse,
                    meter,
                    at,
                });
            }
        }

        // Owed to the invoice and to the partner — when somebody pays, and
        // only the first time the record is stored.
        let deliveries = match (&acceptance, self.token_for(&key)) {
            (Acceptance::Stored, Some(token)) => {
                let data = CdrDelivery {
                    cdr: &cdr,
                    token: &token,
                    signed: assembled
                        .records
                        .iter()
                        .map(|record| record.as_str().to_owned())
                        .collect(),
                    station: &key.0,
                    point: self.registered(&key.0).map(|station| CdrPoint {
                        evse: station.evse,
                        connector: station.connector,
                        rated_power_kw: station.rated_power_kw,
                        location: station.location,
                    }),
                };
                let event = CloudEvent::new(
                    record_key.clone(),
                    "csmsd",
                    emob_service::events::cdr::ISSUED,
                    data,
                )
                .about(format!("cdr/{record_key}"))
                .at(self.now());
                match serde_json::to_string(&event) {
                    Ok(body) => Target::ESTATE
                        .into_iter()
                        .chain(self.subscribers.iter().cloned().map(Target::Subscriber))
                        .map(|to| Delivery {
                            id: record_key.clone(),
                            to,
                            body: body.clone(),
                        })
                        .collect(),
                    Err(error) => {
                        tracing::error!(%error, key = %record_key, "a delivery could not be encoded");
                        Vec::new()
                    }
                }
            }
            (Acceptance::Stored, None) => {
                tracing::warn!(
                    key = %record_key,
                    "settled with no provider named, so nobody is owed this record"
                );
                Vec::new()
            }
            _ => Vec::new(),
        };

        facts.push(Event::Settled {
            at,
            station: key.0.clone(),
            transaction: key.1.clone(),
            cdr: Box::new(cdr),
            outcome,
            deliveries,
        });
        facts
    }

    /// The token a transaction started under.
    fn token_for(&self, key: &Key) -> Option<Presented> {
        lock(&self.folded)
            .state()
            .authorized
            .get(key)
            .and_then(|answer| answer.token.clone())
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
    ) -> Option<Outcome> {
        let Some(Ok(record)) = assembled.records.first().map(ocmf::RecordBuf::record) else {
            return None;
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
                        return Some(Outcome::UnreadableClaimedKey {
                            identity: identity.to_string(),
                            detail: error.to_string(),
                        });
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
                        return Some(Outcome::UnreadableClaimedKey {
                            identity: identity.to_string(),
                            detail: error.to_string(),
                        });
                    }
                };
                if registered.as_ref() == Some(&claimed) {
                    continue;
                }
                return Some(Outcome::KeyMismatch {
                    identity: identity.to_string(),
                    claimed: hex_of(claimed.sec1_bytes()),
                });
            }
        }
        None
    }

    /// The billing half, for all three versions at once.
    ///
    /// One funnel: [`observe_v16`], [`observe_v201`] and [`observe_v21`] all
    /// produce the same [`Observed`], and `emob-ocpp` reads the billable event
    /// out of it.
    ///
    /// A [`WarningKind::UnreadableSignedData`] is refused before anything else
    /// looks at the event: a station that declares `format: SignedData` and
    /// sends a document that does not parse is otherwise **indistinguishable
    /// from one sending no signed data at all**. The message is still
    /// answered — refusing the RPC would only make the station retry.
    fn bill(
        &self,
        by: &Actor,
        ctx: &Ctx,
        transaction: &str,
        observed: &Observed,
        ingested: Option<Ingested>,
    ) -> Result<(), CallError> {
        let identity = ctx.identity();
        let unreadable: Vec<String> = observed
            .warnings
            .iter()
            .filter(|warning| warning.kind == WarningKind::UnreadableSignedData)
            .map(ToString::to_string)
            .collect();
        if !unreadable.is_empty() {
            self.record(
                by,
                Event::Noted {
                    at: self.now(),
                    closes: None,
                    outcome: Outcome::Refused {
                        identity: identity.to_string(),
                        transaction: transaction.to_owned(),
                        reasons: unreadable,
                        findings: Vec::new(),
                    },
                },
            )?;
        }

        if let Some(event) = emob_ocpp::kit::event_from(&observed.event) {
            self.accept(by, ctx, transaction, event, ingested)?;
        }
        Ok(())
    }

    /// The operational half: arrival, sequence, retries.
    fn record_operational(&self, identity: &Identity, observed: &Observed) -> Option<Ingested> {
        let event = ocpp_kit::csms::events::to_ledger_event(identity, observed)?;
        let mut ledger = lock(&self.operational);
        Some(if observed.version == Version::V1_6 {
            ledger.ingest_unsequenced(&event)
        } else {
            ledger.ingest(&event)
        })
    }

    /// The next transaction id for a 1.6 station, journalled before it is
    /// handed out — a counter a restart rewound would give the next station an
    /// id an earlier transaction already carries.
    fn allocate_1_6_transaction(&self, by: &Actor) -> Result<i32, CallError> {
        let id = lock(&self.folded).state().next_1_6;
        self.record(by, Event::Allocated { id })?;
        Ok(id)
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
        findings: Vec::new(),
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
/// The poison flag says a thread died holding the lock and nothing about
/// whether the state still makes sense; the journal is what decides that.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The handler behind an `Arc`, so the state outlives the server and a test
/// can read it.
pub struct Shared<J: Journal>(pub Arc<Csmsd<J>>);

impl<J: Journal> Handler for Shared<J> {
    fn on_request(
        &self,
        ctx: Ctx,
        request: IncomingRequest,
    ) -> BoxFuture<'_, Result<Box<RawValue>, CallError>> {
        // Who every fact this message causes is journalled under: the identity
        // the edge authenticated the socket as, never a field of the message.
        // One outside the actor alphabet is refused, not defaulted — the
        // estate refuses such a station at start, so this is the backstop.
        let by = Actor::station(&ctx.identity().to_string());
        let span = match &by {
            Ok(actor) => tracing::info_span!("station", actor = %actor),
            Err(_) => tracing::info_span!("station"),
        };
        Box::pin(
            async move {
                let by = by.map_err(|error| CallError::security(error.to_string()))?;
                self.0.heard(&by, ctx.identity())?;
                match ctx.version() {
                    Version::V1_6 => self.0.on_v1_6(&by, &ctx, &request).await,
                    Version::V2_0_1 => self.0.on_v2_0_1(&by, &ctx, &request).await,
                    _ => self.0.on_v2_1(&by, &ctx, &request).await,
                }
            }
            .instrument(span),
        )
    }
}

impl<J: Journal> Csmsd<J> {
    async fn on_v1_6(
        &self,
        by: &Actor,
        ctx: &Ctx,
        request: &IncomingRequest,
    ) -> Result<Box<RawValue>, CallError> {
        let action = v1_6::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v1_6::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        match &typed {
            v1_6::CsRequest::BootNotification(boot) => {
                self.booted(
                    by,
                    ctx.identity(),
                    "PowerUp".to_owned(),
                    station::of_v1_6(boot),
                )?;
            }
            v1_6::CsRequest::FirmwareStatusNotification(note) => {
                self.reported(
                    by,
                    ctx.identity(),
                    station::Report::Firmware {
                        status: station::word(&note.status),
                    },
                )?;
                return ctx.reply(&v1_6::FirmwareStatusNotificationResponse::new());
            }
            v1_6::CsRequest::SecurityEventNotification(note) => {
                self.reported(
                    by,
                    ctx.identity(),
                    station::security(&note.r#type, note.tech_info.as_ref()),
                )?;
                return ctx.reply(&v1_6::SecurityEventNotificationResponse::new());
            }
            v1_6::CsRequest::DiagnosticsStatusNotification(_) => {
                return ctx.reply(&v1_6::DiagnosticsStatusNotificationResponse::new());
            }
            v1_6::CsRequest::LogStatusNotification(_) => {
                return ctx.reply(&v1_6::LogStatusNotificationResponse::new());
            }
            _ => {}
        }

        // The provider is asked before anything is billed or allocated an
        // answer: `Authorize`, and again at `StartTransaction`, which carries
        // the idTag the transaction bills against.
        match &typed {
            v1_6::CsRequest::Authorize(authorize) => {
                let answer = self
                    .authorize(by, ctx.identity(), None, &authorize.id_tag)
                    .await?;
                return ctx.reply(&v1_6::AuthorizeResponse::new(v1_6::IdTagInfo::new(
                    authorize::v1_6(answer.status),
                )));
            }
            v1_6::CsRequest::StartTransaction(start) => {
                // 1.6 assigns the id in the *response*, so the CSMS owns it.
                let id = self.allocate_1_6_transaction(by)?;
                let answer = self
                    .authorize(by, ctx.identity(), Some(id.to_string()), &start.id_tag)
                    .await?;
                let observed = observe_v16(&typed);
                let ingested = self.record_operational(ctx.identity(), &observed);
                if answer.is_accepted() {
                    self.bill(by, ctx, &id.to_string(), &observed, ingested)?;
                }
                return ctx.reply(&v1_6::StartTransactionResponse::new(
                    v1_6::IdTagInfo::new(authorize::v1_6(answer.status)),
                    id,
                ));
            }
            _ => {}
        }

        let observed = observe_v16(&typed);
        let ingested = self.record_operational(ctx.identity(), &observed);
        let transaction_id = match &typed {
            v1_6::CsRequest::MeterValues(values) => values.transaction_id,
            v1_6::CsRequest::StopTransaction(stop) => Some(stop.transaction_id),
            _ => None,
        };
        if let Some(id) = transaction_id {
            self.bill(by, ctx, &id.to_string(), &observed, ingested)?;
        }
        answer_v1_6(ctx, &typed, self.now())
    }

    async fn on_v2_0_1(
        &self,
        by: &Actor,
        ctx: &Ctx,
        request: &IncomingRequest,
    ) -> Result<Box<RawValue>, CallError> {
        let action = v2_0_1::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v2_0_1::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        match &typed {
            v2_0_1::CsRequest::BootNotification(boot) => {
                self.booted(
                    by,
                    ctx.identity(),
                    station::word(&boot.reason),
                    station::of_v2_0_1(boot),
                )?;
                self.show_price(ctx.identity());
                answer_v2_0_1(ctx, &typed, self.now())
            }
            v2_0_1::CsRequest::FirmwareStatusNotification(note) => {
                self.reported(
                    by,
                    ctx.identity(),
                    station::Report::Firmware {
                        status: station::word(&note.status),
                    },
                )?;
                ctx.reply(&v2_0_1::FirmwareStatusNotificationResponse::new())
            }
            v2_0_1::CsRequest::SecurityEventNotification(note) => {
                self.reported(
                    by,
                    ctx.identity(),
                    station::security(&note.r#type, note.tech_info.as_ref()),
                )?;
                ctx.reply(&v2_0_1::SecurityEventNotificationResponse::new())
            }
            v2_0_1::CsRequest::LogStatusNotification(_) => {
                ctx.reply(&v2_0_1::LogStatusNotificationResponse::new())
            }
            v2_0_1::CsRequest::Authorize(authorize) => {
                let answer = self
                    .authorize(by, ctx.identity(), None, &authorize.id_token.id_token)
                    .await?;
                ctx.reply(&v2_0_1::AuthorizeResponse::new(v2_0_1::IdTokenInfo::new(
                    authorize::v2_0_1(answer.status),
                )))
            }
            v2_0_1::CsRequest::TransactionEvent(event) => {
                let transaction = event.transaction_info.transaction_id.clone();
                let asked = match &event.id_token {
                    Some(token) if !self.answered(ctx.identity(), &transaction) => Some(
                        self.authorize(
                            by,
                            ctx.identity(),
                            Some(transaction.clone()),
                            &token.id_token,
                        )
                        .await?,
                    ),
                    _ => None,
                };
                let observed = observe_v201(&typed);
                let ingested = self.record_operational(ctx.identity(), &observed);
                self.bill(by, ctx, &transaction, &observed, ingested)?;
                let mut response = v2_0_1::TransactionEventResponse::new();
                if let Some(answer) = asked {
                    response = response.with_id_token_info(v2_0_1::IdTokenInfo::new(
                        authorize::v2_0_1(answer.status),
                    ));
                }
                ctx.reply(&response)
            }
            other => answer_v2_0_1(ctx, other, self.now()),
        }
    }

    async fn on_v2_1(
        &self,
        by: &Actor,
        ctx: &Ctx,
        request: &IncomingRequest,
    ) -> Result<Box<RawValue>, CallError> {
        let action = v2_1::Action::from_wire(&request.action)
            .ok_or_else(|| CallError::not_implemented(&request.action))?;
        let typed = v2_1::CsRequest::decode(action, &request.payload, ctx.decode_options())?;
        match &typed {
            v2_1::CsRequest::BootNotification(boot) => {
                self.booted(
                    by,
                    ctx.identity(),
                    station::word(&boot.reason),
                    station::of_v2_1(boot),
                )?;
                self.show_price(ctx.identity());
                answer_v2_1(ctx, &typed, self.now())
            }
            v2_1::CsRequest::FirmwareStatusNotification(note) => {
                self.reported(
                    by,
                    ctx.identity(),
                    station::Report::Firmware {
                        status: station::word(&note.status),
                    },
                )?;
                ctx.reply(&v2_1::FirmwareStatusNotificationResponse::new())
            }
            v2_1::CsRequest::SecurityEventNotification(note) => {
                self.reported(
                    by,
                    ctx.identity(),
                    station::security(&note.r#type, note.tech_info.as_ref()),
                )?;
                ctx.reply(&v2_1::SecurityEventNotificationResponse::new())
            }
            v2_1::CsRequest::LogStatusNotification(_) => {
                ctx.reply(&v2_1::LogStatusNotificationResponse::new())
            }
            v2_1::CsRequest::Authorize(authorize) => {
                let answer = self
                    .authorize(by, ctx.identity(), None, &authorize.id_token.id_token)
                    .await?;
                ctx.reply(&v2_1::AuthorizeResponse::new(v2_1::IdTokenInfo::new(
                    authorize::v2_1(answer.status),
                )))
            }
            v2_1::CsRequest::TransactionEvent(event) => {
                let transaction = event.transaction_info.transaction_id.clone();
                let asked = match &event.id_token {
                    Some(token) if !self.answered(ctx.identity(), &transaction) => Some(
                        self.authorize(
                            by,
                            ctx.identity(),
                            Some(transaction.clone()),
                            &token.id_token,
                        )
                        .await?,
                    ),
                    _ => None,
                };
                let observed = observe_v21(&typed);
                let ingested = self.record_operational(ctx.identity(), &observed);
                self.bill(by, ctx, &transaction, &observed, ingested)?;
                let mut response = v2_1::TransactionEventResponse::new();
                if let Some(answer) = asked {
                    response = response
                        .with_id_token_info(v2_1::IdTokenInfo::new(authorize::v2_1(answer.status)));
                }
                ctx.reply(&response)
            }
            other => answer_v2_1(ctx, other, self.now()),
        }
    }

    /// Whether the provider has already answered for this transaction.
    fn answered(&self, identity: &Identity, transaction: &str) -> bool {
        let key = (identity.to_string(), transaction.to_owned());
        let folded = lock(&self.folded);
        folded.state().authorized.contains_key(&key) || folded.state().refused.contains(&key)
    }
}

/// A key as hex, for an operator comparing it against a cabinet.
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The instant a station is told in `BootNotification` and `Heartbeat`.
///
/// A station sets its own clock from this field, and every timestamp it then
/// puts on a transaction and a meter value follows from it — so it is this
/// CSMS's clock, never a constant: a simulated station ignores a wrong one and
/// a real one obeys it.
fn current_time(now: time::OffsetDateTime) -> DateTime {
    now.to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .ok()
        .and_then(|instant| {
            instant
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .and_then(|text| DateTime::parse(&text).ok())
        .unwrap_or(DateTime::UNIX_EPOCH)
}

/// The 1.6 replies that need no decision.
fn answer_v1_6(
    ctx: &Ctx,
    request: &v1_6::CsRequest,
    now: time::OffsetDateTime,
) -> Result<Box<RawValue>, CallError> {
    match request {
        v1_6::CsRequest::BootNotification(_) => ctx.reply(&v1_6::BootNotificationResponse::new(
            v1_6::RegistrationStatus::Accepted,
            current_time(now),
            HEARTBEAT_SECS,
        )),
        v1_6::CsRequest::Heartbeat(_) => {
            ctx.reply(&v1_6::HeartbeatResponse::new(current_time(now)))
        }
        v1_6::CsRequest::StatusNotification(_) => {
            ctx.reply(&v1_6::StatusNotificationResponse::new())
        }
        v1_6::CsRequest::MeterValues(_) => ctx.reply(&v1_6::MeterValuesResponse::new()),
        v1_6::CsRequest::StopTransaction(_) => ctx.reply(&v1_6::StopTransactionResponse::new()),
        other => Err(CallError::not_supported(other.action().as_str())),
    }
}

/// The 2.0.1 replies that need no decision.
fn answer_v2_0_1(
    ctx: &Ctx,
    request: &v2_0_1::CsRequest,
    now: time::OffsetDateTime,
) -> Result<Box<RawValue>, CallError> {
    match request {
        v2_0_1::CsRequest::BootNotification(_) => {
            ctx.reply(&v2_0_1::BootNotificationResponse::new(
                current_time(now),
                HEARTBEAT_SECS,
                v2_0_1::RegistrationStatus::Accepted,
            ))
        }
        v2_0_1::CsRequest::Heartbeat(_) => {
            ctx.reply(&v2_0_1::HeartbeatResponse::new(current_time(now)))
        }
        v2_0_1::CsRequest::StatusNotification(_) => {
            ctx.reply(&v2_0_1::StatusNotificationResponse::new())
        }
        other => Err(CallError::not_supported(other.action().as_str())),
    }
}

/// The 2.1 replies that need no decision.
fn answer_v2_1(
    ctx: &Ctx,
    request: &v2_1::CsRequest,
    now: time::OffsetDateTime,
) -> Result<Box<RawValue>, CallError> {
    match request {
        v2_1::CsRequest::BootNotification(_) => ctx.reply(&v2_1::BootNotificationResponse::new(
            current_time(now),
            HEARTBEAT_SECS,
            v2_1::RegistrationStatus::Accepted,
        )),
        v2_1::CsRequest::Heartbeat(_) => {
            ctx.reply(&v2_1::HeartbeatResponse::new(current_time(now)))
        }
        v2_1::CsRequest::StatusNotification(_) => {
            ctx.reply(&v2_1::StatusNotificationResponse::new())
        }
        other => Err(CallError::not_supported(other.action().as_str())),
    }
}

/// What a display call came to, as one word for the log line.
fn display_status(
    outcome: Result<Result<String, ocpp_kit::engine::CallFailure>, tokio::time::error::Elapsed>,
) -> String {
    match outcome {
        Ok(Ok(status)) => status,
        Ok(Err(failure)) => display::failure_name(&failure).to_owned(),
        Err(_) => "no answer".to_owned(),
    }
}

impl<J: Journal> edge::Registry for Csmsd<J> {
    fn admit(&self, identity: &str) -> Option<(ChargePoint, emob_service::estate::Station)> {
        let station = self.registered(identity)?;
        let point = registry::charge_point(&station, &self.tariffs)?;
        Some((point, station))
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
