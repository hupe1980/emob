//! The consignment ledger, kept where it outlives the process that wrote it.
//!
//! `[OCPI 2.3.0 §mod_cdrs]`: *"Because a CDR is for billing purposes, it cannot
//! be changed or replaced once sent to the eMSP."* [`Roamd::consign`] enforces
//! that by reading a delivery this node recorded — so the seal is exactly as
//! durable as the place the answer is kept. An empty ledger answers *"nobody
//! has this"* to every question, which re-opens every record already sent.
//!
//! The same holds for the two facts either side of it: a replacement may not
//! overtake the Credit CDR it depends on, and an inbound record arriving twice
//! is one record.
//!
//! # What is journalled
//!
//! The **ledger** and the **outbox**, not the registry and not the policies. A
//! partner registry and an operator's Cedar set are configuration: they are
//! meant to change, and a node that replayed an old registry would route
//! today's records by yesterday's contracts. What is a fact is *what this node
//! did* — which record went to whom, over which wire, as which document, and
//! what came back.

use emob_cdr::{Cdr, CdrKey};
use std::sync::Arc;

use emob_service::Actor;
use emob_service::journal::{Folded, Journal, JournalError, Kinded};
use emob_service::vault::{DataKey, Digest, KeyId};

use crate::{Answer, Consignment, Delivery, DispatchError, Roamd, Verdict};

/// One fact this node decided.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// A record was addressed to a partner, and the document it will go as.
    Consigned {
        /// Who it went to, over what, and where it had got to when it was
        /// written down.
        consignment: Box<Consignment>,
        /// The document, rendered once at this moment. See `Roamd`'s outbox:
        /// the account of what the crossing cost points **into this copy**, so
        /// a re-rendering at send time would describe a document nobody
        /// received.
        document: Option<Box<emob_roam::ocpi::cdr::Outbound>>,
    },
    /// A receiver answered.
    Delivered {
        /// Which record.
        key: CdrKey,
        /// What the receiver said.
        delivery: Delivery,
    },
    /// A bootstrap token was handed to a platform out of band.
    ///
    /// `CREDENTIALS_TOKEN_A`, which reaches `versions` and `credentials` and
    /// nothing else. Journalled because a node that forgot it cannot complete a
    /// handshake somebody has already been invited to — as a keyed digest,
    /// because a journal copy must not be the invitation (SR-044).
    Offered {
        /// The platform it was handed to.
        peer_id: String,
        /// The token's digest.
        token: Digest,
    },
    /// A platform completed the credentials handshake.
    Registered {
        /// What this node now holds for it.
        registration: Box<crate::peering::Registration>,
    },
    /// A platform unregistered, or was unregistered.
    Unregistered {
        /// Which platform.
        peer_id: String,
    },
    /// A partner's record was accepted into this node's inbound ledger.
    Received {
        /// The record, as this side re-built it.
        cdr: Box<Cdr>,
        /// …and the document it arrived as, kept so a sender can `GET` back
        /// what it sent rather than a rebuild of it.
        document: Option<Box<ocpi_kit::v2_3_0::Cdr>>,
    },
}

impl Kinded for Event {
    fn kind(&self) -> &'static str {
        match self {
            Self::Consigned { .. } => "consigned",
            Self::Delivered { .. } => "delivered",
            Self::Offered { .. } => "offered",
            Self::Registered { .. } => "registered",
            Self::Unregistered { .. } => "unregistered",
            Self::Received { .. } => "received",
        }
    }
}

/// What went wrong that was not about a record.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeError {
    /// The dispatch itself was refused.
    #[error(transparent)]
    Dispatch(Box<DispatchError>),

    /// The journal could not take the record.
    #[error("the journal refused the record for {key}: {source}")]
    Journal {
        /// Which record the node had just decided on.
        key: String,
        /// Why.
        source: JournalError,
    },

    /// The node has stopped, because its ledger is ahead of its journal.
    ///
    /// The seal `[OCPI 2.3.0 §mod_cdrs]` puts on a sent record is the reason
    /// this is fatal rather than retryable: a node whose journal is missing an
    /// acceptance will, after the next restart, send again a record the partner
    /// has already billed.
    #[error(
        "roamd has halted: {why} — the node recorded a delivery its journal could not take, so \
         the seal a CDR carries once it has been sent would not survive a restart. Restarting \
         rebuilds from the journal, which is the only ledger this node can account for"
    )]
    Halted {
        /// What the journal said when it refused.
        why: String,
    },

    /// The journal does not fold.
    #[error(transparent)]
    Fold(#[from] JournalError),

    /// The journal holds tokens under another data key.
    #[error(
        "roamd's journal holds credentials under data key {found}, and ROAMD_DATA_KEY is {held}: \
         with another key no partner's token would authenticate and no record could be pushed, \
         so roamd does not start — restore the key the journal was written under"
    )]
    ForeignKey {
        /// The key id the journal names.
        found: KeyId,
        /// The key id configured.
        held: KeyId,
    },
}

impl From<DispatchError> for NodeError {
    fn from(source: DispatchError) -> Self {
        Self::Dispatch(Box::new(source))
    }
}

/// [`Roamd`], folded from its journal and extended by appending to it.
pub struct Node<J: Journal> {
    folded: Folded<Roamd, Event, J>,
    halted: Option<String>,
    key: Arc<DataKey>,
}

impl<J: Journal> core::fmt::Debug for Node<J> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Node")
            .field("halted", &self.halted)
            .finish_non_exhaustive()
    }
}

impl<J: Journal> Node<J> {
    /// Rebuild the node from its journal, around a registry loaded fresh,
    /// under the data key its tokens are digested and sealed with.
    ///
    /// # Errors
    ///
    /// [`NodeError::Fold`] when the journal does not chain or holds a fact this
    /// build cannot read, and [`NodeError::ForeignKey`] when it was written
    /// under another key.
    pub fn open(empty: Roamd, journal: J, key: impl Into<Arc<DataKey>>) -> Result<Self, NodeError> {
        let key = key.into();
        let folded = Folded::open(empty, journal, Roamd::apply)?;
        if let Some(found) = folded.state().peers().foreign_key(&key) {
            return Err(NodeError::ForeignKey {
                found,
                held: key.id().clone(),
            });
        }
        Ok(Self {
            folded,
            halted: None,
            key,
        })
    }

    /// The data key tokens are digested and sealed under.
    #[must_use]
    pub fn key(&self) -> Arc<DataKey> {
        Arc::clone(&self.key)
    }

    /// The folded node, for every question that does not change it.
    #[must_use]
    pub const fn state(&self) -> &Roamd {
        self.folded.state()
    }

    /// The journal under it.
    #[must_use]
    pub const fn journal(&self) -> &J {
        self.folded.journal()
    }

    /// Whether the node has stopped deciding.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.halted.is_some()
    }

    fn guard(&self) -> Result<(), NodeError> {
        match &self.halted {
            None => Ok(()),
            Some(why) => Err(NodeError::Halted { why: why.clone() }),
        }
    }

    fn commit(&mut self, by: &Actor, event: &Event, key: &str) -> Result<(), NodeError> {
        match self.folded.note(by, event) {
            Ok(_) => Ok(()),
            Err(source) => {
                self.halted = Some(source.to_string());
                Err(NodeError::Journal {
                    key: key.to_owned(),
                    source,
                })
            }
        }
    }

    /// Address a record to the partner its contract identifier names.
    ///
    /// # Errors
    ///
    /// [`NodeError::Dispatch`] for every reason [`Roamd::consign`] gives —
    /// including the seal on a record the partner has already taken — and
    /// [`NodeError::Journal`] where the decision could not be recorded.
    pub fn consign(
        &mut self,
        by: &Actor,
        cdr: &Cdr,
        token: &emob_roam::RoamingToken,
        context: Option<&emob_roam::ocpi::cdr::Context<'_>>,
    ) -> Result<CdrKey, NodeError> {
        self.guard()?;
        let consignment = self.folded.state_mut().consign(cdr, token)?.clone();
        let key = consignment.key.clone();

        // Rendered **after** the routing and **before** the record of it, so
        // the journal holds the bytes that will go out. A record whose document
        // cannot be built here is routed and carries none, which the drain loop
        // reports by name rather than sending something it re-derived.
        let document = match context {
            None => None,
            Some(context) => {
                let rendered = self
                    .folded
                    .state()
                    .prepare(&key, cdr, context)
                    .map_err(NodeError::from)?
                    .value;
                // Into the outbox now as well as into the record: this type
                // decides and mutates in one call and *then* journals, so a
                // fact that only `apply` wrote would exist on the way up and
                // not on the way through — the state and its journal agreeing
                // only after a restart, which is the one disagreement nothing
                // would notice.
                self.folded
                    .state_mut()
                    .hold_document(&key, rendered.clone());
                Some(Box::new(rendered))
            }
        };
        self.commit(
            by,
            &Event::Consigned {
                consignment: Box::new(consignment),
                document,
            },
            &key.to_string(),
        )?;
        Ok(key)
    }

    /// Record that a receiver took a record — the acceptance that seals it.
    ///
    /// # Errors
    ///
    /// [`NodeError::Dispatch`] for a record nobody addressed, and
    /// [`NodeError::Journal`] where the acceptance could not be recorded.
    pub fn accepted(
        &mut self,
        by: &Actor,
        key: &CdrKey,
        at: time::OffsetDateTime,
        location: Option<String>,
    ) -> Result<(), NodeError> {
        self.guard()?;
        self.folded
            .state_mut()
            .accepted(key, at, location.clone())?;
        self.commit(
            by,
            &Event::Delivered {
                key: key.clone(),
                delivery: Delivery::Accepted { at, location },
            },
            &key.to_string(),
        )
    }

    /// Record that a receiver refused one, and what it said.
    ///
    /// # Errors
    ///
    /// [`NodeError::Dispatch`] for a record nobody addressed or one already
    /// accepted, and [`NodeError::Journal`] otherwise.
    pub fn refused(
        &mut self,
        by: &Actor,
        key: &CdrKey,
        at: time::OffsetDateTime,
        reason: impl Into<String>,
    ) -> Result<(), NodeError> {
        self.guard()?;
        let reason = reason.into();
        self.folded.state_mut().refused(key, at, reason.clone())?;
        self.commit(
            by,
            &Event::Delivered {
                key: key.clone(),
                delivery: Delivery::Refused { at, reason },
            },
            &key.to_string(),
        )
    }

    /// Hand a bootstrap token to a platform, out of band.
    ///
    /// # Errors
    ///
    /// [`NodeError::Journal`] where the offer could not be recorded.
    pub fn offer(&mut self, by: &Actor, peer_id: &str, token: &str) -> Result<(), NodeError> {
        self.guard()?;
        let digest = self.key.digest(crate::peering::INCOMING, token.as_bytes());
        self.folded
            .state_mut()
            .peers_mut()
            .offer(peer_id, digest.clone());
        self.commit(
            by,
            &Event::Offered {
                peer_id: peer_id.to_owned(),
                token: digest,
            },
            peer_id,
        )
    }

    /// Record a completed handshake.
    ///
    /// # Errors
    ///
    /// [`NodeError::Journal`] where the registration could not be recorded —
    /// after which the node halts, because a token it has issued and cannot
    /// reproduce is one a partner will present to a `401`.
    pub fn register(
        &mut self,
        by: &Actor,
        registration: crate::peering::Registration,
    ) -> Result<(), NodeError> {
        self.guard()?;
        let peer_id = registration.peer_id.clone();
        self.folded
            .state_mut()
            .peers_mut()
            .register(registration.clone());
        self.commit(
            by,
            &Event::Registered {
                registration: Box::new(registration),
            },
            &peer_id,
        )
    }

    /// Forget a platform.
    ///
    /// # Errors
    ///
    /// [`NodeError::Journal`] where the removal could not be recorded.
    pub fn unregister(&mut self, by: &Actor, peer_id: &str) -> Result<(), NodeError> {
        self.guard()?;
        self.folded.state_mut().peers_mut().unregister(peer_id);
        self.commit(
            by,
            &Event::Unregistered {
                peer_id: peer_id.to_owned(),
            },
            peer_id,
        )
    }

    /// Record what a partner said about a record this node sent.
    ///
    /// The **order** this type exists for: the answer comes back from the wire
    /// first, and only then does it reach the ledger. A node that recorded a
    /// delivery before the `POST` would seal — `[OCPI 2.3.0 §mod_cdrs]` — a
    /// record nobody has.
    ///
    /// [`Answer::Unreached`] records **nothing**, deliberately. A connection
    /// that timed out is not a verdict: the partner may hold the record and may
    /// not, and the only safe reading is that this node still owes it. Writing
    /// that down as a refusal would take a record out of the queue that nobody
    /// ever saw, and writing it down as an acceptance would seal one that was
    /// never delivered.
    ///
    /// # Errors
    ///
    /// [`NodeError::Dispatch`] for a record nobody addressed, and
    /// [`NodeError::Journal`] where the answer could not be recorded.
    pub fn answered(
        &mut self,
        by: &Actor,
        key: &CdrKey,
        answer: &Answer,
        at: time::OffsetDateTime,
    ) -> Result<(), NodeError> {
        match answer {
            Answer::Accepted { location } => self.accepted(by, key, at, location.clone()),
            Answer::Refused { reason } => self.refused(by, key, at, reason.clone()),
            Answer::Unreached { .. } => Ok(()),
        }
    }

    /// Take a partner's record, and write down that it was taken.
    ///
    /// The verdict is the node's; what is journalled is only the half that is a
    /// **fact about this node** — that the record is now in the inbound ledger,
    /// so the retry a partner sends tomorrow is still a retry after a restart
    /// tonight.
    ///
    /// # Errors
    ///
    /// [`NodeError::Journal`] where the acceptance could not be recorded, after
    /// which the node is halted. A refused document journals nothing, because
    /// nothing about this node changed.
    pub fn receive(
        &mut self,
        by: &Actor,
        from: &emob_service::Principal,
        document: &ocpi_kit::v2_3_0::Cdr,
        evidence: Option<emob_cdr::EvidenceRef>,
        at: time::OffsetDateTime,
    ) -> Result<Verdict, NodeError> {
        self.guard()?;
        let verdict = self
            .folded
            .state_mut()
            .receive(from, document, evidence, at);
        if let Verdict::Accepted(inbound) = &verdict {
            let cdr = inbound.cdr.clone();
            let key = cdr.key.to_string();
            let kept = Box::new(document.clone());
            self.folded
                .state_mut()
                .hold_received(document.id.as_str().to_owned(), kept.clone());
            self.commit(
                by,
                &Event::Received {
                    cdr: Box::new(cdr),
                    document: Some(kept),
                },
                &key,
            )?;
        }
        Ok(verdict)
    }
}

impl Roamd {
    /// Fold one recorded fact back in.
    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Consigned {
                consignment,
                document,
            } => {
                if let Some(document) = document {
                    self.outbox.insert(consignment.key.clone(), *document);
                }
                let key = consignment.key.clone();
                self.outbound.insert(key.clone(), *consignment);
                self.reindex(&key);
            }
            Event::Delivered { key, delivery } => {
                if let Some(consignment) = self.outbound.get_mut(&key) {
                    consignment.delivery = delivery;
                }
                self.reindex(&key);
            }
            Event::Offered { peer_id, token } => self.peers.offer(peer_id, token),
            Event::Registered { registration } => self.peers.register(*registration),
            Event::Unregistered { peer_id } => self.peers.unregister(&peer_id),
            Event::Received { cdr, document } => {
                if let Some(document) = document {
                    self.hold_received(document.id.as_str().to_owned(), document);
                }
                let _ = self.inbound.accept(*cdr);
            }
        }
    }
}
