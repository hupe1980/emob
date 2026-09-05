//! `empd` — the provider side of a contract.
//!
//! # The token store `emob-roam` asks for by name
//!
//! [`emob_session::Authorization`] refuses to store an RFID UID: a UID is a
//! lifelong identifier of a physical object a person carries, and a session row
//! holding one builds a movement profile nothing in this platform needs. OCPI
//! requires that same UID on every outgoing CDR. Those two facts do not compose
//! inside a crate, and [`RoamingToken`]'s own documentation says what resolves
//! them — *"a service with a key and a database"*. The mapping lives here, and
//! the UID reaches only the records that are leaving.
//!
//! Three more things are here for the same reason. None is a property of a
//! token, a tariff or a session.
//!
//! # 1. Whether a contract authorises this session, now
//!
//! `[OCPI 2.3.0 §mod_tokens]` takes one of five answers, and four need something
//! no document has: the token's standing, a **clock** against the contract's
//! window, and a **ledger** of sessions nobody has invoiced yet.
//!
//! The whitelist is a **two-sided** rule, and that is the half a platform gets
//! wrong in silence. A token published as `ALWAYS` may not be asked about in real
//! time; one published as `NEVER` may not be started from a list. Each is a
//! session somebody will not be paid for, and neither surfaces as an error
//! anywhere else.
//!
//! # 2. What the driver is quoted before they plug in
//!
//! `[AFIR Art. 5(5)]` binds the *provider*: it must disclose *"all price
//! information specific to that recharging session … clearly distinguishing all
//! price components, including applicable e-roaming costs and other fees or
//! charges applied by the mobility service provider"*.
//!
//! The operator's components are `emob_tariff::describe`'s, passed through
//! unchanged; this service's own follow, each named. The same paragraph forbids a
//! cross-border surcharge outright rather than capping it, so a [`Markup`]
//! belongs to a partner and has no country in it — which is what lets
//! [`Empd::provider_profile`] *derive* what [`emob_core::ProviderProfile`] takes
//! as four booleans.
//!
//! # 3. The fee that is owed whether or not anybody charged
//!
//! C-60/23 turns on a fixed fee charged *"regardless of whether the user actually
//! purchased electricity during the relevant period"*. An invoice is assembled
//! from records and a contract with no sessions produces none, so the one line
//! owed anyway is invisible downstream of the ledger. [`Empd::fees_for`] is where
//! it comes from, and `billd` puts it on the document.
//!
//! # No I/O
//!
//! Nothing here opens a socket or reads a clock. Every date and instant is an
//! argument, so two runs of one authorisation give one answer.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use std::collections::{BTreeMap, BTreeSet};

use emob_billing::Subscription;
use emob_core::{ContractId, Currency, Money, PartyId};
use emob_roam::{RoamingToken, TokenType};
use emob_session::TokenRef;
use rust_decimal::Decimal;

/// How a token is treated by an operator that holds it in a list.
///
/// `[OCPI 2.3.0 §mod_tokens_whitelisttype_enum]`. The two extremes are
/// **instructions to the operator**, not preferences: `Always` says never ask,
/// `Never` says always ask, and each of them is a rule with a matching refusal
/// on this side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whitelist {
    /// Start the session from the list; never ask.
    Always,
    /// The list may be used, and a real-time request is also fine.
    Allowed,
    /// The list may be used only while the operator cannot reach us.
    AllowedOffline,
    /// Always ask; the list may not be used.
    Never,
}

impl Whitelist {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "ALWAYS",
            Self::Allowed => "ALLOWED",
            Self::AllowedOffline => "ALLOWED_OFFLINE",
            Self::Never => "NEVER",
        }
    }
}

/// What the provider charges on top of the operator's price.
///
/// # There is no country in this type, and that is the design
///
/// `[AFIR Art. 5(5)]` does not ask a cross-border surcharge to be reasonable and
/// transparent — it forbids it: *"Mobility service providers shall not apply any
/// extra charges for cross-border e-roaming."* A markup that could be stated per
/// country is one an operator can get wrong, and a compliance flag asserting
/// they did not is a claim rather than a fact.
///
/// So a markup belongs to a **partner**. Charging differently for two partners
/// is an ordinary commercial difference — their roaming costs differ — and the
/// point's country never reaches the arithmetic, which is what lets
/// [`Empd::provider_profile`] state the answer instead of being told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markup {
    /// Added to the operator's price per kilowatt-hour.
    pub energy: Decimal,
    /// Added once per session.
    pub session: Decimal,
    /// The e-roaming cost, which the article requires be named **separately**.
    ///
    /// Its own field rather than a share of [`Self::energy`], because *"clearly
    /// distinguishing all price components, including applicable e-roaming
    /// costs"* is a requirement about the quote a driver reads, and a cost
    /// folded into the kilowatt-hour price is one they cannot distinguish.
    pub e_roaming: Decimal,
}

impl Markup {
    /// A markup that adds nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            energy: Decimal::ZERO,
            session: Decimal::ZERO,
            e_roaming: Decimal::ZERO,
        }
    }

    /// A markup per kilowatt-hour.
    #[must_use]
    pub const fn per_kwh(energy: Decimal) -> Self {
        Self {
            energy,
            session: Decimal::ZERO,
            e_roaming: Decimal::ZERO,
        }
    }

    /// …with a session fee.
    #[must_use]
    pub const fn and_per_session(mut self, session: Decimal) -> Self {
        self.session = session;
        self
    }

    /// …and the e-roaming cost, named.
    #[must_use]
    pub const fn and_e_roaming(mut self, e_roaming: Decimal) -> Self {
        self.e_roaming = e_roaming;
        self
    }

    /// Whether this markup charges anything at all.
    #[must_use]
    pub fn is_free(&self) -> bool {
        self.energy.is_zero() && self.session.is_zero() && self.e_roaming.is_zero()
    }
}

/// A periodic fee, and the reason it is not a session charge.
///
/// C-60/23 held access to the network to be a **separate and independent**
/// supply of services, precisely because the fee is charged *"regardless of
/// whether the user actually purchased electricity during the relevant period"*.
/// A month with no sessions still owes it, and that is a sentence no ledger of
/// records can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fee {
    /// What it is called on the invoice.
    pub description: String,
    /// The **net** amount, because a document states BT-146 exclusive of VAT.
    pub net: Decimal,
}

impl Fee {
    /// A monthly fee.
    #[must_use]
    pub fn monthly(description: impl Into<String>, net: Decimal) -> Self {
        Self {
            description: description.into(),
            net,
        }
    }
}

/// One driver's contract with this provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// The contract identifier a CDR routes on.
    pub id: ContractId,
    /// The day it takes effect.
    pub from: time::Date,
    /// …and the day after which it does not, when it has one.
    pub until: Option<time::Date>,
    /// The periodic fee, when the contract carries one.
    pub fee: Option<Fee>,
    /// What may be outstanding and unbilled before a session is refused.
    ///
    /// `None` is no limit, which is the ordinary post-paid contract. A limit is
    /// what makes `NO_CREDIT` an answer this service can give at all: it is a
    /// question about **sessions nobody has invoiced yet**, and no document
    /// knows that.
    pub credit_limit: Option<Money>,
}

impl Contract {
    /// A contract with no end and no fee.
    #[must_use]
    pub const fn new(id: ContractId, from: time::Date) -> Self {
        Self {
            id,
            from,
            until: None,
            fee: None,
            credit_limit: None,
        }
    }

    /// …that ends.
    #[must_use]
    pub const fn until(mut self, until: time::Date) -> Self {
        self.until = Some(until);
        self
    }

    /// …that carries a periodic fee.
    #[must_use]
    pub fn charging(mut self, fee: Fee) -> Self {
        self.fee = Some(fee);
        self
    }

    /// …with a ceiling on what may go unbilled.
    #[must_use]
    pub const fn limited_to(mut self, credit_limit: Money) -> Self {
        self.credit_limit = Some(credit_limit);
        self
    }

    /// Whether the contract is in force on a given day.
    ///
    /// Inclusive at both ends: a contract that runs *until* the last of the
    /// month covers a session on that day, which is what a driver reading their
    /// own contract expects and what a provider that cut it off at midnight on
    /// the last day would be sued over.
    #[must_use]
    pub fn is_in_force(&self, on: time::Date) -> bool {
        on >= self.from && self.until.is_none_or(|until| on <= until)
    }
}

/// A token this provider issued, and how the operator may treat it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The contract it bills against.
    pub contract: ContractId,
    /// The uid OCPI requires, and a session deliberately does not hold.
    pub uid: String,
    /// What the driver holds up.
    pub token_type: TokenType,
    /// How an operator holding it in a list may treat it.
    pub whitelist: Whitelist,
    /// Whether the provider has blocked it — a lost card, a chargeback.
    pub blocked: bool,
}

/// The answer to `[OCPI 2.3.0 §mod_tokens]`'s real-time question.
///
/// OCPI's own `AllowedType`. Five answers rather than a boolean, because the
/// operator does something different with each: `Blocked` stops the driver,
/// `NoCredit` is a message about money, and `NotAllowed` is *this* point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// Start the session.
    Allowed,
    /// The token is blocked.
    Blocked,
    /// The contract is not in force.
    Expired,
    /// The driver is past what may go unbilled.
    NoCredit,
    /// Not at this location.
    NotAllowed,
}

impl Allowed {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "ALLOWED",
            Self::Blocked => "BLOCKED",
            Self::Expired => "EXPIRED",
            Self::NoCredit => "NO_CREDIT",
            Self::NotAllowed => "NOT_ALLOWED",
        }
    }

    /// Whether a session may start.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// One priced component of a quote, in the driver's own vocabulary.
///
/// `[AFIR Art. 5(5)]` asks for *"all price components"* to be *"clearly
/// distinguished"*, and who charges a component is part of distinguishing it: a
/// driver comparing two providers at one point is comparing the second column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteComponent {
    /// What it is called.
    pub description: String,
    /// The price.
    pub price: Decimal,
    /// The unit it is quoted in — `kWh`, `min`, `session`.
    pub unit: &'static str,
    /// Who charges it.
    pub charged_by: ChargedBy,
}

/// Which party a quoted component belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargedBy {
    /// The operator of the point.
    Operator,
    /// This provider.
    Provider,
    /// This provider, and it is the e-roaming cost the article names.
    ProviderERoaming,
}

/// What a driver is told before they plug in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// Every component, operator's first in `[AFIR Art. 5(4)]`'s order and this
    /// provider's after them.
    pub components: Vec<QuoteComponent>,
    /// The currency.
    pub currency: Currency,
    /// Whether the prices include tax.
    pub tax_included: emob_tariff::TaxIncluded,
    /// The partner whose point this is.
    pub operator: PartyId,
}

impl Quote {
    /// The components one party charges.
    pub fn charged_by(&self, party: ChargedBy) -> impl Iterator<Item = &QuoteComponent> {
        self.components
            .iter()
            .filter(move |component| component.charged_by == party)
    }

    /// Whether the operator's half of this quote is the operator's own price,
    /// unchanged.
    ///
    /// The article's substance, asked of the quote rather than of a checkbox.
    /// The fold `[AFIR Art. 5(5)]` forbids is a provider that adds five cents to
    /// the operator's kilowatt-hour price and shows the driver one number: every
    /// component is still "clearly distinguished", and the driver comparing two
    /// providers at one point is comparing two different accounts of the same
    /// operator's price.
    ///
    /// So the test is not that the components are named. It is that the ones
    /// attributed to the **operator** are the ones the operator published.
    #[must_use]
    pub fn passes_the_operators_price_through(
        &self,
        tariff: &emob_tariff::Tariff,
        at: time::OffsetDateTime,
    ) -> bool {
        let published = emob_tariff::describe(tariff, at);
        let quoted: Vec<Decimal> = self
            .charged_by(ChargedBy::Operator)
            .map(|component| component.price)
            .collect();
        published.lines.len() == quoted.len()
            && published
                .lines
                .iter()
                .zip(&quoted)
                .all(|(line, price)| line.price == *price)
    }
}

/// Why a request could not be answered.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// A token was issued against a contract this provider does not hold.
    #[error("no contract {contract} to issue a token against")]
    NoContract {
        /// Which one.
        contract: String,
    },

    /// A token was presented that this provider never issued.
    #[error("token {token} was not issued by this provider")]
    UnknownToken {
        /// Its reference, which is a digest rather than the uid.
        token: String,
    },

    /// An operator asked in real time about a token published as `ALWAYS`.
    ///
    /// Not pedantry. `ALWAYS` tells the operator to start from its own list and
    /// never ask, so an operator that asks is one whose list is stale — and the
    /// sessions it is *not* asking about are being started against whatever that
    /// stale list says, including tokens this provider has since blocked.
    #[error(
        "token {token} is published as ALWAYS, which tells an operator to start from its list and \
         never ask: a real-time request for it means the list it is starting other sessions from \
         is stale"
    )]
    AskedAboutAlways {
        /// Its reference.
        token: String,
    },

    /// A session was started from a list for a token published as `NEVER`.
    #[error(
        "token {token} is published as NEVER, so a session started from a list was never \
         authorised by this provider: the CDR for it arrives with nobody to bill"
    )]
    StartedFromListWhenNever {
        /// Its reference.
        token: String,
    },

    /// A token was issued against a contract identifier that does not check.
    ///
    /// `emob-roam` verifies this at the crossing *"because here is the last
    /// place anyone looks"*. This is one place earlier and strictly better: a
    /// card issued against a mistyped identifier is refused at the counter
    /// rather than three weeks later, when the record has left, the operator has
    /// been paid, and the session is billed to somebody else's contract.
    #[error(transparent)]
    Contract(#[from] emob_roam::RoamError),

    /// A quote was asked for a partner this provider has no markup for.
    ///
    /// A refusal rather than a zero markup, because a quote that silently
    /// charges nothing is one the driver is entitled to hold this provider to.
    #[error("no price list for partner {partner}: a quote cannot be assembled from a default")]
    NoMarkup {
        /// Which partner.
        partner: String,
    },
}

/// The provider: contracts, the tokens inside them, and the price list.
#[derive(Debug, Clone)]
pub struct Empd {
    me: PartyId,
    contracts: BTreeMap<String, Contract>,
    tokens: BTreeMap<String, Token>,
    markups: BTreeMap<String, Markup>,
    /// What each contract has run up that nobody has invoiced yet.
    unbilled: BTreeMap<String, Money>,
    /// The key the token store is hashed under.
    key: Vec<u8>,
    /// The operators whose meters this provider bills on and from whom it holds
    /// the `[MessEG §33(2)]` confirmation.
    ///
    /// Paperwork rather than data: the statute asks a provider to have the
    /// operator *confirm* that it meets its own meter duties, which lives in a
    /// roaming agreement and in no message this service exchanges. So it is
    /// stated rather than derived — and stated **per operator**, because a
    /// provider that peers with nine and has the confirmation from eight is not
    /// compliant on the ninth.
    confirmations: BTreeSet<String>,
}

impl Empd {
    /// A provider with an empty book.
    ///
    /// The key is the one the token references are derived under. It is an
    /// argument because it is a secret: a store whose digests can be recomputed
    /// by anybody holding the crate is one where a UID can be confirmed by
    /// guessing, which is the privacy property [`TokenRef`] exists for.
    #[must_use]
    pub fn new(me: PartyId, key: impl Into<Vec<u8>>) -> Self {
        Self {
            me,
            contracts: BTreeMap::new(),
            tokens: BTreeMap::new(),
            markups: BTreeMap::new(),
            unbilled: BTreeMap::new(),
            key: key.into(),
            confirmations: BTreeSet::new(),
        }
    }

    /// Record the `[MessEG §33(2)]` confirmation held from one operator.
    #[must_use]
    pub fn confirmed_by(mut self, operator: &PartyId) -> Self {
        self.confirmations.insert(operator.to_string());
        self
    }

    /// The operators this provider holds a `[MessEG §33(2)]` confirmation from.
    pub fn confirmations(&self) -> impl Iterator<Item = &str> {
        self.confirmations.iter().map(String::as_str)
    }

    /// Who this is.
    #[must_use]
    pub const fn party(&self) -> &PartyId {
        &self.me
    }

    /// Take on a contract.
    #[must_use]
    pub fn with(mut self, contract: Contract) -> Self {
        self.contracts.insert(contract.id.to_string(), contract);
        self
    }

    /// State what this provider charges on top of one partner's price.
    #[must_use]
    pub fn charging(mut self, partner: &PartyId, markup: Markup) -> Self {
        self.markups.insert(partner.to_string(), markup);
        self
    }

    /// One contract.
    #[must_use]
    pub fn contract(&self, id: &ContractId) -> Option<&Contract> {
        self.contracts.get(&id.to_string())
    }

    /// Every contract, in identifier order.
    pub fn contracts(&self) -> impl Iterator<Item = &Contract> {
        self.contracts.values()
    }

    /// Issue a token against a contract, and get back the reference a session
    /// may hold.
    ///
    /// The uid stays here. What the caller gets is the keyed digest
    /// [`emob_session::Authorization`] carries, which is the whole of what a
    /// session row is allowed to know about the card in somebody's pocket.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NoContract`] for a contract this provider does not hold.
    /// A token issued against nothing is one whose CDR arrives with nobody to
    /// bill.
    pub fn issue_token(
        &mut self,
        contract: &ContractId,
        uid: impl Into<String>,
        token_type: TokenType,
        whitelist: Whitelist,
    ) -> Result<TokenRef, ProviderError> {
        if !self.contracts.contains_key(&contract.to_string()) {
            return Err(ProviderError::NoContract {
                contract: contract.to_string(),
            });
        }
        let uid = uid.into();
        // Refused at the counter rather than at the crossing. `RoamingToken` is
        // built here and thrown away: what is being kept is its refusal.
        RoamingToken::new(self.me.clone(), &uid, token_type, contract.clone())?;
        let reference = self.reference_for(&uid);
        self.tokens.insert(
            reference.as_str().to_owned(),
            Token {
                contract: contract.clone(),
                uid,
                token_type,
                whitelist,
                blocked: false,
            },
        );
        Ok(reference)
    }

    /// Block a token — a lost card, a chargeback.
    ///
    /// # Errors
    ///
    /// [`ProviderError::UnknownToken`] for one this provider never issued.
    pub fn block(&mut self, token: &TokenRef) -> Result<(), ProviderError> {
        let stored =
            self.tokens
                .get_mut(token.as_str())
                .ok_or_else(|| ProviderError::UnknownToken {
                    token: token.as_str().to_owned(),
                })?;
        stored.blocked = true;
        Ok(())
    }

    /// One token, by the reference a session holds.
    #[must_use]
    pub fn token(&self, token: &TokenRef) -> Option<&Token> {
        self.tokens.get(token.as_str())
    }

    /// Present a token to a crossing: the uid OCPI requires, at the edge that
    /// has to send it.
    ///
    /// This is the whole reason the service exists. `emob_roam::RoamingToken`
    /// documents the gap in as many words — the crossing needs a uid, a session
    /// refuses to hold one, and the mapping belongs to *"a service with a key
    /// and a database"*. Here it is, and the UID reaches exactly the records
    /// that are leaving and nothing else.
    ///
    /// # Errors
    ///
    /// [`ProviderError::UnknownToken`] for a reference this provider did not
    /// issue, and whatever [`RoamingToken::new`] says about the contract
    /// identifier's check digit — which is verified here because here is the
    /// last place anybody looks.
    pub fn present(&self, token: &TokenRef) -> Result<RoamingToken, ProviderError> {
        let stored =
            self.tokens
                .get(token.as_str())
                .ok_or_else(|| ProviderError::UnknownToken {
                    token: token.as_str().to_owned(),
                })?;
        Ok(RoamingToken::new(
            self.me.clone(),
            stored.uid.clone(),
            stored.token_type,
            stored.contract.clone(),
        )?)
    }

    /// Answer `[OCPI 2.3.0 §mod_tokens]`'s real-time question.
    ///
    /// The order matters and is the order an operator would want to explain to a
    /// driver: blocked first, because a blocked token is a stop whatever else is
    /// true; then the contract's own window; then the money.
    ///
    /// # Errors
    ///
    /// [`ProviderError::UnknownToken`], and
    /// [`ProviderError::AskedAboutAlways`] for a token this provider published
    /// as `ALWAYS` — see that variant for why a question about it is a report of
    /// a stale list rather than a request.
    pub fn authorize(&self, token: &TokenRef, on: time::Date) -> Result<Allowed, ProviderError> {
        let stored =
            self.tokens
                .get(token.as_str())
                .ok_or_else(|| ProviderError::UnknownToken {
                    token: token.as_str().to_owned(),
                })?;
        if stored.whitelist == Whitelist::Always {
            return Err(ProviderError::AskedAboutAlways {
                token: token.as_str().to_owned(),
            });
        }
        Ok(self.decide(stored, on))
    }

    /// Answer for a session an operator started from its own list.
    ///
    /// The other half of the whitelist, and the half nothing else checks. A
    /// token published as `NEVER` may not be started this way, and a CDR for one
    /// that was arrives with nobody to bill — so the refusal is here rather than
    /// three weeks later in a settlement dispute.
    ///
    /// # Errors
    ///
    /// [`ProviderError::UnknownToken`], and
    /// [`ProviderError::StartedFromListWhenNever`].
    pub fn started_from_list(
        &self,
        token: &TokenRef,
        on: time::Date,
    ) -> Result<Allowed, ProviderError> {
        let stored =
            self.tokens
                .get(token.as_str())
                .ok_or_else(|| ProviderError::UnknownToken {
                    token: token.as_str().to_owned(),
                })?;
        if stored.whitelist == Whitelist::Never {
            return Err(ProviderError::StartedFromListWhenNever {
                token: token.as_str().to_owned(),
            });
        }
        Ok(self.decide(stored, on))
    }

    /// Record what a session ran up, against the contract that has not been
    /// invoiced for it yet.
    ///
    /// # Errors
    ///
    /// [`emob_core::CoreError`] where the running total and this session are in
    /// different currencies, which is a contract billed in two currencies and
    /// not something to add up.
    pub fn ran_up(
        &mut self,
        contract: &ContractId,
        amount: Money,
    ) -> Result<(), emob_core::CoreError> {
        let key = contract.to_string();
        let total = match self.unbilled.get(&key) {
            Some(running) => running.checked_add(amount)?,
            None => amount,
        };
        self.unbilled.insert(key, total);
        Ok(())
    }

    /// What a contract has outstanding that nobody has invoiced.
    #[must_use]
    pub fn unbilled(&self, contract: &ContractId) -> Option<Money> {
        self.unbilled.get(&contract.to_string()).copied()
    }

    /// Clear a contract's running total, because a month has been invoiced.
    pub fn invoiced(&mut self, contract: &ContractId) {
        self.unbilled.remove(&contract.to_string());
    }

    /// What a driver is told before they plug in.
    ///
    /// The operator's components come from `emob_tariff::describe`, in the order
    /// `[AFIR Art. 5(4)]` prescribes, because the provider has no business
    /// restating somebody else's price in its own words. This provider's own
    /// charges follow, each named — which is the article's requirement and the
    /// reason a `Quote` is a list rather than a total.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NoMarkup`] for a partner this provider has stated no
    /// price list for. A quote assembled from a default charges nothing, and a
    /// driver shown one is entitled to it.
    pub fn quote(
        &self,
        operator: &PartyId,
        tariff: &emob_tariff::Tariff,
        at: time::OffsetDateTime,
    ) -> Result<Quote, ProviderError> {
        let markup =
            self.markups
                .get(&operator.to_string())
                .ok_or_else(|| ProviderError::NoMarkup {
                    partner: operator.to_string(),
                })?;
        let described = emob_tariff::describe(tariff, at);

        let mut components: Vec<QuoteComponent> = described
            .lines
            .iter()
            .map(|line| QuoteComponent {
                description: format!("{} ({operator})", describe_dimension(line.dimension)),
                price: line.price,
                unit: line.unit(),
                charged_by: ChargedBy::Operator,
            })
            .collect();

        for (price, unit, description, charged_by) in [
            (
                markup.energy,
                "kWh",
                "provider service charge",
                ChargedBy::Provider,
            ),
            (
                markup.session,
                "session",
                "provider session fee",
                ChargedBy::Provider,
            ),
            (
                markup.e_roaming,
                "session",
                "e-roaming cost",
                ChargedBy::ProviderERoaming,
            ),
        ] {
            if !price.is_zero() {
                components.push(QuoteComponent {
                    description: description.to_owned(),
                    price,
                    unit,
                    charged_by,
                });
            }
        }

        Ok(Quote {
            components,
            currency: described.currency,
            tax_included: described.tax_included,
            operator: operator.clone(),
        })
    }

    /// The fees owed for a period, whichever contracts charged electricity.
    ///
    /// C-60/23's own words: the fee is charged *"regardless of whether the user
    /// actually purchased electricity during the relevant period"*. So this is
    /// derived from the **contracts in force**, not from the records — a month
    /// with no sessions still produces the line, and there is nothing downstream
    /// of the ledger that could have known it.
    ///
    /// A contract that starts or ends inside the period is included, because the
    /// access it charges for existed for part of it. Whether that fee is then
    /// pro-rated is a commercial decision this service does not make: the amount
    /// is the contract's own.
    #[must_use]
    pub fn fees_for(&self, from: time::Date, to: time::Date) -> Vec<(ContractId, Subscription)> {
        self.contracts
            .values()
            .filter(|contract| {
                contract.from <= to && contract.until.is_none_or(|until| until >= from)
            })
            .filter_map(|contract| {
                contract.fee.as_ref().map(|fee| {
                    (
                        contract.id.clone(),
                        Subscription::new(fee.description.clone(), fee.net, from, to),
                    )
                })
            })
            .collect()
    }

    /// What `[AFIR Art. 5(5)]` asks, answered from the price list.
    ///
    /// `emob_core::ProviderProfile` takes four booleans, and a boolean somebody
    /// ticked is a claim. Three of these are facts about this service's own
    /// data — every quote it can build names each component and names the
    /// e-roaming cost separately — and the fourth is a fact about the **type**:
    /// [`Markup`] has no country in it, so a price list that varies with where
    /// the point stands is not a thing this provider can express.
    ///
    /// A provider with no price list at all discloses nothing, which is what the
    /// profile then says.
    ///
    /// # …and the fifth is a document, checked against a list this service has
    ///
    /// `[MessEG §33(2)]` asks a provider to hold a **confirmation** from the
    /// operator of every meter whose values it bills on. The confirmation is
    /// paperwork in a roaming agreement, in no message this service exchanges,
    /// so it is stated through [`Self::confirmed_by`]. **Who it is owed from is
    /// not**: a provider bills on the meters of the operators it has a
    /// [`Markup`] for, and that list is this service's own. So the answer is a
    /// conjunction over a derived set rather than a boolean somebody ticked —
    /// eight confirmations out of nine peers is a breach on the ninth, and
    /// adding a peer without its confirmation moves the answer by itself.
    #[must_use]
    pub fn provider_profile(&self) -> emob_core::ProviderProfile {
        let stating = !self.markups.is_empty();
        emob_core::ProviderProfile {
            party: self.me.clone(),
            discloses_all_price_components: stating,
            // Named separately by construction: `Markup::e_roaming` is its own
            // field and `quote` emits it as its own component.
            discloses_e_roaming_costs: stating,
            discloses_electronically: stating,
            // Unrepresentable rather than asserted: see `Markup`.
            surcharges_cross_border_roaming: false,
            // A conjunction over the operators this provider actually bills
            // on, which is the set it holds a markup for.
            holds_meter_operator_confirmation: self
                .markups
                .keys()
                .all(|operator| self.confirmations.contains(operator)),
        }
    }

    /// The four answers that do not depend on how the operator asked.
    fn decide(&self, token: &Token, on: time::Date) -> Allowed {
        if token.blocked {
            return Allowed::Blocked;
        }
        let Some(contract) = self.contracts.get(&token.contract.to_string()) else {
            return Allowed::Expired;
        };
        if !contract.is_in_force(on) {
            return Allowed::Expired;
        }
        if let (Some(limit), Some(outstanding)) = (
            contract.credit_limit,
            self.unbilled.get(&token.contract.to_string()),
        ) && outstanding.currency() == limit.currency()
            && outstanding.amount() >= limit.amount()
        {
            return Allowed::NoCredit;
        }
        Allowed::Allowed
    }

    /// The keyed digest a session may hold in place of a uid.
    fn reference_for(&self, uid: &str) -> TokenRef {
        use sha2::{Digest as _, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&self.key);
        hasher.update(uid.as_bytes());
        TokenRef::new(hex::encode(hasher.finalize()))
            .unwrap_or_else(|_| unreachable!("a SHA-256 digest is 64 lowercase hex characters"))
    }
}

/// A dimension in the words a driver reads.
fn describe_dimension(dimension: emob_tariff::Dimension) -> &'static str {
    match dimension {
        emob_tariff::Dimension::Energy => "electricity",
        emob_tariff::Dimension::Time => "charging time",
        emob_tariff::Dimension::ParkingTime => "occupancy",
        emob_tariff::Dimension::Flat => "session fee",
    }
}
