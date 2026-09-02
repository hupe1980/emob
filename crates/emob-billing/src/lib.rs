//! Rated charging records, turned into money.
//!
//! # Where this sits
//!
//! Everything upstream of here answers *what happened*: a meter signed a
//! register, a chain held up, a split conserved, a tariff priced the periods the
//! split produced. `emob-cdr` ends with a record that carries its own energy and
//! its own price and can prove both.
//!
//! A record is a **claim**. An invoice is a **demand**, and it is the document a
//! tax authority reads, a partner pays against and an auditor asks for. Four
//! things have to happen between the two, and each of them is a decision rather
//! than a mapping:
//!
//! | | |
//! |---|---|
//! | [`invoice`] | the exact, unrounded amounts become figures in a currency's minor unit — **once, at the line**, because the standard's own totals are sums of the lines — and the difference is stated rather than absorbed |
//! | [`tax`] | who owes the VAT, and in which country. For a roaming settlement that is not the country the charge point stands in `[UStG §3g]`, and every platform that assumes it is has charged tax it may not charge |
//! | [`en16931`] | the document, and the **verdict on it**: 223 syntax-independent rules and a national usage specification, run before anything is sent |
//! | [`payment`] · [`postings`] | the collection, and the books |
//!
//! # What this crate does not decide
//!
//! **It does not price anything.** `emob-tariff` rates a session and
//! `emob-cdr` carries the result; a second engine here that could produce a
//! different number for the same session is precisely the drift this workspace
//! exists to make unrepresentable. `en16931`'s own `billing` adapter is
//! deliberately not enabled for the same reason.
//!
//! **It reads no clock.** Every date and instant — the issue date, the due date,
//! the collection date, the pain.008 timestamp — is an argument, so a billing
//! run replayed two years later produces the same bytes. That matters more here
//! than anywhere else in the workspace: `sepa` defaults several of those fields
//! off the system clock, and a collection file that differs between two runs of
//! one job is a file no bank reconciles.
//!
//! **It names no accounts, and it links no ledger.** [`postings`] produces
//! movements addressed by [`postings::Role`], balanced, and a service maps them
//! onto its own chart. SKR03 and SKR04 disagree about the numbers, and posting
//! into a journal needs a journal: accounts, a calendar, a policy, a database.
//! None of those can live in a crate that reads no clock — and a bookkeeping
//! engine would bring one in through the door, since a v7 identifier is
//! generated from `SystemTime::now()`. See [`postings`].
//!
//! # A month, end to end
//!
//! ```no_run
//! use emob_billing::{Counterparty, InvoiceBuilder, TaxStatus, en16931, postings};
//! use rust_decimal::Decimal;
//! use std::str::FromStr;
//! use time::macros::date;
//! # let ledger: emob_cdr::CdrLedger = unimplemented!();
//!
//! let cpo = Counterparty::new(
//!     "Stadtwerke Musterstadt GmbH",
//!     "Musterstadt",
//!     TaxStatus::business("DE", "DE123456789"),
//! );
//! // An e-mobility provider in France buys sessions to sell on, so the place of
//! // supply moves with it and the tax is the recipient's [UStG §3g].
//! let emsp = Counterparty::new(
//!     "Mobilité SAS",
//!     "Lyon",
//!     TaxStatus::reseller("FR", "FR12345678901"),
//! );
//!
//! let crossing = InvoiceBuilder::new(
//!         "R-2026-0007",
//!         date!(2026 - 07 - 01),
//!         (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
//!         cpo,
//!         emsp,
//!     )
//!     .supplied_from("DE", Decimal::from_str("19")?)
//!     // `live`, not `iter`: a correction is a new record, and summing both
//!     // bills the session twice.
//!     .ledger(&ledger)
//!     .due_on(date!(2026 - 07 - 15))
//!     .build()?;
//!
//! let invoice = crossing.value;
//! assert!(invoice.reconciles());
//!
//! // The document, and the verdict on it before anything is sent.
//! let crossed = en16931::to_en16931(&invoice, en16931::CEN_CORE)?;
//! assert!(crossed.value.is_valid(), "{:?}", crossed.value.reasons().collect::<Vec<_>>());
//!
//! // …and the books, balanced before a single account is named.
//! assert!(postings::postings_for(&invoice).balances());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::module_name_repetitions,
    reason = "`InvoiceBuilder` in `invoice` is the name the type has everywhere else"
)]

pub mod en16931;
pub mod error;
pub mod invoice;
pub mod payment;
pub mod postings;
pub mod tax;

pub use error::BillingError;
pub use invoice::{
    Contact, Counterparty, Invoice, InvoiceBuilder, InvoiceLine, PaymentDetails, TaxSubtotal,
    unit_code,
};
pub use payment::{Collection, Creditor, Mandate, PaymentError};
pub use postings::{Posting, Postings, Role, Side};
pub use tax::{TaxStatus, TaxTreatment, VatCategory, VatRates};
