//! `billd` — the service that closes a month.
//!
//! # What it decides, and what it must not
//!
//! `emob-billing` turns rated records into a document: the rounding that happens
//! once at the line, the VAT treatment derived from the parties, the EN 16931
//! semantic invoice and the verdict on it, the pain.008 that draws it, and
//! postings addressed by **role**. Everything that could be *wrong about a
//! document* is there and is tested there.
//!
//! Four things are not, and none of them is a property of any document:
//!
//! 1. **What number it carries.** `[UStG §14(4) Nr. 4]` requires *"eine
//!    fortlaufende Nummer mit einer oder mehreren Zahlenreihen, die zur
//!    Identifizierung der Rechnung vom Rechnungsaussteller **einmalig** vergeben
//!    wird"*. Unique, and issued by the person issuing the invoice — which is a
//!    fact about a **counter**, not about a month's records. Two closings that
//!    each produce `R-2026-0001` is the failure, and no crate can see it.
//! 2. **When a period is closed, and that it closes once.** A month re-closed
//!    against the same records is not a second month; it is either the same
//!    invoice or a correction, and which of those it is decides whether money
//!    moves twice.
//! 3. **Which invoice supersedes which.** A re-rated month cancels the one it
//!    replaces, and the order is the same one `[OCPI 2.3.0 §mod_cdrs]` gives a
//!    CDR correction: the reversal first. `Invoice::cancellation` builds the
//!    *Stornorechnung* and `postings_for` reverses its books; neither knows
//!    whether the original was ever issued.
//! 4. **Which account a role lands in.** `emob-billing` addresses a posting by
//!    what it is *for* — a receivable, energy revenue, VAT payable in a named
//!    country at a named rate — because a chart of accounts is not a domain
//!    crate's business. It is this one's, and this is the only manifest in the
//!    workspace that declares [`doubleentry`] (D181).
//!
//! # A document that was not accepted was not issued
//!
//! `[UStG §14(1)]` lets an electronic invoice be transmitted only in a
//! structured format meeting the European norm, and a German public buyer's
//! platform answers a submission rather than swallowing it. So [`Billd::issue`]
//! produces a document and a number; [`Billd::accepted`] records that the
//! recipient's platform **took** it. Until then the invoice exists, is numbered
//! — the number is spent whatever happens next, because `[UStG §14(4) Nr. 4]`
//! says *einmalig* — and is **not** in the books.
//!
//! That ordering is the whole reason posting is a separate act. A platform that
//! books on issue and submits afterwards has a trial balance that disagrees with
//! what the recipient holds, and the disagreement is invisible until somebody
//! reconciles.
//!
//! # No I/O
//!
//! Nothing here opens a socket or reads a clock. The journal is in memory and
//! persisting it is a deployment's job; every date is an argument, so two runs
//! of one closing produce one set of postings and one set of bytes.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use std::collections::BTreeMap;

use doubleentry::{
    Account, AccountId, AccountKind, AccountPath, Amount, Currency as LedgerCurrency, Description,
    Entry, EntryId, IdempotencyKey, Journal, LedgerId, Recorded,
};
use emob_billing::invoice::Invoice;
use emob_billing::postings::{Postings, Role, Side};

/// The precision every amount in the journal is held at.
///
/// Two decimal places, because every figure that reaches here has already been
/// rounded to the currency's minor unit by the document — `emob-billing` does
/// that **once, at the line**, and the books post what the document states. A
/// ledger at a finer precision would invite a posting the invoice does not show.
pub const SCALE: u8 = 2;

/// Where a document has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// Numbered and built, and not yet sent.
    Issued,
    /// The recipient's platform took it, and said where it lives.
    Accepted {
        /// When.
        at: time::Date,
        /// The reference the platform returned, when it returned one.
        reference: Option<String>,
    },
    /// The recipient's platform refused it, and said why.
    ///
    /// The number stays spent: `[UStG §14(4) Nr. 4]` issues it **once**, and a
    /// number reused after a rejection is one two documents can carry.
    Rejected {
        /// When.
        at: time::Date,
        /// What the platform said.
        reason: String,
    },
}

impl Submission {
    /// Whether the recipient has it.
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }
}

/// One issued document, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    /// The document.
    pub invoice: Invoice,
    /// Where it has got to.
    pub submission: Submission,
    /// The invoice this one cancels, for a *Stornorechnung*.
    pub cancels: Option<String>,
    /// Whether it has been booked.
    pub booked: bool,
}

/// Why a closing could not be made.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClosingError {
    /// The document itself could not be built.
    #[error(transparent)]
    Billing(#[from] emob_billing::BillingError),

    /// A period that has already been closed was closed again.
    ///
    /// Not an error about a document. A month closed twice is either the same
    /// invoice — in which case the second closing is the first one's number
    /// spent for nothing — or a correction, and a correction is a **pair** of
    /// documents in an order. [`Billd::rebill`] is the second reading, stated
    /// rather than guessed.
    #[error(
        "{period} is already closed by invoice {number}: re-closing it is either that invoice \
         again or a correction, and a correction cancels before it re-bills"
    )]
    AlreadyClosed {
        /// Which period.
        period: String,
        /// The invoice that closed it.
        number: String,
    },

    /// A correction was asked for against an invoice this service never issued.
    #[error("invoice {number} was not issued by this service, so there is nothing to cancel")]
    NotIssued {
        /// Which number.
        number: String,
    },

    /// A correction was asked for against an invoice already cancelled.
    #[error("invoice {number} has already been cancelled by {by}")]
    AlreadyCancelled {
        /// Which number.
        number: String,
        /// …and by which document.
        by: String,
    },

    /// The books refused the entry.
    #[error("the journal refused the entry for {number}: {source}")]
    Journal {
        /// Which document.
        number: String,
        /// What the journal said.
        #[source]
        source: doubleentry::JournalError,
    },

    /// A document was booked before the recipient had it.
    ///
    /// See the module documentation: a platform that books on issue and submits
    /// afterwards holds a trial balance the recipient's own records disagree
    /// with, and nothing fails until somebody reconciles.
    #[error(
        "invoice {number} has not been accepted by its recipient, so it is not a document to \
         book: a submission that was refused is money that never moved"
    )]
    NotAccepted {
        /// Which number.
        number: String,
    },

    /// A document was booked twice.
    #[error("invoice {number} is already in the books")]
    AlreadyBooked {
        /// Which number.
        number: String,
    },

    /// The document's own number is not one the journal can key an entry on.
    ///
    /// A refusal rather than a substitution: the idempotency key **is** the
    /// invoice number, and an entry keyed on anything else is one a replayed
    /// closing posts a second time.
    #[error("invoice {number} is not a number the books can key an entry on: {source}")]
    Field {
        /// Which document.
        number: String,
        /// What the ledger said.
        #[source]
        source: doubleentry::EntryFieldError,
    },

    /// An amount on the document is not one the journal can hold.
    #[error("an amount on {number} is not one the books can carry: {source}")]
    Money {
        /// Which document.
        number: String,
        /// What the ledger said.
        #[source]
        source: doubleentry::MoneyError,
    },
}

/// An amount in the currency's minor unit, refusing rather than defaulting.
///
/// Every figure that reaches here has already been rounded to the minor unit by
/// the document — `emob-billing` does that once, at the line — so the
/// multiplication is exact and the rounding below is a formality. It is a
/// refusal rather than a fallback because the fallback would be **zero**, and a
/// posting of nothing is a statement the books would balance around.
fn minor_units(amount: rust_decimal::Decimal, number: &str) -> Result<i64, ClosingError> {
    use rust_decimal::prelude::ToPrimitive as _;
    (amount * rust_decimal::Decimal::from(100))
        .round()
        .to_i64()
        .ok_or_else(|| ClosingError::Money {
            number: number.to_owned(),
            source: doubleentry::MoneyError::Overflow,
        })
}

/// How a document's number is made.
///
/// `[UStG §14(4) Nr. 4]` asks for *"eine fortlaufende Nummer mit einer oder
/// mehreren Zahlenreihen … einmalig vergeben"*: consecutive, in one or more
/// series, and issued once. **One or more series** is the part worth modelling —
/// the statute explicitly permits a separate run per year, per branch or per
/// document kind, and an operator that wants `R-2026-0001` beside `S-2026-0001`
/// is doing what the paragraph allows rather than something clever.
#[derive(Debug, Clone)]
pub struct Numbering {
    prefix: String,
    year: i32,
    width: usize,
    next: u32,
}

impl Numbering {
    /// A series, starting at one.
    #[must_use]
    pub fn series(prefix: impl Into<String>, year: i32) -> Self {
        Self {
            prefix: prefix.into(),
            year,
            width: 4,
            next: 1,
        }
    }

    /// Resume a series that has already issued numbers.
    ///
    /// A deployment reads the last number it issued out of its store and says
    /// so. Starting at one against a store that already holds `R-2026-0007`
    /// issues a number two documents carry, which is the one thing
    /// `[UStG §14(4) Nr. 4]` forbids by name.
    #[must_use]
    pub const fn resuming_after(mut self, issued: u32) -> Self {
        self.next = issued + 1;
        self
    }

    /// The next number, which is then spent.
    fn take(&mut self) -> String {
        let number = format!(
            "{}-{}-{:0width$}",
            self.prefix,
            self.year,
            self.next,
            width = self.width
        );
        self.next += 1;
        number
    }
}

/// Which account a role's postings land in.
///
/// # Why the mapping is here and nowhere else
///
/// `emob-billing` addresses a posting by what it is **for** — a receivable,
/// energy revenue, the VAT owed in a named country at a named rate — and stops.
/// That is not squeamishness: a chart of accounts is an operator's own, it
/// differs between SKR 03 and SKR 04 before it differs between companies, and a
/// domain crate that hard-coded `1400` would be wrong for every second
/// deployment while looking authoritative.
///
/// So the role is the stable name and this is the translation, with the default
/// spelled as **paths** rather than numbers — `assets:receivable`,
/// `liabilities:vat:DE:19` — because a path says what an account is for in the
/// same vocabulary the role does. A deployment that wants SKR 04 supplies its
/// own.
#[derive(Debug, Clone)]
pub struct ChartOfAccounts {
    accounts: BTreeMap<String, AccountPath>,
}

impl Default for ChartOfAccounts {
    fn default() -> Self {
        Self::new()
    }
}

impl ChartOfAccounts {
    /// The default chart: paths that say what each account is for.
    #[must_use]
    pub fn new() -> Self {
        Self {
            accounts: BTreeMap::new(),
        }
    }

    /// Map one role to an account of the operator's own choosing.
    ///
    /// # Panics
    ///
    /// Never. A path that does not parse is ignored rather than substituted,
    /// because a silently substituted account is a posting in the wrong place.
    #[must_use]
    pub fn mapping(mut self, role: &Role, path: &str) -> Self {
        if let Ok(path) = path.parse::<AccountPath>() {
            self.accounts.insert(role.to_string(), path);
        }
        self
    }

    /// The account a role posts to.
    fn path_for(&self, role: &Role) -> AccountPath {
        if let Some(path) = self.accounts.get(&role.to_string()) {
            return path.clone();
        }
        let default = match role {
            Role::Receivable => "assets:receivable".to_owned(),
            Role::EnergyRevenue => "income:energy".to_owned(),
            Role::ServiceRevenue => "income:service".to_owned(),
            // One account per authority **and** rate, because a liability has a
            // creditor: a document may owe 19 % in two countries, and an
            // operator files two returns (D270).
            Role::VatPayable {
                rate,
                place_of_supply,
            } => format!("liabilities:vat:{place_of_supply}:{rate}"),
            _ => "equity:unclassified".to_owned(),
        };
        default
            .parse()
            .unwrap_or_else(|_| unreachable!("a literal path this crate wrote"))
    }

    /// Which reporting classification an account of this role has.
    const fn kind_for(role: &Role) -> AccountKind {
        match role {
            Role::Receivable => AccountKind::Asset,
            Role::EnergyRevenue | Role::ServiceRevenue => AccountKind::Income,
            Role::VatPayable { .. } => AccountKind::Liability,
            _ => AccountKind::Equity,
        }
    }
}

/// The service: a number series, the documents it has issued, and the books.
pub struct Billd {
    numbering: Numbering,
    chart: ChartOfAccounts,
    /// Keyed by invoice number, which `[UStG §14(4) Nr. 4]` makes unique.
    issued: BTreeMap<String, Issued>,
    /// Which period each closing covered, so a month cannot be closed twice.
    closed: BTreeMap<String, String>,
    /// Which document cancels which.
    cancelled_by: BTreeMap<String, String>,
    journal: Journal<{ SCALE }>,
}

impl Billd {
    /// A service with a number series and an empty journal.
    #[must_use]
    pub fn new(ledger: &str, numbering: Numbering) -> Self {
        Self {
            numbering,
            chart: ChartOfAccounts::new(),
            issued: BTreeMap::new(),
            closed: BTreeMap::new(),
            cancelled_by: BTreeMap::new(),
            journal: Journal::new(
                LedgerId::new(ledger.to_owned())
                    .unwrap_or_else(|_| unreachable!("a ledger name this crate accepted")),
            ),
        }
    }

    /// Post to an operator's own chart of accounts rather than the default one.
    #[must_use]
    pub fn posting_to(mut self, chart: ChartOfAccounts) -> Self {
        self.chart = chart;
        self
    }

    /// The books.
    #[must_use]
    pub const fn journal(&self) -> &Journal<{ SCALE }> {
        &self.journal
    }

    /// What this service has issued, by number.
    #[must_use]
    pub fn issued(&self, number: &str) -> Option<&Issued> {
        self.issued.get(number)
    }

    /// Every document, in the order the statute issues their numbers.
    pub fn documents(&self) -> impl Iterator<Item = &Issued> {
        self.issued.values()
    }

    /// Issue the invoice that closes a period.
    ///
    /// The caller hands over a document already assembled by `emob-billing` —
    /// which is where every question about what it *says* is answered — and this
    /// gives it the number the statute asks for and records that the period is
    /// closed.
    ///
    /// # Errors
    ///
    /// [`ClosingError::AlreadyClosed`] for a period this service has closed
    /// before. See [`Self::rebill`] for the other reading of that request.
    pub fn issue(
        &mut self,
        period: &str,
        invoice: impl FnOnce(String) -> Result<Invoice, emob_billing::BillingError>,
    ) -> Result<&Issued, ClosingError> {
        if let Some(number) = self.closed.get(period) {
            return Err(ClosingError::AlreadyClosed {
                period: period.to_owned(),
                number: number.clone(),
            });
        }
        // The number is spent before the document is built, and stays spent
        // whatever happens next: `[UStG §14(4) Nr. 4]` issues one **once**, and
        // a counter that rewound on a failure would hand the next closing a
        // number an earlier document already carried.
        let number = self.numbering.take();
        let invoice = invoice(number.clone())?;

        self.closed.insert(period.to_owned(), number.clone());
        Ok(self.issued.entry(number).or_insert(Issued {
            invoice,
            submission: Submission::Issued,
            cancels: None,
            booked: false,
        }))
    }

    /// Cancel an issued invoice and re-bill the period.
    ///
    /// # The order is the correction's, not the caller's
    ///
    /// A *Stornorechnung* is a document rather than a minus sign — BT-3 = 381,
    /// BG-3 naming what it reverses, positive figures because EN 16931 carries
    /// the direction in the type — and `Invoice::cancellation` builds it from
    /// the invoice that was issued rather than from lines somebody re-derived.
    /// What no crate can know is whether the original was ever issued, which is
    /// why this is here.
    ///
    /// Both documents get their own number from the same series, and both are
    /// returned: the cancellation is not a bookkeeping detail, it is a document
    /// the recipient has to receive before the replacement makes sense.
    ///
    /// # Errors
    ///
    /// [`ClosingError::NotIssued`] for an invoice this service never issued,
    /// and [`ClosingError::AlreadyCancelled`] for one already reversed.
    pub fn rebill(
        &mut self,
        number: &str,
        cancelled_on: time::Date,
        reason: impl Into<String>,
        replacement: impl FnOnce(String) -> Result<Invoice, emob_billing::BillingError>,
    ) -> Result<(String, String), ClosingError> {
        let Some(original) = self.issued.get(number) else {
            return Err(ClosingError::NotIssued {
                number: number.to_owned(),
            });
        };
        if let Some(by) = self.cancelled_by.get(number) {
            return Err(ClosingError::AlreadyCancelled {
                number: number.to_owned(),
                by: by.clone(),
            });
        }

        let credit_number = self.numbering.take();
        // The reason travels **on the document** as a BT-22 note, not in a log
        // here. A recipient's accounts-payable clerk holding a credit note that
        // does not say why has to telephone for it, and `[UStG §14(4)]` gives
        // them nothing else on the document to read.
        let credit = original
            .invoice
            .cancellation(credit_number.clone(), cancelled_on, reason)?;
        self.issued.insert(
            credit_number.clone(),
            Issued {
                invoice: credit,
                submission: Submission::Issued,
                cancels: Some(number.to_owned()),
                booked: false,
            },
        );
        self.cancelled_by
            .insert(number.to_owned(), credit_number.clone());

        let replacement_number = self.numbering.take();
        let invoice = replacement(replacement_number.clone())?;
        self.issued.insert(
            replacement_number.clone(),
            Issued {
                invoice,
                submission: Submission::Issued,
                cancels: None,
                booked: false,
            },
        );
        Ok((credit_number, replacement_number))
    }

    /// Record that the recipient's platform **took** a document.
    ///
    /// # Errors
    ///
    /// [`ClosingError::NotIssued`] for a number this service never issued.
    pub fn accepted(
        &mut self,
        number: &str,
        at: time::Date,
        reference: Option<String>,
    ) -> Result<(), ClosingError> {
        self.submitted(number, Submission::Accepted { at, reference })
    }

    /// Record that the recipient's platform **refused** one, and what it said.
    ///
    /// # Errors
    ///
    /// [`ClosingError::NotIssued`] for a number this service never issued.
    pub fn rejected(
        &mut self,
        number: &str,
        at: time::Date,
        reason: impl Into<String>,
    ) -> Result<(), ClosingError> {
        self.submitted(
            number,
            Submission::Rejected {
                at,
                reason: reason.into(),
            },
        )
    }

    /// Post an accepted document into the journal.
    ///
    /// The postings are `emob-billing`'s, addressed by role; what happens here
    /// is the translation to accounts and the entry the journal seals. A
    /// document the recipient has not accepted is not one to book — see the
    /// module documentation.
    ///
    /// # Two different second bookings
    ///
    /// A caller that books the same number twice gets
    /// [`ClosingError::AlreadyBooked`], because that is an operator repeating
    /// themselves and the answer is to say so. A *process* that crashed between
    /// the journal's write and this flag's is a different case: the entry is
    /// keyed on the invoice number, so the replay finds the entry it already
    /// wrote and returns it with [`Recorded::is_new`] false, rather than posting
    /// the month a second time.
    ///
    /// # Errors
    ///
    /// [`ClosingError::NotIssued`], [`ClosingError::NotAccepted`],
    /// [`ClosingError::AlreadyBooked`] and [`ClosingError::Journal`].
    pub fn book(&mut self, number: &str) -> Result<Recorded, ClosingError> {
        let Some(document) = self.issued.get(number) else {
            return Err(ClosingError::NotIssued {
                number: number.to_owned(),
            });
        };
        if !document.submission.is_accepted() {
            return Err(ClosingError::NotAccepted {
                number: number.to_owned(),
            });
        }
        if document.booked {
            return Err(ClosingError::AlreadyBooked {
                number: number.to_owned(),
            });
        }

        let postings = emob_billing::postings::postings_for(&document.invoice);
        let entry = self.entry_for(&postings, number)?;
        let recorded = self
            .journal
            .record(entry)
            .map_err(|source| ClosingError::Journal {
                number: number.to_owned(),
                source,
            })?;
        if let Some(document) = self.issued.get_mut(number) {
            document.booked = true;
        }
        Ok(recorded)
    }

    /// The trial balance, for an operator that wants to see the books add up.
    ///
    /// # Errors
    ///
    /// [`doubleentry::MoneyError`] where the journal cannot total its own
    /// postings, which is a ledger that has stopped balancing.
    pub fn trial_balance(
        &self,
    ) -> Result<doubleentry::TrialBalance<{ SCALE }>, doubleentry::MoneyError> {
        self.journal.trial_balance(doubleentry::BalanceQuery::all())
    }

    /// What one account holds, in one currency.
    ///
    /// The question D270 exists to make answerable: *how much VAT do we owe in
    /// France at 20 %* is a filing, and it is a different filing from the German
    /// one at the same rate. `liabilities:vat:FR:20` is the account, and this is
    /// the figure — `None` where nothing has been posted to it, which is a
    /// different statement from a balance of zero.
    ///
    /// # Errors
    ///
    /// [`doubleentry::MoneyError`] where the journal cannot total its own
    /// postings.
    pub fn balance_of(
        &self,
        path: &str,
        currency: &str,
    ) -> Result<Option<doubleentry::Balance<{ SCALE }>>, doubleentry::MoneyError> {
        let (Ok(path), Ok(currency)) = (path.parse::<AccountPath>(), LedgerCurrency::new(currency))
        else {
            return Ok(None);
        };
        let Some(account) = self.journal.accounts().id_of(&path) else {
            return Ok(None);
        };
        Ok(self
            .trial_balance()?
            .get(&doubleentry::BalanceKey {
                account,
                currency,
                layer: doubleentry::Layer::Settled,
            })
            .copied())
    }

    /// One document's postings as a journal entry, with every account opened.
    fn entry_for(
        &mut self,
        postings: &Postings,
        number: &str,
    ) -> Result<Entry<doubleentry::Draft, { SCALE }>, ClosingError> {
        let currency = LedgerCurrency::new(postings.currency.as_str()).map_err(|source| {
            ClosingError::Money {
                number: number.to_owned(),
                source,
            }
        })?;
        // The **idempotency key** is the invoice number, which
        // `[UStG §14(4) Nr. 4]` already makes unique. The entry's own id is not
        // in the journal's canonical bytes, so a closing replayed after a crash
        // records once and returns what it recorded the first time.
        let key = IdempotencyKey::new(number.as_bytes().to_vec()).map_err(|source| {
            ClosingError::Field {
                number: number.to_owned(),
                source,
            }
        })?;
        let description = Description::new(format!("invoice {number}")).map_err(|source| {
            ClosingError::Field {
                number: number.to_owned(),
                source,
            }
        })?;
        let mut entry = Entry::<doubleentry::Draft, { SCALE }>::new(
            EntryId::generate(),
            key,
            postings.booked_on,
        )
        .with_description(description);

        for posting in &postings.postings {
            let account = self.account_for(&posting.role, postings.booked_on, number)?;
            let amount = Amount::<{ SCALE }>::from_minor(minor_units(posting.amount, number)?);
            entry = match posting.side {
                Side::Debit => entry.debit(account, amount, currency),
                Side::Credit => entry.credit(account, amount, currency),
            };
        }
        Ok(entry)
    }

    /// The account a role posts to, opened on first use.
    ///
    /// An operator's chart is a fact about the operator, and a role that turns
    /// up for the first time — the VAT of a country this node had not billed in
    /// before — opens its account rather than failing the closing. The
    /// alternative is a month that will not book because a driver charged in a
    /// new country, which is a worse answer than an account with an obvious
    /// name.
    fn account_for(
        &mut self,
        role: &Role,
        on: time::Date,
        number: &str,
    ) -> Result<AccountId, ClosingError> {
        let path = self.chart.path_for(role);
        if let Some(existing) = self.journal.accounts().id_of(&path) {
            return Ok(existing);
        }
        self.journal
            .register_account(Account::new(path, on).with_kind(ChartOfAccounts::kind_for(role)))
            .map_err(|source| ClosingError::Journal {
                number: number.to_owned(),
                source,
            })
    }

    /// Record one submission outcome.
    fn submitted(&mut self, number: &str, outcome: Submission) -> Result<(), ClosingError> {
        let Some(document) = self.issued.get_mut(number) else {
            return Err(ClosingError::NotIssued {
                number: number.to_owned(),
            });
        };
        document.submission = outcome;
        Ok(())
    }
}
