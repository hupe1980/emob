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

pub mod books;
pub mod daemon;
pub mod deliver;
pub mod invoice_page;
pub mod receive;
pub mod retail;

pub use books::{Books, BooksError, Event};

use std::collections::BTreeMap;

use doubleentry::{
    AccountKind, AccountPath, Amount, Currency as LedgerCurrency, Entry, Journal, LedgerId,
    Recorded,
};
use emob_billing::Specification;
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Submission {
    /// Numbered and built, and not yet sent.
    Issued,
    /// The recipient's platform took it, and said where it lives.
    Accepted {
        /// When.
        #[serde(with = "emob_core::wire::date")]
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
        #[serde(with = "emob_core::wire::date")]
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
    /// The invoice this one **replaces**, for the second half of a re-billing.
    ///
    /// The other direction of [`Self::cancels`], and it is here because only
    /// the two together describe a correction. A *Stornorechnung* knows what it
    /// reverses and a replacement knew nothing at all, so a replacement posted
    /// while its original still stood in the books charged one month twice and
    /// the journal balanced (D307). [`Billd::book`] reads this.
    pub replaces: Option<String>,
    /// Whether it has been booked.
    pub booked: bool,
    /// The specification it was issued in, and is served and delivered in
    /// (spec 010). A decision made once, at issue: a partner whose
    /// configuration changes later does not change a document already issued.
    pub specification: Specification,
    /// XML syntax selected when this document was issued.
    pub delivery_syntax: emob_billing::Syntax,
    /// The `[[subscribers]]` its `de.emob.invoice.issued` event is owed to,
    /// fixed at issue.
    pub owed_to: Vec<String>,
    /// What each of them answered: `None` taken, `Some(reason)` refused.
    pub answered: std::collections::BTreeMap<String, Option<String>>,
}

impl Issued {
    /// The subscribers still owed its event.
    pub fn unannounced(&self) -> impl Iterator<Item = &str> {
        self.owed_to
            .iter()
            .filter(|name| !self.answered.contains_key(name.as_str()))
            .map(String::as_str)
    }
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
        source: Box<doubleentry::Error>,
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

    /// A **cancellation** was booked before the invoice it reverses.
    ///
    /// `postings_for` reverses every side of a *Stornorechnung* — same accounts,
    /// same amounts, the other way — which is right beside the entry it undoes
    /// and is a fabrication on its own. Posted alone it credits a receivable
    /// nobody debited and debits revenue nobody earned, and the journal balances
    /// perfectly: double entry balances whatever it is told, which is why
    /// nothing here fails until somebody reconciles against the recipient (D298).
    #[error(
        "invoice {number} reverses {cancels}, which is not in the books: posting a reversal on \
         its own credits a receivable nobody debited and debits revenue nobody earned — book \
         {cancels} first, or book neither"
    )]
    ReversesUnbooked {
        /// The cancellation.
        number: String,
        /// The invoice it reverses.
        cancels: String,
    },

    /// Another invoice for the same period already stands in the books.
    ///
    /// The mirror of [`Self::ReversesUnbooked`], and the half a correction was
    /// missing. That one refuses a reversal posted without its original; this
    /// refuses a second invoice for a period one is already posted for. Between
    /// them the pair is atomic in the only sense a journal can express: the
    /// books hold both sides of a correction or neither of them.
    ///
    /// A correction chain — the invoice, its *Stornorechnung*, the replacement,
    /// and however many times that repeats — holds **one** invoice in the books
    /// at a time. Posted beside its predecessor, a replacement debits the
    /// customer a second time for one month — 31.04 and 24.70 against one
    /// June — and the journal balances, because every entry it was given
    /// balanced.
    ///
    /// It is stated over the chain rather than over the one document a
    /// replacement names, because the order the calls arrive in is not the
    /// operator's to choose: a month corrected twice runs A → R1 → R2, and
    /// booking the *original* after its replacement is the same double charge
    /// arriving backwards. Both were found by the property that drives the
    /// books in every order (D307).
    #[error(
        "invoice {number} cannot be booked while {standing} stands in the books for the same \
         period: a correction chain holds one invoice at a time — book the cancellation \
         {credit} first, or book neither"
    )]
    OriginalNotReversed {
        /// The document being booked.
        number: String,
        /// The invoice in its chain that is already booked and not reversed.
        standing: String,
        /// The cancellation that would take that one back out.
        credit: String,
    },

    /// A cancellation was itself cancelled and re-billed.
    ///
    /// A *Stornorechnung* closes no period. Re-billing one issues an invoice
    /// against a month another chain already covers, and nothing downstream can
    /// tell the two apart (D307).
    #[error(
        "invoice {number} is a cancellation, which closes no period: there is nothing to \
         re-bill, and correcting one means correcting the pair it belongs to"
    )]
    CannotRebillACancellation {
        /// Which number.
        number: String,
    },

    /// A document was booked twice.
    #[error("invoice {number} is already in the books")]
    AlreadyBooked {
        /// Which number.
        number: String,
    },

    /// The recipient refused a document that is already in the books.
    ///
    /// A late refusal is ordinary — a platform acknowledges receipt and its
    /// validator rejects afterwards — and the answer to it is a correction
    /// rather than a field. Recording it in place left the books holding a
    /// document nobody accepted while [`Billd::book`] went on refusing to post
    /// one, which is the same state read two ways (D307).
    #[error(
        "invoice {number} is already in the books, so a refusal is a correction rather than a \
         submission outcome: cancel it and re-bill — the books do not un-post"
    )]
    RefusedAfterBooking {
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
        source: Box<doubleentry::Error>,
    },

    /// An amount on the document is not one the journal can hold.
    #[error("an amount on {number} is not one the books can carry: {source}")]
    Money {
        /// Which document.
        number: String,
        /// What the ledger said.
        #[source]
        source: Box<doubleentry::Error>,
    },
}

/// An amount in the currency's minor unit, refusing rather than defaulting.
///
/// Every figure that reaches here has already been rounded to the minor unit by
/// the document — `emob-billing` does that once, at the line — so the
/// multiplication is exact and the rounding below is a formality. It is a
/// refusal rather than a fallback because the fallback would be **zero**, and a
/// posting of nothing is a statement the books would balance around.
fn minor_units(amount: rust_decimal::Decimal, number: &str) -> Result<i128, ClosingError> {
    use rust_decimal::prelude::ToPrimitive as _;
    (amount * rust_decimal::Decimal::from(100))
        .round()
        .to_i128()
        .ok_or_else(|| ClosingError::Money {
            number: number.to_owned(),
            source: Box::new(doubleentry::error::AmountError::Overflow.into()),
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

    /// Advance the counter past a number the journal says was issued.
    ///
    /// The fold's half of [`Self::resuming_after`]: a replay does not re-decide
    /// which number a document got, it reads the one the document carries and
    /// makes sure nothing later can be given it again. A number this series
    /// does not recognise — a document imported from a predecessor system,
    /// under another prefix — moves nothing, because a counter that jumped to
    /// follow a foreign series would issue numbers under a prefix it does not
    /// own.
    pub(crate) fn resume_past(&mut self, number: &str) {
        let Some(tail) = number.rsplit('-').next() else {
            return;
        };
        let expected = format!("{}-{}-", self.prefix, self.year);
        if !number.starts_with(&expected) {
            return;
        }
        if let Ok(issued) = tail.parse::<u32>() {
            self.next = self.next.max(issued.saturating_add(1));
        }
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

/// A record `csmsd` delivered, and whom it is owed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// Commercial authorization attached to the signed private delivery.
    pub retail: Option<retail::Binding>,
    /// The record.
    pub cdr: emob_cdr::Cdr,
    /// Who pays it.
    pub partner: emob_core::PartyId,
    /// The closing it belongs to.
    pub period: String,
    /// The driver's receipt link, when `csmsd` sent one (spec 003).
    pub receipt: Option<String>,
}

/// The closing key of a partner's month: `<CC*PID>/<YYYY-MM>`.
#[must_use]
pub fn period_key(partner: &emob_core::PartyId, year: i32, month: time::Month) -> String {
    format!("{partner}/{year:04}-{:02}", u8::from(month))
}

/// The closing a record belongs to: the month it **ended** in, in UTC.
#[must_use]
pub fn period_of(partner: &emob_core::PartyId, ended_at: time::OffsetDateTime) -> String {
    let utc = ended_at.to_offset(time::UtcOffset::UTC);
    period_key(partner, utc.year(), utc.month())
}

/// Everything one closing needs beside the records.
#[derive(Debug, Clone)]
pub struct Closing {
    /// Who pays.
    pub partner: emob_core::PartyId,
    /// The month.
    pub year: i32,
    /// The month.
    pub month: time::Month,
    /// The invoice date.
    pub issued_on: time::Date,
    /// The operator.
    pub seller: emob_billing::Counterparty,
    /// The partner.
    pub buyer: emob_billing::Counterparty,
    /// The standard VAT rate where the energy was supplied.
    pub vat_rate: rust_decimal::Decimal,
    /// The specification the partner's documents are written in (BT-24).
    pub specification: Specification,
    /// How the partner pays (BG-16), when the estate says.
    pub payment: Option<emob_billing::PaymentDetails>,
    /// The partner's reference for our invoices (BT-10).
    pub buyer_reference: Option<String>,
}

/// The service: a number series, the documents it has issued, and the books.
pub struct Billd {
    /// Records delivered, by key.
    received: BTreeMap<String, Received>,
    retail: retail::Ledger,
    numbering: Numbering,
    delivery_syntax: emob_billing::Syntax,
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
            received: BTreeMap::new(),
            retail: retail::Ledger::default(),
            numbering,
            delivery_syntax: emob_billing::Syntax::Ubl,
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

    /// The records delivered, by key.
    pub fn received(&self) -> impl Iterator<Item = &Received> {
        self.received.values()
    }

    /// How many records were delivered — without walking them.
    #[must_use]
    pub fn received_count(&self) -> usize {
        self.received.len()
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
    #[must_use]
    pub fn documents(
        &self,
    ) -> impl DoubleEndedIterator<Item = &Issued> + ExactSizeIterator<Item = &Issued> {
        self.issued.values()
    }

    /// Owe a document's event to these subscribers.
    pub(crate) fn owe(&mut self, number: &str, to: &[String]) {
        if let Some(document) = self.issued.get_mut(number) {
            document.owed_to = to.to_vec();
        }
    }

    /// Record a subscriber's answer to a document's event.
    pub(crate) fn answer(&mut self, number: &str, to: &str, refused: Option<String>) {
        if let Some(document) = self.issued.get_mut(number) {
            document.answered.insert(to.to_owned(), refused);
        }
    }

    /// The *Stornorechnung* that cancels a document, where one does.
    #[must_use]
    pub fn cancelled_by(&self, number: &str) -> Option<&str> {
        self.cancelled_by.get(number).map(String::as_str)
    }

    /// The document that closes a period **now**.
    ///
    /// Which is not the one that closed it first: a correction replaces it, and
    /// [`ClosingError::AlreadyClosed`] names whichever is in force. Asking it
    /// without spending a number is the operator's question and the one a
    /// property can check after every call (D307).
    #[must_use]
    pub fn closes(&self, period: &str) -> Option<&Issued> {
        self.issued.get(self.closed.get(period)?)
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
        self.issue_as(period, Specification::Core, invoice)
    }

    /// [`Self::issue`], in a stated EN 16931 specification (BT-24).
    ///
    /// # Errors
    ///
    /// As [`Self::issue`].
    pub fn issue_as(
        &mut self,
        period: &str,
        specification: Specification,
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
            replaces: None,
            booked: false,
            specification,
            delivery_syntax: self.delivery_syntax,
            owed_to: Vec::new(),
            answered: std::collections::BTreeMap::new(),
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
        // A *Stornorechnung* closes no period, so there is nothing to re-bill:
        // cancelling one and issuing a "replacement" for it produces a second
        // invoice against a month another chain already covers, and the two
        // stand in the books together (D307). A credit note that is itself
        // wrong is corrected by correcting the pair it belongs to.
        if original.invoice.kind.is_credit_note() {
            return Err(ClosingError::CannotRebillACancellation {
                number: number.to_owned(),
            });
        }

        let specification = original.specification;
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
                replaces: None,
                booked: false,
                specification,
                delivery_syntax: self.delivery_syntax,
                owed_to: Vec::new(),
                answered: std::collections::BTreeMap::new(),
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
                // The other direction of the correction. Without it a
                // replacement is a document with no past, and `book` has
                // nothing to ask about the original (D307).
                replaces: Some(number.to_owned()),
                booked: false,
                specification,
                delivery_syntax: self.delivery_syntax,
                owed_to: Vec::new(),
                answered: std::collections::BTreeMap::new(),
            },
        );

        // The period is closed by the document that closes it **now**. Left
        // pointing at the original, `AlreadyClosed` named a cancelled invoice
        // as the one in force, and an operator following that name would
        // correct a document that had already been corrected (D307).
        if let Some(period) = self
            .closed
            .iter()
            .find(|(_, closed_by)| closed_by.as_str() == number)
            .map(|(period, _)| period.clone())
        {
            self.closed.insert(period, replacement_number.clone());
        }

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
    /// [`ClosingError::NotIssued`] for a number this service never issued, and
    /// [`ClosingError::RefusedAfterBooking`] for one already in the books.
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
    /// # A correction is a pair, and the books hold both sides or neither
    ///
    /// `postings_for` reverses every side of a *Stornorechnung*, which is right
    /// against the invoice it cancels and a fabrication on its own: posted alone
    /// it credits a receivable nobody debited and debits revenue nobody earned.
    /// The journal balances either way — double entry balances whatever it is
    /// told — so a cancellation whose original was never booked is refused by
    /// name. Which document cancels which is this service's fact and nothing
    /// else holds it (D298).
    ///
    /// The **replacement** is the same rule pointed the other way, and it was
    /// missing. A re-billing produces two documents, and booking only the
    /// second leaves the original standing: one June debited at 31.04 and again
    /// at 24.70, in a journal every entry of which balances. So a replacement
    /// whose original is booked and whose cancellation is not is refused too,
    /// and between the two refusals the pair is atomic in the only sense a
    /// ledger can express (D307).
    ///
    /// # Two different second bookings
    ///
    /// A caller that books the same number twice gets
    /// [`ClosingError::AlreadyBooked`], because that is an operator repeating
    /// themselves and the answer is to say so. A *process* that crashed between
    /// the journal's write and this flag's is a different case: the entry is
    /// keyed on the invoice number, so the replay finds the entry it already
    /// wrote and returns it with [`Recorded::replayed`] set, rather than posting
    /// the month a second time.
    ///
    /// # Errors
    ///
    /// [`ClosingError::NotIssued`], [`ClosingError::NotAccepted`],
    /// [`ClosingError::AlreadyBooked`], [`ClosingError::ReversesUnbooked`],
    /// [`ClosingError::OriginalNotReversed`] and [`ClosingError::Journal`].
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
        // A reversal is only a reversal beside the entry it undoes. See
        // `ClosingError::ReversesUnbooked`.
        if let Some(cancels) = &document.cancels
            && !self
                .issued
                .get(cancels)
                .is_some_and(|original| original.booked)
        {
            return Err(ClosingError::ReversesUnbooked {
                number: number.to_owned(),
                cancels: cancels.clone(),
            });
        }
        // …and the other way. One invoice per correction chain stands in the
        // books at a time. See `ClosingError::OriginalNotReversed`.
        if !document.invoice.kind.is_credit_note()
            && let Some(standing) = self.standing_in(number)
        {
            let credit = self
                .cancelled_by
                .get(&standing)
                .cloned()
                .unwrap_or_else(|| standing.clone());
            return Err(ClosingError::OriginalNotReversed {
                number: number.to_owned(),
                standing,
                credit,
            });
        }

        let postings = emob_billing::postings::postings_for(&document.invoice);
        let entry = self.entry_for(&postings, number)?;
        let recorded = self
            .journal
            .record(entry)
            .map_err(|source| ClosingError::Journal {
                number: number.to_owned(),
                source: Box::new(source),
            })?;
        if let Some(document) = self.issued.get_mut(number) {
            document.booked = true;
        }
        Ok(recorded)
    }

    /// Another invoice in this document's correction chain that is booked and
    /// whose reversal is not.
    ///
    /// The chain is a path: `rebill` refuses a document already cancelled, so
    /// each invoice is replaced at most once. Walking it from the root rather
    /// than one step back is what makes the answer independent of the order the
    /// calls arrived in.
    fn standing_in(&self, number: &str) -> Option<String> {
        let mut at = number.to_owned();
        while let Some(prior) = self.issued.get(&at).and_then(|d| d.replaces.clone()) {
            at = prior;
        }
        loop {
            if at != number
                && self.issued.get(&at).is_some_and(|d| d.booked)
                && !self
                    .cancelled_by
                    .get(&at)
                    .and_then(|credit| self.issued.get(credit))
                    .is_some_and(|note| note.booked)
            {
                return Some(at);
            }
            let next = self
                .issued
                .iter()
                .find(|(_, d)| d.replaces.as_deref() == Some(at.as_str()))
                .map(|(name, _)| name.clone());
            match next {
                Some(next) => at = next,
                None => return None,
            }
        }
    }

    /// The trial balance, for an operator that wants to see the books add up.
    ///
    /// # Errors
    ///
    /// [`doubleentry::Error`] where the journal cannot total its own
    /// postings, which is a ledger that has stopped balancing.
    pub fn trial_balance(
        &self,
    ) -> Result<doubleentry::report::TrialBalance<{ SCALE }>, doubleentry::Error> {
        self.journal
            .trial_balance(&doubleentry::BalanceQuery::all())
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
    /// [`doubleentry::Error`] where the journal cannot total its own
    /// postings.
    pub fn balance_of(
        &self,
        path: &str,
        currency: &str,
    ) -> Result<Option<doubleentry::Balance<{ SCALE }>>, doubleentry::Error> {
        let (Ok(path), Ok(currency)) = (path.parse::<AccountPath>(), LedgerCurrency::new(currency))
        else {
            return Ok(None);
        };
        Ok(self
            .trial_balance()?
            .get(&path, currency)
            .map(|row| row.balance))
    }

    /// One document's postings as a journal entry, with every account opened.
    fn entry_for(
        &mut self,
        postings: &Postings,
        number: &str,
    ) -> Result<Entry<{ SCALE }>, ClosingError> {
        let currency = LedgerCurrency::new(postings.currency.as_str()).map_err(|source| {
            ClosingError::Money {
                number: number.to_owned(),
                source: Box::new(source.into()),
            }
        })?;
        let opened = self.calendar_through(postings.booked_on, number)?;
        self.journal
            .declare_currency(currency, SCALE, opened)
            .map_err(|source| ClosingError::Journal {
                number: number.to_owned(),
                source: Box::new(source),
            })?;
        // The **idempotency key** is the invoice number, which
        // `[UStG §14(4) Nr. 4]` already makes unique. The entry's id is derived
        // from the ledger and the key, so a closing replayed after a crash
        // records once and returns what it recorded the first time.
        let key = doubleentry::IdempotencyKey::try_from(number).map_err(|source| {
            ClosingError::Field {
                number: number.to_owned(),
                source: Box::new(source.into()),
            }
        })?;
        // No memo: the key already names the document, and an unsalted memo is
        // salted from the OS random source, which would make two replays of one
        // journal two different ledgers.
        let mut entry = Entry::<{ SCALE }>::new(key, postings.booked_on);

        for posting in &postings.postings {
            let account = self.account_for(&posting.role, opened, number)?;
            let amount = Amount::<{ SCALE }>::from_minor(minor_units(posting.amount, number)?)
                .map_err(|source| ClosingError::Money {
                    number: number.to_owned(),
                    source: Box::new(source.into()),
                })?;
            entry = match posting.side {
                Side::Debit => entry.debit(account, amount, currency),
                Side::Credit => entry.credit(account, amount, currency),
            };
        }
        Ok(entry)
    }

    /// The month calendar, extended to cover `on`; returns the day it opens.
    ///
    /// The journal books only into a defined period, and periods are defined
    /// back to back. billd closes months, so a period is a month: the first
    /// booking opens the calendar at January of its year — a correction dated
    /// earlier in the same year still has a period to land in — and every
    /// later booking extends it forward to its own month. A booking dated
    /// before the calendar opened is refused by the journal (`[300]`), which
    /// is a document claiming a date before these books existed.
    fn calendar_through(
        &mut self,
        on: time::Date,
        number: &str,
    ) -> Result<time::Date, ClosingError> {
        let journal_error = |source| ClosingError::Journal {
            number: number.to_owned(),
            source: Box::new(source),
        };
        let periods = self.journal.periods().map_err(journal_error)?;
        let (Some(first), Some(last)) = (periods.first(), periods.last()) else {
            self.journal
                .define_months(on.year(), 1..=u8::from(on.month()))
                .map_err(journal_error)?;
            return Ok(
                time::Date::from_calendar_date(on.year(), time::Month::January, 1).unwrap_or(on),
            );
        };
        let opened = first.start;
        let mut next = last.end.next_day();
        while let Some(start) = next.filter(|start| *start <= on) {
            self.journal
                .define_months(
                    start.year(),
                    u8::from(start.month())..=u8::from(start.month()),
                )
                .map_err(journal_error)?;
            next = time::Date::from_calendar_date(start.year(), start.month(), 1)
                .ok()
                .and_then(|month| month.checked_add(time::Duration::days(31)))
                .and_then(|later| {
                    time::Date::from_calendar_date(later.year(), later.month(), 1).ok()
                });
        }
        Ok(opened)
    }

    /// The account a role posts to, opened on first use.
    ///
    /// An operator's chart is a fact about the operator, and a role that turns
    /// up for the first time — the VAT of a country this node had not billed in
    /// before — opens its account rather than failing the closing. The
    /// alternative is a month that will not book because a driver charged in a
    /// new country, which is a worse answer than an account with an obvious
    /// name. Opening is idempotent in the journal, so a role already open
    /// writes nothing.
    fn account_for(
        &mut self,
        role: &Role,
        on: time::Date,
        number: &str,
    ) -> Result<AccountPath, ClosingError> {
        let path = self.chart.path_for(role);
        self.journal
            .open(path.to_string(), ChartOfAccounts::kind_for(role), on)
            .map_err(|source| ClosingError::Journal {
                number: number.to_owned(),
                source: Box::new(source),
            })?;
        Ok(path)
    }

    /// Record one submission outcome.
    ///
    /// A refusal that arrives **after** the document is in the books is not a
    /// field to overwrite. `book` refuses a document the recipient has not
    /// accepted, and the answer to a late rejection is a correction: the books
    /// do not un-post. Overwriting the field left a journal holding a document
    /// the recipient had refused, with `book` still saying it may not have been
    /// posted and the entry already there (D307).
    fn submitted(&mut self, number: &str, outcome: Submission) -> Result<(), ClosingError> {
        let Some(document) = self.issued.get_mut(number) else {
            return Err(ClosingError::NotIssued {
                number: number.to_owned(),
            });
        };
        if document.booked && !outcome.is_accepted() {
            return Err(ClosingError::RefusedAfterBooking {
                number: number.to_owned(),
            });
        }
        document.submission = outcome;
        Ok(())
    }
}
