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

mod connector;

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
use crate::station::{MeterIdentity, Report, StationIdentity};

/// A transaction, by the station that runs it and the id it carries.
pub type Key = (String, String);

/// One fact `csmsd` decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum Event {
    /// Direct response to the current OCPP 2.1 station tariff.
    StationTariff {
        /// Station the CSMS addressed.
        station: String,
        /// What was attempted and observed.
        delivery: StationTariff,
    },
    /// Public terms activated by an estate configuration change.
    PublicOfferPublished {
        /// The reproducible terms.
        snapshot: Box<emob_service::estate::RetailTerms>,
    },
    /// The daemon restarted and pre-restart connector availability became unknown.
    PublicationRestarted {
        /// Restart time on the source clock.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A timestamped connector status from the registered single-point station.
    ConnectorObserved {
        /// Reporting station.
        station: String,
        /// Receipt time on the CSMS clock. Earlier journals filtered future timestamps before append.
        #[serde(with = "time::serde::rfc3339::option", default)]
        received_at: Option<time::OffsetDateTime>,
        /// Source status and time.
        observation: Option<emob_service::publication::ConnectorObservation>,
    },
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
    /// A station was heard while the fold held it offline or unknown
    /// (spec 012) — a transition, never a heartbeat (SR-138).
    Online {
        /// Which station.
        station: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A station was silent past the heartbeat interval, found by the sweep.
    Offline {
        /// Which station.
        station: String,
        /// When it was last heard from.
        #[serde(with = "time::serde::rfc3339")]
        last_heard: time::OffsetDateTime,
        /// When the sweep found it.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A station booted and said what it is.
    Booted {
        /// Which station.
        station: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
        /// Why, in the station's word (`PowerUp`, `RemoteReset`, …).
        reason: String,
        /// What it says it is.
        identity: StationIdentity,
    },
    /// A billed session's signed records name a meter other than the one the
    /// fold holds for that point (SR-136).
    Metered {
        /// Which station.
        station: String,
        /// The point.
        evse: String,
        /// What the meter signed about itself.
        meter: MeterIdentity,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A station joined the registry (spec 021) — from the estate at start,
    /// or from an operator. Only the password's hash is ever in it (SR-144).
    StationRegistered {
        /// Newly applicable public provider terms, atomic with the assignment.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        offers: Vec<emob_service::estate::RetailTerms>,
        /// The record, as the estate spells a station.
        station: Box<emob_service::estate::Station>,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A registered station's record was replaced.
    StationUpdated {
        /// Newly applicable public provider terms.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        offers: Vec<emob_service::estate::RetailTerms>,
        /// The new record.
        station: Box<emob_service::estate::Station>,
        /// Why, in the operator's words.
        reason: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A station left the registry; its next connection is refused.
    StationRetired {
        /// Which.
        identity: String,
        /// Why.
        reason: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// An operator's note on a station (spec 023).
    StationNoted {
        /// Which station.
        identity: String,
        /// Its number on that station, from 1.
        number: u64,
        /// Who wrote it, as the actor reads.
        by: String,
        /// What it says, cleaned and bounded (SR-154).
        text: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A note taken off a station; the fact that wrote it stays.
    NoteRemoved {
        /// Which station.
        identity: String,
        /// Which note.
        number: u64,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A point held for one card until a time (spec 025). The card is its
    /// provider's reference and a masked tail, never the UID (SR-176).
    Reserved {
        /// Which station.
        station: String,
        /// Its number on that station, from 1 — the `reservationId` sent.
        number: u64,
        /// The provider's keyed digest of the card's UID.
        reference: String,
        /// The UID's last two characters, for a person to recognise it.
        card: String,
        /// Held until.
        #[serde(with = "time::serde::rfc3339")]
        until: time::OffsetDateTime,
        /// Who asked, as the actor reads.
        by: String,
        /// Why.
        reason: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A reservation is over.
    ReservationEnded {
        /// Which station.
        station: String,
        /// Which reservation.
        number: u64,
        /// How.
        how: Ended,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    },
    /// A station's firmware or security report (SR-140).
    Reported {
        /// Which station.
        station: String,
        /// When.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
        /// What it reported.
        report: Report,
    },
}

impl Kinded for Event {
    fn kind(&self) -> &'static str {
        match self {
            Self::PublicOfferPublished { .. } => "public_offer_published",
            Self::Allocated { .. } => "allocated",
            Self::Authorized { .. } => "authorized",
            Self::Observed { .. } => "observed",
            Self::Retried { .. } => "retried",
            Self::Settled { .. } => "settled",
            Self::Receipted { .. } => "receipted",
            Self::Noted { .. } => "noted",
            Self::Delivered { .. } => "delivered",
            Self::Refused { .. } => "refused",
            Self::Online { .. } => "online",
            Self::Offline { .. } => "offline",
            Self::Booted { .. } => "booted",
            Self::Metered { .. } => "metered",
            Self::Reported { .. } => "reported",
            Self::StationTariff { .. } => "station_tariff",
            Self::PublicationRestarted { .. } => "publication_restarted",
            Self::ConnectorObserved { .. } => "connector_observed",
            Self::StationRegistered { .. } => "station_registered",
            Self::StationUpdated { .. } => "station_updated",
            Self::StationRetired { .. } => "station_retired",
            Self::StationNoted { .. } => "station_noted",
            Self::NoteRemoved { .. } => "note_removed",
            Self::Reserved { .. } => "reserved",
            Self::ReservationEnded { .. } => "reservation_ended",
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
    /// Final refusals prevent a new charge from reusing the driver's original transaction identity.
    pub(crate) closed_refusals: BTreeSet<Key>,
    pub(crate) investigations: crate::investigation::Investigations,
    /// Revision used for consistent paged publication.
    pub publication_revision: u64,
    /// Site revisions across changes of station membership and metadata.
    pub site_versions: BTreeMap<String, u32>,
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
    /// What the platform knows of each station it has heard from (spec 012).
    pub stations: BTreeMap<String, Known>,
    /// Who may connect, and as what (spec 021): the registered stations, by
    /// identity. A retired one is not here.
    pub registry: BTreeMap<String, emob_service::estate::Station>,
    /// Live point to registry identity, rebuilt on replay.
    pub evse_registry: BTreeMap<String, String>,
    /// Public offer versions retained for drivers.
    pub offer_archive: crate::provider_price::Archive,
}

/// One attempt to install the effective tariff on a station.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationTariff {
    /// Full tariff fingerprint.
    pub version: String,
    /// Identity of this registration.
    pub facility_version: u32,
    /// Monotone boot count, including two boots at the same clock instant. Zero denotes a legacy receipt.
    #[serde(default)]
    pub boot_generation: u64,
    /// The boot answered before sending the request.
    #[serde(with = "time::serde::rfc3339")]
    pub boot: time::OffsetDateTime,
    /// When the attempt ended.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// Station response or local representation/transport failure.
    pub outcome: String,
    /// A later attempt may succeed after a transport failure.
    pub retry: bool,
}

/// One station, as the facts describe it. Every list is bounded
/// ([`crate::station::KEPT`]).
#[derive(Debug, Clone, Default)]
pub struct Known {
    /// Current boot and registry version’s tariff delivery.
    pub station_tariff: Option<StationTariff>,
    /// Monotone revision of this identity, including re-registration.
    pub facility_version: u32,
    /// The latest registration or change.
    pub registered_at: Option<time::OffsetDateTime>,
    /// Last boot still eligible for tariff deliveries on the current registration.
    pub tariff_boot: Option<time::OffsetDateTime>,
    /// Number of boot facts folded for this station, including repeated timestamps.
    pub boot_generation: u64,
    /// Device observation and the source boundaries that can invalidate it.
    connector: connector::ConnectorState,
    /// Online since, or offline since and last heard.
    pub presence: Option<Presence>,
    /// What it said at its last boot, and when.
    pub identity: Option<(StationIdentity, time::OffsetDateTime)>,
    /// The meter each point's records named last, and since when.
    pub meters: BTreeMap<String, (MeterIdentity, time::OffsetDateTime)>,
    /// Firmware changes, newest last — the intervention record.
    pub firmware: Vec<FirmwareChange>,
    /// Firmware and security reports, newest last.
    pub reports: Vec<(time::OffsetDateTime, Report)>,
    /// Its last boots, newest last.
    pub boots: Vec<time::OffsetDateTime>,
    /// Its last sessions, newest last ([`RECENT`]).
    pub recent: Vec<Recent>,
    /// Its notes, newest last ([`NOTES`]).
    pub notes: Vec<Note>,
    /// How many notes were ever written on it — the next one's number.
    pub notes_written: u64,
    /// The reservation holding it, if one is open (spec 025).
    pub reservation: Option<Reservation>,
    /// How many reservations were ever made on it — the next one's number.
    pub reservations_made: u64,
}

impl Known {
    /// An existing station report might have to become unknown at daemon startup.
    pub(crate) fn has_connector_report(&self) -> bool {
        self.connector.has_report()
    }

    /// A sweeper journals the end later; publication changes at the actual reservation deadline.
    pub(crate) fn projected_expiry(
        &self,
        now: time::OffsetDateTime,
    ) -> Option<time::OffsetDateTime> {
        let until = self.reservation.as_ref()?.until;
        (until <= now && self.connector.reserved_as_of(until)).then_some(until)
    }

    /// Current public connector evidence and the time the effective source state last changed.
    pub(crate) fn public_status(
        &self,
        now: time::OffsetDateTime,
    ) -> (
        Option<emob_service::publication::ConnectorObservation>,
        Option<time::OffsetDateTime>,
    ) {
        self.projected_expiry(now)
            .map_or_else(|| self.connector.projection(), |end| (None, Some(end)))
    }
}

/// A point held for one card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reservation {
    /// Its number on the station.
    pub number: u64,
    /// The provider's reference for the card; never serialised to a view.
    #[serde(skip)]
    pub reference: String,
    /// The card's masked tail, `…F6`.
    pub card: String,
    /// Held until.
    #[serde(with = "time::serde::rfc3339")]
    pub until: time::OffsetDateTime,
    /// Who asked.
    pub by: String,
}

/// How a reservation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// The reserved card started charging.
    Honoured,
    /// The operator cancelled it.
    Cancelled,
    /// Its time passed.
    Expired,
    /// The station ended it (2.x `ReservationStatusUpdate` `Removed`).
    Removed,
}

/// How many notes a station's page and fold keep (SR-154).
pub const NOTES: usize = 10;

/// An operator's note on a station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    /// Its number on the station.
    pub number: u64,
    /// Who wrote it.
    pub by: String,
    /// What it says.
    pub text: String,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
}

/// How many sessions a station's page lists.
pub const RECENT: usize = 20;

/// One session, as a station's page lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recent {
    /// The immutable outcome position.
    pub position: u64,
    /// The transaction.
    pub transaction: String,
    /// When it was decided.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// Whether it billed.
    pub billed: bool,
}

impl State {
    fn remember(
        &mut self,
        station: &str,
        transaction: &str,
        at: time::OffsetDateTime,
        billed: bool,
    ) {
        let recent = &mut self.stations.entry(station.to_owned()).or_default().recent;
        recent.push(Recent {
            position: self.outcomes.len() as u64,
            transaction: transaction.to_owned(),
            at,
            billed,
        });
        if recent.len() > RECENT {
            recent.remove(0);
        }
    }
}

/// Whether a station is reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Heard from since the instant.
    Online(time::OffsetDateTime),
    /// Silent since `since`, last heard at `last_heard`.
    Offline {
        /// When the sweep found it.
        since: time::OffsetDateTime,
        /// When it was last heard from.
        last_heard: time::OffsetDateTime,
    },
}

/// One firmware change `[MessEG §31(2) Nr. 4]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirmwareChange {
    /// When it was first seen.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// `station` (its boot) or the point whose meter signed it.
    pub component: String,
    /// The version before, when one was known.
    pub from: Option<String>,
    /// The version after.
    pub to: String,
}

fn keep_last<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > crate::station::KEPT {
        list.remove(0);
    }
}

impl State {
    /// An empty state — what a fresh journal folds from — whose days are
    /// read in `zone`.
    #[must_use]
    pub fn new(zone: TimeZone) -> Self {
        Self {
            closed_refusals: BTreeSet::new(),
            investigations: Default::default(),
            publication_revision: 0,
            site_versions: BTreeMap::new(),
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
            stations: BTreeMap::new(),
            registry: BTreeMap::new(),
            evse_registry: BTreeMap::new(),
            offer_archive: crate::provider_price::Archive::default(),
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
        Event::StationTariff { station, delivery } => {
            if !state.registry.contains_key(&station) {
                return;
            }
            let known = state.stations.entry(station).or_default();
            if known.facility_version == delivery.facility_version
                && known.tariff_boot == Some(delivery.boot)
                && (delivery.boot_generation == 0
                    || known.boot_generation == delivery.boot_generation)
            {
                known.station_tariff = Some(delivery);
            }
        }
        Event::PublicationRestarted { at } => {
            for known in state.stations.values_mut() {
                if let Some(expiry) = known.projected_expiry(at) {
                    known.connector.release(expiry);
                }
                known.connector.invalidate(at, false);
            }
            state.publication_revision += 1;
        }
        Event::ConnectorObserved {
            station,
            received_at,
            observation,
        } => {
            if state.registry.contains_key(&station)
                && let Some(known) = state.stations.get_mut(&station)
                && !matches!(known.presence, Some(Presence::Offline { .. }))
            {
                let expiry = received_at
                    .or_else(|| observation.as_ref().map(|report| report.at))
                    .and_then(|now| known.projected_expiry(now));
                if let Some(at) = expiry {
                    known.connector.release(at);
                }
                let changed = match observation {
                    Some(report) => known.connector.observe(
                        report,
                        received_at,
                        known.reservation.as_ref().map(|r| r.until),
                    ),
                    None => received_at.is_some_and(|at| known.connector.lose_clock(at)),
                };
                if changed || expiry.is_some() {
                    state.publication_revision += 1;
                }
            }
        }
        Event::PublicOfferPublished { snapshot } => state.offer_archive.insert(*snapshot),
        Event::StationRegistered {
            station,
            at,
            offers,
        }
        | Event::StationUpdated {
            station,
            at,
            offers,
            ..
        } => {
            state.publication_revision += 1;
            let known = state.stations.entry(station.identity.clone()).or_default();
            known.facility_version = known.facility_version.saturating_add(1);
            known.registered_at = Some(at);
            if !state.registry.contains_key(&station.identity) {
                known.tariff_boot = None;
            }
            known.station_tariff = None;
            known.connector.invalidate(at, true);
            let site = station.location.id.clone();
            *state.site_versions.entry(site.clone()).or_default() += 1;
            let identity = station.identity.clone();
            let evse = station.evse.to_string();
            if let Some(old) = state.registry.insert(identity.clone(), *station) {
                if old.location.id != site {
                    *state
                        .site_versions
                        .entry(old.location.id.clone())
                        .or_default() += 1;
                }
                state.evse_registry.remove(&old.evse.to_string());
            }
            state.evse_registry.insert(evse, identity);
            for offer in offers {
                state.offer_archive.insert(offer);
            }
        }
        Event::StationRetired { identity, .. } => {
            state.publication_revision += 1;
            if let Some(known) = state.stations.get_mut(&identity) {
                known.tariff_boot = None;
                known.station_tariff = None;
            }
            if let Some(old) = state.registry.remove(&identity) {
                *state
                    .site_versions
                    .entry(old.location.id.clone())
                    .or_default() += 1;
                state.evse_registry.remove(&old.evse.to_string());
            }
        }
        Event::StationNoted {
            identity,
            number,
            by,
            text,
            at,
        } => {
            let known = state.stations.entry(identity).or_default();
            known.notes_written = known.notes_written.max(number);
            known.notes.push(Note {
                number,
                by,
                text,
                at,
            });
            if known.notes.len() > NOTES {
                known.notes.remove(0);
            }
        }
        Event::Reserved {
            station,
            number,
            reference,
            card,
            until,
            by,
            at,
            ..
        } => {
            let known = state.stations.entry(station).or_default();
            known.reservations_made = known.reservations_made.max(number);
            known.connector.reserve(at);
            state.publication_revision += 1;
            known.reservation = Some(Reservation {
                number,
                reference,
                card,
                until,
                by,
            });
        }
        Event::ReservationEnded {
            station,
            number,
            at,
            ..
        } => {
            if let Some(known) = state.stations.get_mut(&station)
                && let Some(held) = known
                    .reservation
                    .as_ref()
                    .filter(|held| held.number == number)
            {
                known.connector.release(at.min(held.until));
                state.publication_revision += 1;
                known.reservation = None;
            }
        }
        Event::NoteRemoved {
            identity, number, ..
        } => {
            if let Some(known) = state.stations.get_mut(&identity) {
                known.notes.retain(|note| note.number != number);
            }
        }
        Event::Online { station, at } => {
            state.publication_revision += 1;
            state.stations.entry(station).or_default().presence = Some(Presence::Online(at));
        }
        Event::Offline {
            station,
            last_heard,
            at,
        } => {
            state.publication_revision += 1;
            let known = state.stations.entry(station).or_default();
            known.connector.invalidate(at, false);
            known.presence = Some(Presence::Offline {
                since: at,
                last_heard,
            });
        }
        Event::Booted {
            station,
            at,
            identity,
            ..
        } => {
            let known = state.stations.entry(station).or_default();
            let before = known
                .identity
                .as_ref()
                .and_then(|(i, _)| i.firmware.clone());
            if let Some(to) = identity.firmware.clone()
                && known.identity.is_some()
                && before.as_deref() != Some(to.as_str())
            {
                keep_last(
                    &mut known.firmware,
                    FirmwareChange {
                        at,
                        component: "station".to_owned(),
                        from: before,
                        to,
                    },
                );
            }
            known.identity = Some((identity, at));
            known.presence = Some(Presence::Online(at));
            known.station_tariff = None;
            known.connector.invalidate(at, false);
            state.publication_revision += 1;
            known.tariff_boot = Some(at);
            known.boot_generation = known.boot_generation.saturating_add(1);
            keep_last(&mut known.boots, at);
        }
        Event::Metered {
            station,
            evse,
            meter,
            at,
        } => {
            let known = state.stations.entry(station).or_default();
            let before = known
                .meters
                .get(&evse)
                .and_then(|(m, _)| m.firmware_label().map(ToOwned::to_owned));
            if let Some(to) = meter.firmware_label()
                && known.meters.contains_key(&evse)
                && before.as_deref() != Some(to)
            {
                keep_last(
                    &mut known.firmware,
                    FirmwareChange {
                        at,
                        component: evse.clone(),
                        from: before,
                        to: to.to_owned(),
                    },
                );
            }
            known.meters.insert(evse, (meter, at));
        }
        Event::Reported {
            station,
            at,
            report,
        } => keep_last(
            &mut state.stations.entry(station).or_default().reports,
            (at, report),
        ),
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
            // A capability already handed to a driver must always open the
            // same signed evidence. An unfinished receipt, on the other hand,
            // follows the evidence the retried decision is actually about.
            if state
                .receipt_of
                .get(&key)
                .and_then(|digest| state.receipts.get(digest))
                .is_none_or(|issued| issued.verdict.is_none())
            {
                state.receipt_of.insert(key.clone(), receipt.digest.clone());
                state.receipts.insert(
                    receipt.digest.clone(),
                    Issued {
                        key,
                        receipt,
                        verdict: None,
                    },
                );
            }
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
            let issued = state
                .receipt_of
                .get(&key)
                .and_then(|digest| state.receipts.get_mut(digest))
                .filter(|issued| issued.verdict.is_none());
            let has_receipt = issued.is_some();
            if let Some(issued) = issued {
                match &outcome {
                    Outcome::Settled { .. } => {
                        issued.verdict = Some(Verdict::Billed(cdr.key.clone()));
                        state.receipt_for.insert(cdr.key.to_string(), key.clone());
                    }
                    Outcome::Refused {
                        reasons, findings, ..
                    } => {
                        issued.verdict = Some(Verdict::Refused {
                            reasons: reasons.clone(),
                            findings: findings.clone(),
                        });
                    }
                    _ => {}
                }
            }
            let open = state.inflight.remove(&key);
            state.authorized.remove(&key);
            if matches!(outcome, Outcome::Settled { .. } | Outcome::Refused { .. }) {
                state.investigations.record(
                    state.outcomes.len() as u64,
                    &key,
                    at,
                    matches!(outcome, Outcome::Settled { .. }),
                    open.as_ref(),
                    has_receipt,
                );
            }
            // A record stored is a success; a retry the ledger already held is
            // not a second attempt; a conflict is a record nobody may bill.
            match &outcome {
                Outcome::Settled { .. } => {
                    state.attempt(at, &station, Ending::Billed);
                    state.remember(&station, &key.1, at, true);
                }
                Outcome::Refused { .. } => {
                    state.attempt(at, &station, Ending::Failed(Cause::Evidence));
                    state.remember(&station, &key.1, at, false);
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
            let mut has_receipt = false;
            if let Outcome::Refused {
                identity,
                transaction,
                reasons,
                findings,
            } = &outcome
            {
                if let Some(key) = &closes
                    && let Some(issued) = state
                        .receipt_of
                        .get(key)
                        .and_then(|digest| state.receipts.get_mut(digest))
                        .filter(|issued| issued.verdict.is_none())
                {
                    issued.verdict = Some(Verdict::Refused {
                        reasons: reasons.clone(),
                        findings: findings.clone(),
                    });
                    has_receipt = true;
                }
                let key = (identity.clone(), transaction.clone());
                let open = state.inflight.get(&key);
                state.investigations.record(
                    state.outcomes.len() as u64,
                    &key,
                    at,
                    false,
                    open,
                    has_receipt,
                );
            }
            if let Some(key) = closes {
                state.authorized.remove(&key);
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
                    state.remember(&key.0, &key.1, at, false);
                    state.closed_refusals.insert(key);
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

    fn registered_point() -> State {
        let mut state = berlin();
        let mut estate: crate::estate::Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .unwrap();
        apply(
            &mut state,
            Event::StationRegistered {
                station: Box::new(estate.stations.remove(0)),
                at: AT,
                offers: vec![],
            },
        );
        state
    }

    fn at(seconds: i64) -> time::OffsetDateTime {
        AT + time::Duration::seconds(seconds)
    }

    fn report(state: &mut State, status: &str, reported: i64, received: i64) {
        apply(
            state,
            Event::ConnectorObserved {
                station: "CP-1".into(),
                received_at: Some(at(received)),
                observation: Some(emob_service::publication::ConnectorObservation {
                    status: status.into(),
                    at: at(reported),
                }),
            },
        );
    }

    fn status(
        state: &State,
        now: i64,
    ) -> (
        Option<(String, time::OffsetDateTime)>,
        Option<time::OffsetDateTime>,
    ) {
        let (observation, changed) = state.stations["CP-1"].public_status(at(now));
        (observation.map(|o| (o.status, o.at)), changed)
    }

    #[test]
    fn sr_207_a_conflicted_or_future_station_clock_cannot_restore_availability_on_retry() {
        let mut state = registered_point();
        assert_eq!(status(&state, 0), (None, Some(AT)));
        report(&mut state, "Available", 10, 11);
        report(&mut state, "Available", 12, 13);
        assert_eq!(
            status(&state, 13),
            (Some(("Available".into(), at(12))), Some(at(10)))
        );
        let revision = state.publication_revision;
        report(&mut state, "Available", 12, 13);
        assert_eq!(state.publication_revision, revision);
        report(&mut state, "Faulted", 12, 14);
        report(&mut state, "Available", 12, 15);
        report(&mut state, "Faulted", 11, 16);
        assert_eq!(status(&state, 16), (None, Some(at(12))));
        report(&mut state, "Occupied", 17, 17);
        report(&mut state, "Available", 900, 18);
        assert_eq!(status(&state, 18), (None, Some(at(18))));
        report(&mut state, "Available", 17, 19);
        assert_eq!(status(&state, 19), (None, Some(at(18))));
        report(&mut state, "Faulted", 20, 20);
        assert_eq!(
            status(&state, 20),
            (Some(("Faulted".into(), at(20))), Some(at(20)))
        );
        apply(
            &mut state,
            Event::ConnectorObserved {
                station: "CP-1".into(),
                received_at: Some(at(21)),
                observation: None,
            },
        );
        assert_eq!(status(&state, 21), (None, Some(at(21))));
    }

    #[test]
    fn sr_207_registration_boot_offline_and_restart_fence_delayed_status_and_preserve_the_deadline()
    {
        let mut state = registered_point();
        report(&mut state, "Available", -1, 1);
        assert_eq!(status(&state, 1), (None, Some(AT)));
        report(&mut state, "Occupied", 2, 2);
        apply(
            &mut state,
            Event::Booted {
                station: "CP-1".into(),
                at: at(3),
                reason: "PowerUp".into(),
                identity: Default::default(),
            },
        );
        report(&mut state, "Available", 3, 4);
        assert_eq!(status(&state, 4), (None, Some(at(3))));
        report(&mut state, "Available", 5, 5);
        apply(
            &mut state,
            Event::Offline {
                station: "CP-1".into(),
                last_heard: at(5),
                at: at(6),
            },
        );
        report(&mut state, "Available", 7, 7);
        assert_eq!(status(&state, 7), (None, Some(at(6))));
        apply(
            &mut state,
            Event::Online {
                station: "CP-1".into(),
                at: at(8),
            },
        );
        report(&mut state, "Available", 6, 8);
        assert_eq!(status(&state, 8), (None, Some(at(6))));
        report(&mut state, "Available", 9, 10);
        apply(&mut state, Event::PublicationRestarted { at: at(11) });
        apply(&mut state, Event::PublicationRestarted { at: at(12) });
        report(&mut state, "Available", 11, 13);
        assert_eq!(status(&state, 13), (None, Some(at(11))));
        report(&mut state, "Available", 14, 14);
        assert_eq!(
            status(&state, 14),
            (Some(("Available".into(), at(14))), Some(at(14)))
        );
    }

    #[test]
    fn sr_207_a_reservation_changes_availability_without_refreshing_the_device_observation() {
        let mut state = registered_point();
        report(&mut state, "Available", 4, 5);
        apply(
            &mut state,
            Event::Reserved {
                station: "CP-1".into(),
                number: 1,
                reference: "fixture".into(),
                card: "00".into(),
                until: at(20),
                by: "fixture".into(),
                reason: "test booking".into(),
                at: at(8),
            },
        );
        assert_eq!(
            status(&state, 8),
            (Some(("Reserved".into(), at(4))), Some(at(8)))
        );
        report(&mut state, "Available", 6, 9);
        report(&mut state, "Available", 12, 13);
        assert_eq!(
            status(&state, 13),
            (Some(("Reserved".into(), at(12))), Some(at(8)))
        );
        assert_eq!(status(&state, 20), (None, Some(at(20))));
        report(&mut state, "Occupied", 19, 22);
        assert_eq!(status(&state, 22), (None, Some(at(20))));
        apply(
            &mut state,
            Event::ReservationEnded {
                station: "CP-1".into(),
                number: 1,
                how: Ended::Expired,
                at: at(27),
            },
        );
        report(&mut state, "Available", 19, 28);
        assert_eq!(status(&state, 28), (None, Some(at(20))));
        report(&mut state, "Available", 29, 29);
        assert_eq!(
            status(&state, 29),
            (Some(("Available".into(), at(29))), Some(at(29)))
        );
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
