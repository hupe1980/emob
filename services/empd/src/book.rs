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
    /// A token was blocked — a lost card, a chargeback.
    Blocked {
        /// Which token.
        token: TokenRef,
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
            Self::Issued { .. } => "issued",
            Self::Blocked { .. } => "blocked",
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

    /// The provider has stopped, because its ledgers are ahead of its journal.
    #[error(
        "empd has halted: {why} — the provider decided something its journal could not record. \
         A credit limit judged against a total this process holds and the journal does not is a \
         limit nobody can reconstruct. Restarting rebuilds from the journal"
    )]
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
        Ok(Self {
            folded: Folded::open(empty, journal, Empd::apply)?,
            halted: None,
        })
    }

    /// The folded provider, for every question that does not change it.
    #[must_use]
    pub const fn state(&self) -> &Empd {
        self.folded.state()
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

    fn guard(&self) -> Result<(), BookError> {
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
        self.guard()?;
        let token = self
            .folded
            .state_mut()
            .issue_token(contract, uid, token_type, whitelist)?;
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
        self.folded.state_mut().block(token)?;
        self.commit(
            by,
            &Event::Blocked {
                token: token.clone(),
            },
            "a token",
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
        Ok(self.folded.state().authorize(token, on)?)
    }
}

impl Empd {
    /// Fold one recorded fact back in.
    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Contracted { contract } => self.hold(*contract),
            Event::Issued { token, held, .. } => {
                self.tokens.insert(token.as_str().to_owned(), *held);
            }
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
        self.contracts.insert(contract.id.to_string(), contract);
    }
}
