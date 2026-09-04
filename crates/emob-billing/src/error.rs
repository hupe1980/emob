//! What can stop a rated record becoming money.

use emob_core::{Currency, Money};
use rust_decimal::Decimal;

/// Everything that can go wrong between a rated CDR and an invoice, a
/// collection or a set of postings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BillingError {
    /// The record carries no price.
    ///
    /// An invoice line has to state an amount, and a CDR that was never rated
    /// has none. Inventing one by re-rating here would put a second pricing
    /// engine in the workspace, which is the drift `emob-tariff` exists to
    /// prevent.
    #[error("the record {cdr} carries no price: an unrated CDR has no invoice line")]
    NotRated {
        /// Which record.
        cdr: String,
    },

    /// Two records on one invoice are in different currencies.
    ///
    /// An EN 16931 invoice has one currency (BT-5) and every amount on it is in
    /// that currency. A month that mixes them is two invoices.
    #[error(
        "this invoice is in {invoice} and the record {cdr} is priced in {found}: an invoice states one currency"
    )]
    CurrencyMismatch {
        /// The invoice's currency.
        invoice: Currency,
        /// Which record disagrees.
        cdr: String,
        /// What it is priced in.
        found: Currency,
    },

    /// The parties and the supply have no VAT category between them.
    #[error("no VAT treatment applies: {reason}")]
    NoTaxTreatment {
        /// Which combination, and why it has no answer.
        reason: String,
    },

    /// The supply is standard-rated and no rate was stated for the country it
    /// is taxed in.
    ///
    /// Almost always the `[UStG §3g]` case: the charge points stand in one
    /// country, the reseller that buys the sessions is established in another,
    /// and the place of supply is the second. An invoice built with only the
    /// first country's rate would state a place of supply and a rate that
    /// belong to two different countries.
    ///
    /// Refused rather than defaulted, because the two silent outcomes — using
    /// the rate that was supplied, or using zero — are an invoice that
    /// over-declares its VAT and one that under-declares it.
    #[error(
        "the supply is taxed in {place_of_supply} (the charge points stand in {point_country}) and no standard VAT rate was stated for it; rates were stated for: {stated_for}"
    )]
    NoVatRate {
        /// The country the supply is taxed in.
        place_of_supply: String,
        /// The country the charge points stand in.
        point_country: String,
        /// The countries the caller did state a rate for.
        stated_for: String,
    },

    /// A date will not fit an EN 16931 date.
    ///
    /// The standard bounds the year to four digits, which every date a charging
    /// session carries satisfies — so this cannot arise for an invoice this
    /// crate built from records. It can for one deserialised from elsewhere,
    /// and substituting a date there would put a day on the document that
    /// nothing about the session supports: an issue date `BT-2` no validator
    /// objects to, or a due date `BT-9` that fell in the past.
    #[error(
        "the {what} is {date}, which EN 16931 cannot state: a document carries the day it was \
         issued and the day it falls due, and substituting one would put a date on the invoice \
         that nothing about the session supports"
    )]
    UnrepresentableDate {
        /// Which business term.
        what: String,
        /// The date that would not fit.
        date: String,
    },

    /// An amount will not fit the currency's minor unit.
    ///
    /// EN 16931 amounts are exact to two decimals and the standard refuses a
    /// third outright rather than rounding it away. Everything this crate
    /// produces is rounded to the currency's own minor unit before it gets
    /// here, so this is a figure that arrived already too precise — or one so
    /// large it does not fit the standard's own type.
    #[error("{what} is {amount}, which does not fit an EN 16931 amount in {currency}")]
    UnrepresentableAmount {
        /// Which figure.
        what: String,
        /// The amount.
        amount: Decimal,
        /// The currency it is in.
        currency: Currency,
    },

    /// The invoice has no lines.
    #[error("an invoice needs at least one line: no record was accepted")]
    NoLines,

    /// A record was superseded by a correction, and both were offered.
    ///
    /// Billing both is the double-billing `CdrLedger::live` exists to prevent,
    /// and an invoice run that reads `iter()` instead of `live()` walks into it.
    #[error("the record {cdr} is superseded by {corrector} and both were offered for billing")]
    SupersededRecord {
        /// The corrected record.
        cdr: String,
        /// The correction that replaces it.
        corrector: String,
    },

    /// A payment instruction could not be built.
    #[error("this invoice cannot be collected: {reason}")]
    NotCollectable {
        /// Why.
        reason: String,
    },

    /// Something is owed and the document does not say when.
    ///
    /// `BR-CO-25`: an invoice whose amount due is positive must carry either a
    /// payment due date (BT-9) or payment terms (BT-20). Refused here rather
    /// than left to the validator, for the reason the reverse-charge
    /// identifiers are: the fix is a fact the caller has and this crate does
    /// not, and a finding that names a rule id sends them looking for it in the
    /// standard instead.
    ///
    /// There is no default, and that is deliberate. A due date is a commercial
    /// term, and this crate reads no clock to invent one from.
    #[error(
        "this invoice asks for {amount} and says nothing about when it is due: EN 16931's \
         BR-CO-25 requires a due date or payment terms on any invoice with something owing. \
         Give it `due_on` or `payment_terms`"
    )]
    NoDueDate {
        /// What is owed.
        amount: Money,
    },
}
