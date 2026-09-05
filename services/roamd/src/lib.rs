//! `roamd` — the roaming node: who a record goes to, when it is late, and what
//! to do with one that arrives.
//!
//! # What it decides, and what it must not
//!
//! Both crossings are built and proven. [`emob_roam::ocpi::cdr::for_partner`]
//! carries a canonical record onto the OCPI version the registry records a peer
//! as speaking; [`emob_roam::oicp::cdr::to_oicp`] carries it onto Hubject's
//! wire, where it carries no money at all; [`emob_roam::from_ocpi`] reads
//! a partner's document back, **unpriced**. None of that is here, and none of it
//! could be: a document is what a domain crate says it is.
//!
//! What is here is the half a domain crate cannot have — a **ledger of what was
//! sent to whom, and what came back** — and the four questions that need it:
//!
//! 1. **Who is owed this record?** [`Roamd::consign`], routed out of the
//!    contract identifier's own issuer rather than a map somebody maintains.
//! 2. **May it be sent at all?** A record a partner has already accepted is
//!    sealed: `[OCPI 2.3.0 §mod_cdrs]` says *"it cannot be changed or replaced
//!    once sent to the eMSP. Changes are simply not allowed. Instead, a Credit
//!    CDR can be sent."* Re-consigning one is refused **by name**, and the
//!    correction has an order — see [`Roamd::credit`].
//! 3. **Which of them are late?** [`Roamd::unsettled`], against the window
//!    *that partner* agreed to. Not a constant: the same paragraph makes the
//!    cadence a contract between the two parties, so a node peering with a
//!    monthly settler and a same-day settler has two answers to one question.
//! 4. **What is to be done with a record that arrives?**
//!    [`Roamd::receive`] — accept, dispute, or refuse, with the duplicate and
//!    the restatement told apart because a CDR is never an upsert.
//!
//! # The verdict is three gates, in the order that makes them mean something
//!
//! [`emob_roam::preflight`] asks OCPI's own questions of the **document**;
//! [`emob_roam::from_ocpi`] converts it; [`emob_cdr::validate()`] asks this side's
//! questions of the **record**. Running them in that order is not arrangement,
//! it is the whole design of the read-back: every conversion repairs something,
//! and the pre-flight exists to find what would have been repaired.
//!
//! So the two failures are different answers rather than one error.
//! [`Verdict::Rejected`] is *"your document is wrong"* — the sender can fix it
//! and re-send. [`Verdict::Disputed`] is *"your document is fine and your claim
//! does not hold on our side"*, which is a conversation between two companies
//! and not a retry.
//!
//! # No I/O
//!
//! Nothing here opens a socket or reads a clock. [`Roamd::prepare`] returns the
//! document and the daemon sends it; [`Roamd::accepted`] records a delivery that
//! **succeeded**, with the URL the receiver assigned it — `[OCPI 2.3.0
//! §mod_cdrs]` returns one in the `Location` header precisely so the sender can
//! fetch back what it sent. A push that failed leaves the consignment pending,
//! so it turns up in [`Roamd::unsettled`] rather than being forgotten, which is
//! the whole reason recording a delivery is a separate act from attempting one.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use std::collections::BTreeMap;

use emob_cdr::{Cdr, CdrKey, CdrLedger};
use emob_core::Money;
use emob_roam::ocpi::cdr::Outbound;
use emob_roam::{
    Inbound, Partner, PartnerRegistry, Reach, RoamError, RoamingToken, SignedDataPolicy, Wire,
};

/// Where a record has got to on its way to the party that will pay it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Queued and not yet answered — never sent, or sent and unacknowledged.
    Pending,
    /// The receiver took it.
    ///
    /// `location` is the URL it returned, which `[OCPI 2.3.0 §mod_cdrs]` makes
    /// mandatory on the response precisely so the sender can `GET` back what it
    /// sent. Optional here because the OICP leg has no equivalent.
    Accepted {
        /// When it was taken.
        at: time::OffsetDateTime,
        /// Where it now lives in the receiver's system.
        location: Option<String>,
    },
    /// The receiver refused it, and said why.
    ///
    /// A refused record may be corrected and consigned again. An **accepted**
    /// one may not: see [`Roamd::consign`].
    Refused {
        /// When.
        at: time::OffsetDateTime,
        /// What the receiver said.
        reason: String,
    },
}

impl Delivery {
    /// Whether the receiver has taken this record.
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }
}

/// One record on its way to one partner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consignment {
    /// Which record.
    pub key: CdrKey,
    /// Who it is addressed to, and whether a hub decides the rest.
    pub reach: Reach,
    /// Which wire it goes out on — a fact about the partner, not about the
    /// record.
    pub wire: Wire,
    /// When the session it records ended, which is when the settlement window
    /// starts running.
    pub ended_at: time::OffsetDateTime,
    /// Where it has got to.
    pub delivery: Delivery,
    /// The record this one supersedes, for the replacement half of a
    /// correction.
    pub supersedes: Option<CdrKey>,
    /// Whether this consignment is the **Credit CDR** that reverses another.
    pub credits: Option<CdrKey>,
}

impl Consignment {
    /// The party the document is actually sent to.
    #[must_use]
    pub const fn recipient(&self) -> &emob_core::PartyId {
        self.reach.recipient()
    }
}

/// Why a record could not be consigned to anybody.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DispatchError {
    /// The record names no contract this registry can route.
    #[error(transparent)]
    Roam(#[from] RoamError),

    /// A gate between a record and a price refused it.
    #[error(transparent)]
    Cdr(#[from] emob_cdr::CdrError),

    /// A record already accepted by its partner was consigned again.
    ///
    /// `[OCPI 2.3.0 §mod_cdrs]`: *"Because a CDR is for billing purposes, it
    /// cannot be changed or replaced once sent to the eMSP. Changes are simply
    /// not allowed. Instead, a Credit CDR can be sent."*
    ///
    /// This is the one rule in this service that is about a **ledger of what was
    /// sent** rather than about a document, which is why no crate can hold it:
    /// `emob_roam::ocpi::cdr::to_ocpi_credit` builds the reversal and
    /// `emob_cdr` refuses to bill both halves, and neither of them knows whether
    /// the original ever left the building.
    #[error(
        "{key} was accepted by {recipient} at {at} and a CDR cannot be changed or replaced once \
         sent [OCPI 2.3.0 §mod_cdrs]: send the Credit CDR that reverses it, then the replacement"
    )]
    AlreadyAccepted {
        /// Which record.
        key: String,
        /// Who took it.
        recipient: String,
        /// When.
        at: time::OffsetDateTime,
    },

    /// A replacement was consigned before the credit that reverses what it
    /// replaces was accepted.
    ///
    /// The order is the specification's: *"the CPO has to send a Credit CDR for
    /// the first CDR … **After having sent the Credit CDR**, the CPO can send a
    /// new CDR with a new unique ID"*. Sent the other way round, the partner
    /// holds two records for one session and settles both until somebody
    /// notices.
    #[error(
        "{key} supersedes {superseded}, and no Credit CDR reversing {superseded} has been accepted \
         yet: [OCPI 2.3.0 §mod_cdrs] sends the reversal first, or the partner holds two records \
         for one session"
    )]
    CreditNotAcceptedYet {
        /// The replacement.
        key: String,
        /// What it replaces.
        superseded: String,
    },

    /// The consignment is owed on the other wire.
    ///
    /// Which protocol a partner is reached on is a field rather than an
    /// inference — Hubject is a hub **and** speaks OICP, GIREVE is a hub and
    /// speaks OCPI — so a caller reaching for the wrong builder is refused here
    /// rather than sending a document the receiver parses none of.
    #[error("{recipient} is reached over {expected} and this is the {offered} document")]
    WrongWire {
        /// Who.
        recipient: String,
        /// The wire the registry records.
        expected: Wire,
        /// The one the caller reached for.
        offered: Wire,
    },

    /// The contract identifier is in a scheme this build cannot read a provider
    /// out of, and no partner claims it explicitly.
    ///
    /// A **different** operational problem from
    /// [`RoamError::NoRoute`], which the registry
    /// cannot tell apart on its own because both arrive as "no route". There the
    /// contract names a provider and nobody peers with it — a partner is
    /// missing. Here the identifier is in an eMSP's own scheme, which
    /// `[OCPI 2.3.0 §mod_tokens]` permits, and the registry needs the explicit
    /// `Partner::issuing` entry that says which namespace it covers. One message
    /// for the two would send an operator looking for the wrong thing.
    #[error(
        "no provider can be read out of the contract {contract} and no partner claims it: an \
         eMSP may issue under its own scheme, and the registry needs a `Partner::issuing` \
         entry naming the namespace rather than a route by prefix"
    )]
    UnroutableContract {
        /// The identifier, as it arrived.
        contract: String,
    },

    /// A re-rating produced a record with no price on it.
    ///
    /// Unreachable through [`Roamd::settle`], which prices through
    /// [`Cdr::rerated_with`] — the one door a `Cost` is made through, and it
    /// always makes one. Stated as a refusal rather than defaulted to zero
    /// because the default would be a **number**: `[OCPI 2.3.0 §mod_cdrs]` gives
    /// the obvious placeholder its own meaning, *"0.00 means free of charge"*,
    /// and a settlement that quietly says a session was free is the answer
    /// nobody queries.
    #[error("re-rating {key} produced no price, and a price of zero is a statement")]
    NotRated {
        /// Which record.
        key: String,
    },

    /// The record was never consigned, so there is nothing to prepare or record
    /// a delivery against.
    #[error("{key} was never consigned to anybody")]
    NotConsigned {
        /// Which record.
        key: String,
    },
}

/// A record whose partner agreed to have it by now and does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsettled {
    /// Which record.
    pub key: CdrKey,
    /// Who is owed it.
    pub recipient: emob_core::PartyId,
    /// The window that partner agreed to.
    pub agreed: time::Duration,
    /// How far past it this record is.
    pub overdue_by: time::Duration,
    /// Whether it was refused, and what the receiver said.
    pub refused: Option<String>,
}

impl core::fmt::Display for Unsettled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} has not been accepted by {} and their agreed window of {} hours ran out {} hours \
             ago",
            self.key,
            self.recipient,
            self.agreed.whole_hours(),
            self.overdue_by.whole_hours(),
        )?;
        match &self.refused {
            Some(reason) => write!(
                f,
                ": they refused it — {reason}. A refused record may be corrected and sent again"
            ),
            None => write!(
                f,
                ". `[OCPI 2.3.0 §mod_cdrs]` leaves the cadence to the agreement between the two \
                 parties, and this one is past it: the session is delivered, settled on this side, \
                 and unbilled"
            ),
        }
    }
}

/// What is to be done with a record a partner sent.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Verdict {
    /// The document holds up and the record is new. Settle it.
    Accepted(Box<Inbound>),

    /// The document holds up and the **record** does not, on this side's rules.
    ///
    /// Not a retry. The sender's document is well formed and its claim is one
    /// this side cannot settle — signed records that do not verify here, a
    /// price computed for a quantity the record does not state, minutes charged
    /// that its own periods do not account for. That is a conversation between
    /// two companies, and it is a different answer from
    /// [`Self::Rejected`] for exactly that reason.
    Disputed {
        /// Which record.
        key: CdrKey,
        /// Everything that does not hold, at once.
        reasons: Vec<String>,
    },

    /// A record with this key is already held, unchanged.
    ///
    /// Idempotent: a partner that re-sends after a timeout has not created a
    /// second session, and a receiver that treated the retry as one would bill
    /// the driver twice.
    Duplicate {
        /// Which record.
        key: CdrKey,
    },

    /// A record with this key is already held and this one says something
    /// different.
    ///
    /// **Never an upsert.** `[OCPI 2.3.0 §mod_cdrs]` does not permit a CDR to be
    /// changed or replaced, so a restatement under a held id is a partner doing
    /// something the protocol forbids, and the record already held is the one
    /// that stands. This is the event a human answers.
    Conflicted {
        /// Which record.
        key: CdrKey,
    },

    /// The document does not answer OCPI's own questions about itself.
    ///
    /// The sender can fix it and send it again.
    Rejected {
        /// Everything wrong with it, at once — a partner integration is
        /// debugged by seeing all of it in one pass.
        reasons: Vec<String>,
    },
}

/// What a settled record is worth to each of the two parties.
///
/// # Two numbers, and the difference is the business
///
/// An eMSP owes the **CPO** what the CPO's own document states — that is the
/// claim it accepted — and owes its **driver** its own retail price, which has
/// nothing to do with the CPO's tariff. `emob_roam::from_ocpi` lands the record
/// unpriced for exactly this reason: rebuilding the CPO's price from totals with
/// no unit prices would make this side's validator check its own arithmetic.
///
/// So the pair is stated rather than reconciled, and neither is derived from the
/// other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// Which record.
    pub key: CdrKey,
    /// What the partner's own document says the session came to.
    pub owed_to_partner: Money,
    /// What this side's retail tariff makes of the same periods, gross.
    pub owed_by_driver: Money,
}

impl Settlement {
    /// What is left between the two, in the record's own currency.
    ///
    /// Negative where the retail price is below what the partner charged, which
    /// is a real and ordinary outcome — a capped consumer tariff over an
    /// expensive ad-hoc session — and not an error.
    ///
    /// # Errors
    ///
    /// [`emob_core::QuantityError`] where the two are not in one currency, which
    /// is a record this side re-rated in a currency the partner did not use.
    pub fn margin(&self) -> Result<Money, emob_core::QuantityError> {
        self.owed_by_driver.checked_sub(self.owed_to_partner)
    }
}

/// The roaming node: who this peers with, what has gone out, and what has come
/// in.
#[derive(Debug)]
pub struct Roamd {
    registry: PartnerRegistry,
    /// One entry per record this node owes a partner, keyed the way a CDR is
    /// keyed: a party and an id, because OCPI makes an id unique per party and
    /// two CPOs may each have a CDR `1`.
    outbound: BTreeMap<CdrKey, Consignment>,
    /// The records partners have sent, for the idempotency a receiver owes.
    inbound: CdrLedger,
    /// What every partner's document is asked about its signed metering data.
    signed_data: SignedDataPolicy,
}

impl Roamd {
    /// A node that peers with a registry and has sent and received nothing.
    #[must_use]
    pub fn new(registry: PartnerRegistry) -> Self {
        Self {
            registry,
            outbound: BTreeMap::new(),
            inbound: CdrLedger::new(),
            signed_data: SignedDataPolicy::default(),
        }
    }

    /// Require every inbound document to carry signed metering data.
    ///
    /// A node settling German sessions does `[MessEG §33]`; one peering into
    /// jurisdictions that do not ask should not refuse a lawful record over it.
    #[must_use]
    pub const fn requiring_signed_data(mut self) -> Self {
        self.signed_data = SignedDataPolicy::Required;
        self
    }

    /// Who this node peers with.
    #[must_use]
    pub const fn registry(&self) -> &PartnerRegistry {
        &self.registry
    }

    /// The records this node has taken from partners.
    #[must_use]
    pub const fn received(&self) -> &CdrLedger {
        &self.inbound
    }

    /// Address a record to the party that will pay it.
    ///
    /// Routed out of the **contract identifier's own issuer**, so a record goes
    /// to the provider the driver actually holds a contract with rather than to
    /// whoever a map says. A direct peer claiming that namespace wins; failing
    /// that, a hub; failing that, this is [`RoamError::NoRoute`] — a record sent
    /// to a party that never had the driver is settlement money leaving for the
    /// wrong company.
    ///
    /// # Errors
    ///
    /// [`RoamError::NoRoute`] where nobody claims the contract's namespace,
    /// [`DispatchError::UnroutableContract`] where no provider can be read out
    /// of the identifier at all, [`DispatchError::AlreadyAccepted`] for a record
    /// its partner has already taken — which is sealed
    /// `[OCPI 2.3.0 §mod_cdrs]` — and [`DispatchError::CreditNotAcceptedYet`]
    /// for a replacement sent before the reversal it depends on.
    ///
    /// # Consigning again keeps what happened
    ///
    /// A record the partner **refused** may be corrected and consigned again —
    /// nothing was settled, so nothing is sealed — and the consignment it
    /// already has is the one that stands, refusal and all. That is deliberate:
    /// the refusal is what [`Self::unsettled`] reports beside the overdue
    /// window, and an operator reading *"past its window, and they said the
    /// total does not match the periods"* is reading one sentence rather than
    /// two records.
    pub fn consign(
        &mut self,
        cdr: &Cdr,
        token: &RoamingToken,
    ) -> Result<&Consignment, DispatchError> {
        if let Some(held) = self.outbound.get(&cdr.key)
            && let Delivery::Accepted { at, .. } = held.delivery
        {
            return Err(DispatchError::AlreadyAccepted {
                key: cdr.key.to_string(),
                recipient: held.recipient().to_string(),
                at,
            });
        }

        // The replacement half of a correction may not overtake its own
        // reversal. `emob_cdr` refuses to bill both halves and `to_ocpi_credit`
        // builds the reversal; neither knows whether it left the building.
        if let Some(superseded) = &cdr.supersedes
            && !self.credit_accepted_for(superseded)
        {
            return Err(DispatchError::CreditNotAcceptedYet {
                key: cdr.key.to_string(),
                superseded: superseded.to_string(),
            });
        }

        // Two causes, two messages. `route` answers `None` both for a contract
        // whose provider nobody peers with and for one in a scheme this build
        // reads no provider out of at all, and an operator told "no partner
        // routes a contract issued by …" for the second goes looking for a
        // partner that is not the problem (D268).
        let Some(reach) = self.registry.route(&token.contract_id) else {
            return Err(match emob_roam::partner::issuer_of(&token.contract_id) {
                Some(issuer) => RoamError::NoRoute {
                    issuer: issuer.to_string(),
                }
                .into(),
                None => DispatchError::UnroutableContract {
                    contract: token.contract_id.to_string(),
                },
            });
        };
        let wire = self
            .registry
            .get(reach.recipient())
            .map_or(Wire::Ocpi, |partner| partner.wire);

        let consignment = Consignment {
            key: cdr.key.clone(),
            reach,
            wire,
            ended_at: cdr.ended_at,
            delivery: Delivery::Pending,
            supersedes: cdr.supersedes.clone(),
            credits: None,
        };
        Ok(self.outbound.entry(cdr.key.clone()).or_insert(consignment))
    }

    /// Address the **Credit CDR** that reverses a record already sent.
    ///
    /// `credit_key` is the reversal's own id, which the specification wants
    /// distinct from the original's — *"the id of the original CDR with
    /// something appended like for example `-C`"*.
    ///
    /// # Errors
    ///
    /// [`DispatchError::NotConsigned`] where the record being reversed was never
    /// sent to anybody, and [`RoamError::NoRoute`] where the reversal cannot be
    /// routed.
    pub fn credit(
        &mut self,
        original: &CdrKey,
        credit_key: CdrKey,
        ended_at: time::OffsetDateTime,
    ) -> Result<&Consignment, DispatchError> {
        let Some(sent) = self.outbound.get(original) else {
            return Err(DispatchError::NotConsigned {
                key: original.to_string(),
            });
        };
        let consignment = Consignment {
            key: credit_key.clone(),
            reach: sent.reach.clone(),
            wire: sent.wire,
            ended_at,
            delivery: Delivery::Pending,
            supersedes: None,
            credits: Some(original.clone()),
        };
        Ok(self.outbound.entry(credit_key).or_insert(consignment))
    }

    /// Build the OCPI document for a consignment, in the version its partner
    /// speaks.
    ///
    /// # Errors
    ///
    /// [`DispatchError::NotConsigned`], [`DispatchError::WrongWire`] for a
    /// partner reached over OICP, and everything
    /// [`emob_roam::ocpi::cdr::for_partner`] refuses.
    pub fn prepare(
        &self,
        key: &CdrKey,
        cdr: &Cdr,
        context: &emob_roam::ocpi::cdr::Context<'_>,
    ) -> Result<emob_roam::Crossing<Outbound>, DispatchError> {
        let partner = self.partner_for(key, Wire::Ocpi)?;
        Ok(emob_roam::ocpi::cdr::for_partner(cdr, partner, context)?)
    }

    /// Build the OICP document for a consignment reached through a broker.
    ///
    /// A separate entry point because the wire needs what OCPI does not: the
    /// session identifier **the broker** issued when it authorised the session.
    /// A record carrying an id Hubject never issued is refused with
    /// `SessionIsInvalid`, after the driver has gone.
    ///
    /// # Errors
    ///
    /// [`DispatchError::NotConsigned`], [`DispatchError::WrongWire`] for a
    /// partner reached over OCPI, and everything
    /// [`emob_roam::oicp::cdr::to_oicp`] refuses.
    pub fn prepare_for_broker(
        &self,
        key: &CdrKey,
        cdr: &Cdr,
        context: &emob_roam::oicp::cdr::Context<'_>,
    ) -> Result<emob_roam::Crossing<oicp_kit::cpo::ChargeDetailRecord>, DispatchError> {
        let partner = self.partner_for(key, Wire::Oicp)?;
        Ok(emob_roam::oicp::cdr::to_oicp(cdr, partner, context)?)
    }

    /// Record that a receiver **took** a record.
    ///
    /// Called after the push, never before it. `location` is the URL the
    /// receiver returned, which `[OCPI 2.3.0 §mod_cdrs]` makes mandatory on the
    /// response so that the sender can fetch back what it sent.
    ///
    /// # Errors
    ///
    /// [`DispatchError::NotConsigned`] for a record nobody addressed.
    pub fn accepted(
        &mut self,
        key: &CdrKey,
        at: time::OffsetDateTime,
        location: Option<String>,
    ) -> Result<(), DispatchError> {
        self.delivered(key, Delivery::Accepted { at, location })
    }

    /// Record that a receiver **refused** a record, and what it said.
    ///
    /// # Errors
    ///
    /// [`DispatchError::NotConsigned`] for a record nobody addressed.
    pub fn refused(
        &mut self,
        key: &CdrKey,
        at: time::OffsetDateTime,
        reason: impl Into<String>,
    ) -> Result<(), DispatchError> {
        self.delivered(
            key,
            Delivery::Refused {
                at,
                reason: reason.into(),
            },
        )
    }

    /// Every consignment still waiting to be sent, in the order they were keyed.
    ///
    /// **Not** the same as "not accepted". A record the receiver *refused* is
    /// not waiting for a socket, it is waiting for somebody to decide what was
    /// wrong with it — and a drain loop that re-sent it every tick would push
    /// the same rejected document at a partner until the queue was emptied by
    /// hand. It is still overdue, and [`Self::unsettled`] is where it says so.
    pub fn pending(&self) -> impl Iterator<Item = &Consignment> {
        self.outbound
            .values()
            .filter(|consignment| matches!(consignment.delivery, Delivery::Pending))
    }

    /// What one record's journey looks like.
    #[must_use]
    pub fn consignment(&self, key: &CdrKey) -> Option<&Consignment> {
        self.outbound.get(key)
    }

    /// The records whose partner agreed to have them by now and does not.
    ///
    /// The sharp question, and a different one from [`Self::pending`]: that is
    /// what a socket is owed, this is a session that has been delivered, settled
    /// on this side and never billed to anybody. A record the partner **refused**
    /// is in this list and not in that one — it is overdue and it is not waiting
    /// for a retry. Judged against **each partner's
    /// own** agreed window, because `[OCPI 2.3.0 §mod_cdrs]` makes the cadence
    /// an agreement between the two parties rather than a rule of the protocol.
    #[must_use]
    pub fn unsettled(&self, now: time::OffsetDateTime) -> Vec<Unsettled> {
        self.outbound
            .values()
            .filter(|consignment| !consignment.delivery.is_accepted())
            .filter_map(|consignment| {
                let agreed = self
                    .registry
                    .get(consignment.recipient())
                    .map_or(emob_roam::DEFAULT_SETTLEMENT_WINDOW, |p| p.settles_within);
                let age = now - consignment.ended_at;
                (age > agreed).then(|| Unsettled {
                    key: consignment.key.clone(),
                    recipient: consignment.recipient().clone(),
                    agreed,
                    overdue_by: age - agreed,
                    refused: match &consignment.delivery {
                        Delivery::Refused { reason, .. } => Some(reason.clone()),
                        Delivery::Pending | Delivery::Accepted { .. } => None,
                    },
                })
            })
            .collect()
    }

    /// Decide what to do with a record a partner sent.
    ///
    /// Three gates in the order that makes them mean something — the document,
    /// the conversion, the record — and four answers rather than one error. See
    /// the module documentation.
    ///
    /// `evidence` is the caller's because producing it means **verifying** the
    /// signed records against this node's own key registry, which is
    /// `emob-eichrecht`'s job and needs a registry this service has no business
    /// holding. `emob_roam::ocpi::cdr::inbound_payloads` hands over what to
    /// verify.
    #[must_use]
    pub fn receive(
        &mut self,
        document: &ocpi_kit::v2_3_0::Cdr,
        evidence: Option<emob_cdr::EvidenceRef>,
    ) -> Verdict {
        // 1. The document, asked OCPI's own questions — **before** any
        //    conversion, because every conversion repairs something.
        let report = emob_roam::preflight(document, self.signed_data);
        if !report.is_settleable() {
            return Verdict::Rejected {
                reasons: report.blocking().map(ToString::to_string).collect(),
            };
        }

        // 2. The conversion, which refuses rather than repairs.
        let inbound = match emob_roam::from_ocpi(document, evidence) {
            Ok(crossing) => crossing.into_value_discarding_notes(),
            Err(error) => {
                return Verdict::Rejected {
                    reasons: vec![error.to_string()],
                };
            }
        };

        // 3. This side's questions about the **record**. A document that is
        //    well formed and a claim that does not hold are different answers.
        let verdict = emob_cdr::validate(&inbound.cdr);
        if !verdict.is_settleable() {
            return Verdict::Disputed {
                key: inbound.cdr.key.clone(),
                reasons: verdict.blocking().map(ToString::to_string).collect(),
            };
        }

        // …and only then the ledger, which is what makes a retry idempotent and
        // a restatement a conflict rather than an overwrite.
        let key = inbound.cdr.key.clone();
        match self.inbound.accept(inbound.cdr.clone()) {
            emob_cdr::Acceptance::Stored => Verdict::Accepted(Box::new(inbound)),
            emob_cdr::Acceptance::Duplicate => Verdict::Duplicate { key },
            // …and everything else. `Acceptance` is `#[non_exhaustive]`, so an
            // outcome a later release adds arrives here — and it must not be
            // read as "stored", because a receiver that guessed would settle a
            // session it never took. A conflict is the answer a human already
            // reads, which is the right place for one nobody has classified.
            _ => Verdict::Conflicted { key },
        }
    }

    /// What an accepted record is worth to each of the two parties.
    ///
    /// Re-rated through [`Cdr::rerated_with`] — the same door the issuing side
    /// prices with — because reaching for the rating engine directly silently
    /// skips every gate the issuer applied: a retail tariff that was not in
    /// force when the session ran, a version the meter says was superseded
    /// mid-session, a duration the signed records do not vouch for, and the
    /// clock resolution `[REA 6-A §3.1]` puts under a per-minute fee.
    ///
    /// # Errors
    ///
    /// Every gate that door applies, and [`DispatchError::NotRated`] for the
    /// price it cannot produce — which it always can, and which is a refusal
    /// rather than a zero because a zero is a statement.
    pub fn settle(
        &self,
        inbound: &Inbound,
        retail: &emob_tariff::Tariff,
    ) -> Result<Settlement, DispatchError> {
        let ours = inbound
            .cdr
            .rerated_with(retail)
            .map_err(DispatchError::Cdr)?;
        let Some(owed_by_driver) = ours.total_cost() else {
            return Err(DispatchError::NotRated {
                key: inbound.cdr.key.to_string(),
            });
        };
        Ok(Settlement {
            key: inbound.cdr.key.clone(),
            owed_to_partner: inbound.stated_total,
            owed_by_driver,
        })
    }

    /// The partner a consignment is addressed to, on the wire it is owed on.
    fn partner_for(&self, key: &CdrKey, offered: Wire) -> Result<&Partner, DispatchError> {
        let consignment = self
            .outbound
            .get(key)
            .ok_or_else(|| DispatchError::NotConsigned {
                key: key.to_string(),
            })?;
        if consignment.wire != offered {
            return Err(DispatchError::WrongWire {
                recipient: consignment.recipient().to_string(),
                expected: consignment.wire,
                offered,
            });
        }
        self.registry
            .get(consignment.recipient())
            .ok_or_else(|| DispatchError::NotConsigned {
                key: key.to_string(),
            })
    }

    /// Record one outcome against a consignment.
    fn delivered(&mut self, key: &CdrKey, delivery: Delivery) -> Result<(), DispatchError> {
        let Some(consignment) = self.outbound.get_mut(key) else {
            return Err(DispatchError::NotConsigned {
                key: key.to_string(),
            });
        };
        consignment.delivery = delivery;
        Ok(())
    }

    /// Whether a reversal of this record has been accepted by its partner.
    fn credit_accepted_for(&self, original: &CdrKey) -> bool {
        self.outbound.values().any(|consignment| {
            consignment.credits.as_ref() == Some(original) && consignment.delivery.is_accepted()
        })
    }
}
