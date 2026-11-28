//! The provider's ledgers, kept where they outlive the process.
//!
//! Three facts, and each fails in a different direction when it is lost. A lost
//! **token store** refuses every driver — loud, and fixed in an hour. A lost
//! **block** admits a card its owner reported stolen. A lost **running total**
//! reads zero, so every session is authorised past the credit limit that was
//! the point of holding it, and nothing raises: a limit that is never reached
//! looks exactly like a limit that is never exceeded.
//!
//! None of the three is a document, which is why no crate holds them and why
//! losing them is invisible until a month is wrong.

use emob_core::Money;
use emob_service::Actor;
use emob_service::journal::{Folded, Journal, JournalError, Kinded};

use crate::{
    Allowed, Contract, ContractId, Counted, Empd, ProviderError, SessionId, Token, TokenRef,
    TokenType, Whitelist,
};

/// One fact this provider decided.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// A contract was taken on, or restated.
    Contracted {
        /// The contract.
        contract: Box<Contract>,
    },
    /// A token was issued against a contract.
    Issued {
        /// The keyed digest the token is known by.
        token: TokenRef,
        /// The data key the reference was derived and the uid sealed under
        /// (SR-047) — so a key can be rotated and a record under a destroyed
        /// one is recognised as erased rather than as a forgery.
        key: emob_service::vault::KeyId,
        /// What it carries.
        held: Box<Token>,
    },
    /// The provider registered a card's printed number for exact support lookup.
    Labelled {
        /// Which card.
        token: TokenRef,
        /// Keyed lookup digest of the printed number, or absent when removed.
        digest: Option<String>,
    },
    /// A token was blocked — a lost card, a chargeback.
    Blocked {
        /// Which token.
        token: TokenRef,
    },
    /// A named operator confirmed a change to online card authorization.
    CardChanged {
        /// The reviewed request, its confirmation time and resulting state.
        change: crate::CardChange,
    },
    /// One accepted native agreement and its first physical card are indivisible.
    Enrolled {
        /// Keyed fingerprint of the bill-to profile accepted for this account.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<emob_service::vault::Digest>,
        /// Exact terms, card reference, actor and acceptance time.
        agreement: Box<crate::Agreement>,
        /// Sealed UID and online-only standing of the issued card.
        held: Box<Token>,
    },
    /// The sole live physical card changed, or the contract ended.
    AgreementChanged {
        /// Effective operation and immutable agreement identity.
        change: Box<crate::AgreementChange>,
        /// Retired card.
        previous: TokenRef,
        /// Sealed replacement, when another card is issued.
        replacement: Option<Box<(TokenRef, Token)>>,
    },
    /// A private withdrawal request or its reviewed disposition.
    Withdrawal {
        /// Sealed evidence and agreement binding.
        evidence: emob_service::vault::Sealed,
    },
    /// Identifies the independent custody for this provider journal.
    CustodyBound {
        /// Random store identity pinned on first initialization.
        id: String,
    },
    /// The independently committed erasure acknowledged to a named privacy reviewer.
    CustomerErased {
        /// Blinded customer selector in the independent custody.
        subject_tag: String,
        /// Original immutable erasure operation.
        operation: String,
    },
    /// A named operator changed one customer access assignment.
    CustomerGrant {
        /// Blinded customer key selector.
        subject_tag: String,
        /// Sealed subject, target and desired standing.
        change: emob_service::vault::Sealed,
    },
    /// A session was counted against a contract.
    RanUp {
        /// Which contract.
        contract: ContractId,
        /// Which session, because a running total cannot tell one session from
        /// two that cost the same (D309).
        session: SessionId,
        /// What it came to.
        amount: Money,
    },
    /// Sessions were invoiced and came off the running total.
    Invoiced {
        /// Which contract.
        contract: ContractId,
        /// Which sessions the document covers.
        sessions: Vec<SessionId>,
    },
}

impl Kinded for Event {
    fn kind(&self) -> &'static str {
        match self {
            Self::Contracted { .. } => "contracted",
            Self::Withdrawal { .. } => "withdrawal",
            Self::CustodyBound { .. } => "custody_bound",
            Self::CustomerGrant { .. } => "customer_grant",
            Self::CustomerErased { .. } => "customer_erased",
            Self::Issued { .. } => "issued",
            Self::AgreementChanged { .. } => "agreement_changed",
            Self::Enrolled { .. } => "enrolled",
            Self::Labelled { .. } => "labelled",
            Self::Blocked { .. } => "blocked",
            Self::CardChanged { .. } => "card_changed",
            Self::RanUp { .. } => "ran_up",
            Self::Invoiced { .. } => "invoiced",
        }
    }
}

/// What went wrong that was not about a token or a contract.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BookError {
    /// The decision itself was refused.
    #[error(transparent)]
    Provider(Box<ProviderError>),

    /// A quantity could not be added to the running total.
    #[error(transparent)]
    Core(#[from] emob_core::CoreError),

    /// The journal could not take the record.
    #[error("the journal refused a record for {about}: {source}")]
    Journal {
        /// What the provider had just decided on.
        about: String,
        /// Why.
        source: JournalError,
    },

    /// The provider stopped after storage failed to acknowledge a decision.
    #[error("empd has halted: {why}. Restarting rebuilds the ledgers from the journal")]
    Halted {
        /// What the journal said when it refused.
        why: String,
    },
}

impl From<ProviderError> for BookError {
    fn from(source: ProviderError) -> Self {
        Self::Provider(Box::new(source))
    }
}

/// [`Empd`], folded from its journal and extended by appending to it.
pub struct Book<J: Journal> {
    folded: Folded<Empd, Event, J>,
    halted: Option<String>,
    pub(crate) pending: crate::actions::Pending,
    pub(crate) lifecycle_reviews: std::collections::BTreeMap<Actor, crate::lifecycle::Review>,
    pub(crate) withdrawal_reviews: std::collections::BTreeMap<Actor, crate::withdrawal::Review>,
    pub(crate) erasure_reviews:
        std::collections::BTreeMap<Actor, crate::customer::erasure::Pending>,
    pub(crate) enrol_reviews: crate::enrol::Reviews,
}

impl<J: Journal> core::fmt::Debug for Book<J> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Book")
            .field("halted", &self.halted)
            .finish_non_exhaustive()
    }
}

impl<J: Journal> Book<J> {
    /// Rebuild the provider from its journal.
    ///
    /// The key the token references are derived under is **not** journalled and
    /// is supplied here: it is a secret, and a journal that carried it would
    /// make every digest in the file reversible by anybody holding the file —
    /// which is the whole property [`TokenRef`] exists for. The consequence is
    /// stated rather than hidden: a provider restarted under a different key
    /// cannot resolve the tokens it issued under the old one, exactly as it
    /// could not before.
    ///
    /// # Errors
    ///
    /// [`JournalError::Broken`] when the journal does not chain, and
    /// [`JournalError::Encoding`] when it holds a fact this build cannot read.
    pub fn open(empty: Empd, journal: J) -> Result<Self, JournalError> {
        let folded = Folded::open(empty, journal, Empd::apply)?;
        if !folded.state().grants.readable() {
            return Err(JournalError::Encoding {
                doing: "opening customer grants",
                source: serde::de::Error::custom(
                    "restore the current customer custody and original provider data key",
                ),
            });
        }
        if folded.state().withdrawals.unreadable {
            return Err(JournalError::Encoding {
                doing: "opening withdrawal evidence",
                source: serde::de::Error::custom("restore the original empd data key and journal"),
            });
        }
        Ok(Self {
            folded,
            halted: None,
            pending: crate::actions::Pending::new(),
            lifecycle_reviews: std::collections::BTreeMap::new(),
            withdrawal_reviews: std::collections::BTreeMap::new(),
            erasure_reviews: std::collections::BTreeMap::new(),
            enrol_reviews: crate::enrol::Reviews::default(),
        })
    }

    /// The folded provider, for every question that does not change it.
    #[must_use]
    pub const fn state(&self) -> &Empd {
        self.folded.state()
    }

    pub(crate) fn forget_customer_grants(&mut self, subject: &str) {
        self.folded.state_mut().grants.forget(subject);
    }

    /// The journal under it.
    #[must_use]
    pub const fn journal(&self) -> &J {
        self.folded.journal()
    }

    /// Whether the provider has stopped deciding.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.halted.is_some()
    }

    pub(crate) fn guard(&self) -> Result<(), BookError> {
        match &self.halted {
            None => Ok(()),
            Some(why) => Err(BookError::Halted { why: why.clone() }),
        }
    }

    fn commit(&mut self, by: &Actor, event: &Event, about: &str) -> Result<(), BookError> {
        match self.folded.note(by, event) {
            Ok(_) => Ok(()),
            Err(source) => {
                self.halted = Some(source.to_string());
                Err(BookError::Journal {
                    about: about.to_owned(),
                    source,
                })
            }
        }
    }

    // A standing becomes visible only after storage acknowledges its record.
    pub(crate) fn record_fact(&mut self, by: &Actor, event: Event) -> Result<(), BookError> {
        let kind = event.kind();
        self.folded.record(by, event).map_err(|source| {
            self.halted = Some(source.to_string());
            BookError::Journal {
                about: kind.into(),
                source,
            }
        })?;
        Ok(())
    }

    /// Take a contract on, as `by`.
    ///
    /// Every fact names who caused it (SR-100): the estate at start, `csmsd`
    /// for what its authorisation request or a delivery drove, an operator by
    /// the name the gate authenticated — never a name read from a body.
    ///
    /// # Errors
    ///
    /// [`BookError::Journal`] where the fact could not be recorded.
    pub fn with(&mut self, by: &Actor, contract: Contract) -> Result<(), BookError> {
        self.guard()?;
        let about = contract.id.to_string();
        self.folded.state_mut().hold(contract.clone());
        self.commit(
            by,
            &Event::Contracted {
                contract: Box::new(contract),
            },
            &about,
        )
    }

    /// Issue a token against a contract.
    ///
    /// # Errors
    ///
    /// [`BookError::Provider`] for a contract this provider does not hold, and
    /// [`BookError::Journal`] where the issue could not be recorded.
    pub fn issue_token(
        &mut self,
        by: &Actor,
        contract: &ContractId,
        uid: impl Into<String>,
        token_type: TokenType,
        whitelist: Whitelist,
    ) -> Result<TokenRef, BookError> {
        self.issue_with_standing(by, contract, uid, token_type, whitelist, false)
    }

    pub(crate) fn issue_with_standing(
        &mut self,
        by: &Actor,
        contract: &ContractId,
        uid: impl Into<String>,
        token_type: TokenType,
        whitelist: Whitelist,
        blocked: bool,
    ) -> Result<TokenRef, BookError> {
        self.guard()?;
        let token = self
            .folded
            .state_mut()
            .issue_token(contract, uid, token_type, whitelist)?;
        if blocked {
            self.folded.state_mut().block(&token)?;
        }
        let held = self
            .folded
            .state()
            .token(&token)
            .cloned()
            .ok_or_else(|| BookError::Halted {
                why: "a token was issued and immediately not found".to_owned(),
            })?;
        self.commit(
            by,
            &Event::Issued {
                token: token.clone(),
                key: self.folded.state().key_id().clone(),
                held: Box::new(held),
            },
            contract.as_str(),
        )?;
        Ok(token)
    }

    pub(crate) fn label(
        &mut self,
        by: &Actor,
        token: &TokenRef,
        digest: Option<String>,
    ) -> Result<bool, BookError> {
        self.guard()?;
        let held = self
            .state()
            .token(token)
            .ok_or_else(|| ProviderError::UnknownToken {
                token: token.as_str().to_owned(),
            })?;
        if held.visual_digest == digest {
            return Ok(false);
        }
        self.folded.state_mut().set_label(token, digest.clone());
        self.commit(
            by,
            &Event::Labelled {
                token: token.clone(),
                digest,
            },
            "a card label",
        )?;
        Ok(true)
    }

    /// Block a token.
    ///
    /// # Errors
    ///
    /// [`BookError::Provider`] for a token this provider never issued, and
    /// [`BookError::Journal`] where the block could not be recorded — which is
    /// the one that matters most here, because a block that is forgotten is a
    /// lost card that authorises again after a restart.
    pub fn block(&mut self, by: &Actor, token: &TokenRef) -> Result<(), BookError> {
        self.guard()?;
        self.state()
            .token(token)
            .ok_or_else(|| ProviderError::UnknownToken {
                token: token.as_str().to_owned(),
            })?;
        self.record_fact(
            by,
            Event::Blocked {
                token: token.clone(),
            },
        )
    }

    /// Count a session against a contract's running total.
    ///
    /// # Errors
    ///
    /// [`BookError::Core`] where the amount is in another currency than the
    /// contract's running total, and [`BookError::Journal`] otherwise.
    pub fn ran_up(
        &mut self,
        by: &Actor,
        contract: &ContractId,
        session: &SessionId,
        amount: Money,
    ) -> Result<Counted, BookError> {
        self.guard()?;
        let counted = self.folded.state_mut().ran_up(contract, session, amount)?;
        // A session already counted **at the same figure** changes nothing, so
        // it records nothing: a journal that grew by one line per retry would
        // make a replay's cost proportional to a partner's flakiness. A
        // *restatement* is not that — a re-rated record moves the total, and a
        // fold that skipped it would rebuild a contract owing the old figure.
        if !matches!(counted, Counted::Already) {
            self.commit(
                by,
                &Event::RanUp {
                    contract: contract.clone(),
                    session: session.clone(),
                    amount,
                },
                contract.as_str(),
            )?;
        }
        Ok(counted)
    }

    /// Take invoiced sessions off a contract's running total.
    ///
    /// # Errors
    ///
    /// [`BookError::Provider`] where a session named is not one this contract
    /// ran up, and [`BookError::Journal`] otherwise.
    pub fn invoiced(
        &mut self,
        by: &Actor,
        contract: &ContractId,
        sessions: &[SessionId],
    ) -> Result<Option<Money>, BookError> {
        self.guard()?;
        let total = self.folded.state_mut().invoiced(contract, sessions)?;
        self.commit(
            by,
            &Event::Invoiced {
                contract: contract.clone(),
                sessions: sessions.to_vec(),
            },
            contract.as_str(),
        )?;
        Ok(total)
    }

    /// What an operator asking about a token is told.
    ///
    /// A read, and here because a caller holding a [`Book`] should not have to
    /// reach through it for the one question this service exists to answer.
    ///
    /// # Errors
    ///
    /// [`BookError::Provider`] for a token this provider never issued.
    pub fn authorize(&self, token: &TokenRef, on: time::Date) -> Result<Allowed, BookError> {
        self.guard()?;
        Ok(self.folded.state().authorize(token, on)?)
    }
}

impl Empd {
    /// Fold one recorded fact back in.
    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Withdrawal { evidence } => self.apply_withdrawal(&evidence),
            Event::AgreementChanged {
                change,
                previous,
                replacement,
            } => self.apply_contract_change(*change, previous, replacement.map(|r| *r)),
            Event::Contracted { contract } => self.hold(*contract),
            Event::CustodyBound { id } => {
                if !self.custody.matches(&id) {
                    self.grants.reject_custody();
                }
            }
            Event::CustomerErased {
                subject_tag,
                operation,
            } => self.apply_erasure_witness(subject_tag, &operation),
            Event::CustomerGrant {
                subject_tag,
                change,
            } => self.apply_customer_grant(&subject_tag, &change),
            Event::Enrolled {
                agreement,
                held,
                profile,
            } => self.apply_agreement(*agreement, *held, profile),
            Event::CardChanged { change } => self.apply_change(change),
            Event::Issued { token, held, .. } => {
                self.store_card(&token, *held);
            }
            Event::Labelled { token, digest } => self.set_label(&token, digest),
            Event::Blocked { token } => {
                let _ = self.block(&token);
            }
            Event::RanUp {
                contract,
                session,
                amount,
            } => {
                let _ = self.ran_up(&contract, &session, amount);
            }
            Event::Invoiced { contract, sessions } => {
                let _ = self.invoiced(&contract, &sessions);
            }
        }
    }

    /// Hold a contract, replacing whatever was held under its id.
    pub(crate) fn hold(&mut self, contract: Contract) {
        if let Ok(emaid) = emob_core::Emaid::parse(contract.id.as_str()) {
            self.account_ids.insert(emaid);
        }
        self.contracts.insert(contract.id.to_string(), contract);
    }
}
