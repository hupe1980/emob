//! The invoice: a month of rated records, as one document that adds up.
//!
//! # The one decision this module exists to make
//!
//! A rated CDR carries **exact, unrounded** amounts. `29.500 kWh × 0.49` is
//! `14.45500`, and `emob-tariff` keeps every digit of it on purpose: rounding
//! per line and then summing gives a different total from summing and then
//! rounding, and which of the two is correct is a tax question rather than an
//! arithmetic one, so the exact figures survive and the layer that has to answer
//! the tax question does the rounding.
//!
//! **This is that layer.** An invoice amount is a number in a currency's minor
//! unit — EN 16931 refuses a third decimal outright rather than rounding it away
//! — so somewhere between the meter and the document a number gets rounded, and
//! the choice decides three things a recipient will check.
//!
//! ## Which basis
//!
//! EN 16931's line amount (BT-131) is a **net** figure, always — and so is its
//! item price, BT-146, which is defined *exclusive of VAT*. A tariff quoted
//! gross therefore has both converted, at the same rate, before anything is
//! rounded: a document whose price does not reproduce its own line amount is one
//! a Peppol access point returns (`PEPPOL-EN16931-R120`), and a partner
//! multiplying the two columns simply reads it as wrong.
//!
//! ## In which unit
//!
//! OCPI quotes time per **hour** and 3600 has two factors of three, so
//! twenty-five minutes is `0.41666…` h and a line whose quantity is rounded no
//! longer reproduces its own amount. The standard has the field for it — BT-149,
//! the item price base quantity — so a time line is `1500 SEC` at
//! `6.00 EUR per 3600 SEC` and `BT-131 = BT-129 × BT-146 ÷ BT-149` holds to the
//! last digit.
//!
//! ## Where
//!
//! **Per line, and nowhere else.** The standard's own totals are sums of the
//! line amounts: `BT-106 = Σ BT-131` (`BR-CO-10`), and per category
//! `BT-116 = Σ BT-131` (`BR-S-08` and its eight siblings). Both are sums of
//! *rounded* figures, so the rounding has to happen at the line or the document
//! cannot satisfy both at once. Rounding per category and apportioning back to
//! lines produces an invoice whose own lines do not add up to its own subtotals,
//! which is the one thing every validator in this space checks first.
//!
//! ## And what the difference is
//!
//! Rounding at the line means the invoice's taxable amount need not equal what
//! the records came to exactly. It is at most a minor unit per line and it is
//! real money, so it is neither hidden nor silently absorbed:
//! [`Invoice::rounding_residual`] is the difference,
//! [`Invoice::exact_taxable_total`] is what the records came to, and the
//! [`Crossing`] the builder returns names each record the document does not
//! reproduce, by JSON Pointer into the invoice.
//!
//! The tax follows from the *rounded* taxable amount, by the standard's own
//! rule, so that residual is the whole of what this document approximates.
//!
//! The invoice is the authoritative figure — it is the document the tax office
//! and the partner both read — and the record is the claim it was built from.
//! Saying which is which, in the document, is the whole of it.
//!
//! ## …and a bound is not a line at all
//!
//! `[OCPI 2.3.0 §Tariff]`'s `min_price` and `max_price` move a session's total
//! without changing what was delivered, and a maximum moves it **down**. Put on
//! the document as a line, a cap is a line with a negative amount and a negative
//! BT-146 — which `BR-27` refuses outright, so the whole invoice is invalid.
//! EN 16931 models exactly this as a document level allowance or charge
//! (BG-20/BG-21), whose amount is a positive magnitude and which the totals
//! chain subtracts or adds: `BT-109 = BT-106 − BT-107 + BT-108` (`BR-CO-13`).
//! See [`DocumentAdjustment`] — including why the amount is derived from what
//! the document states rather than from the exact difference, and why a bound
//! with no lines to adjust is the line.
//!
//! # One line per price, not one per session
//!
//! `emob-tariff` yields one [`emob_tariff::Line`] per *distinct price* that
//! applied, so a
//! tiered session comes out as two energy lines at two prices — "which is what a
//! tiered invoice has to show". This module keeps that shape rather than summing
//! it away: an invoice line here is one rated line of one record, and it names
//! the session, the point and the window it came from. A driver looking for the
//! charge they remember finds it; a partner disputing one finds the same row.

use emob_cdr::{Cdr, CdrKey, CdrLedger};
use emob_core::{Crossing, Currency, Money};
use emob_tariff::{Dimension, Rated};
use rust_decimal::Decimal;

use crate::error::BillingError;
use crate::tax::{TaxStatus, TaxTreatment, VatCategory, VatRates};

/// Percent, as a decimal.
const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

/// A party on an invoice.
///
/// The postal fields EN 16931 makes mandatory (`BR-08`…`BR-11`: a city, a post
/// code and a country on both sides) plus the tax facts. Everything optional in
/// the standard stays optional here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Counterparty {
    /// The legal name — BT-27 for the seller, BT-44 for the buyer.
    pub name: String,
    /// The street line, when there is one.
    pub street: Option<String>,
    /// The city. Mandatory for both parties.
    pub city: String,
    /// The post code.
    pub post_code: Option<String>,
    /// The ISO 3166-1 alpha-2 country code. Mandatory for both parties.
    pub country: String,
    /// The electronic address and its EAS scheme — BT-34/BT-49, which
    /// `XRechnung`'s `BR-DE-*` make mandatory and the CEN core does not.
    pub electronic_address: Option<(String, String)>,
    /// The legal registration identifier and its scheme — BT-30 for the seller,
    /// BT-47 for the buyer. A German operator's `HRB` entry.
    ///
    /// Optional in general and **the only way to identify a seller** on the one
    /// document that may not carry a VAT identifier. `BR-CO-26` requires a
    /// buyer to be able to identify its supplier from BT-29, BT-30 **or**
    /// BT-31, and `BR-O-02` forbids BT-31 outright on an outside-scope invoice
    /// — so a settlement with a reseller established outside the Union is
    /// exactly the case where this field is not optional at all. See
    /// [`Counterparty::registered_as`].
    pub legal_registration: Option<(String, Option<String>)>,
    /// A person to ask — BG-6 / BG-9, and its three terms.
    ///
    /// Optional in the CEN core and **mandatory on the seller** in `XRechnung`:
    /// `BR-DE-2` requires the group and `BR-DE-5`…`-7` require all three of a
    /// name, a telephone number and an email address. A German public buyer
    /// rejects the document without them.
    pub contact: Option<Contact>,
    /// What this party is, for tax.
    pub tax: TaxStatus,
}

/// A person to ask about an invoice — BG-6 / BG-9.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Contact {
    /// BT-41 / BT-56 — the contact point.
    pub name: String,
    /// BT-42 / BT-57 — a telephone number.
    pub phone: String,
    /// BT-43 / BT-58 — an email address.
    pub email: String,
}

impl Counterparty {
    /// A party with the three fields EN 16931 requires of every one.
    #[must_use]
    pub fn new(name: impl Into<String>, city: impl Into<String>, tax: TaxStatus) -> Self {
        Self {
            name: name.into(),
            street: None,
            city: city.into(),
            country: tax.country.to_ascii_uppercase(),
            post_code: None,
            electronic_address: None,
            legal_registration: None,
            contact: None,
            tax,
        }
    }

    /// The same party, with a street and a post code.
    #[must_use]
    pub fn at(mut self, street: impl Into<String>, post_code: impl Into<String>) -> Self {
        self.street = Some(street.into());
        self.post_code = Some(post_code.into());
        self
    }

    /// The same party, reachable at an electronic address under an EAS scheme.
    #[must_use]
    pub fn reachable_at(mut self, address: impl Into<String>, scheme: impl Into<String>) -> Self {
        self.electronic_address = Some((address.into(), scheme.into()));
        self
    }

    /// The same party, with the legal registration identifier BT-30 / BT-47
    /// carries — a German operator's `HRB` entry, optionally under a scheme.
    ///
    /// Worth setting on every invoice and **required** on one: an outside-scope
    /// settlement may state no VAT identifier (`BR-O-02`), and `BR-CO-26` still
    /// wants the buyer to be able to identify its supplier.
    #[must_use]
    pub fn registered_as(mut self, identifier: impl Into<String>, scheme: Option<String>) -> Self {
        self.legal_registration = Some((identifier.into(), scheme));
        self
    }

    /// The same party, with the contact `BR-DE-2` requires of a seller.
    ///
    /// All three terms together, because `BR-DE-5`, `-6` and `-7` require all
    /// three and a group with two of them is a document that comes back.
    #[must_use]
    pub fn contactable(
        mut self,
        name: impl Into<String>,
        phone: impl Into<String>,
        email: impl Into<String>,
    ) -> Self {
        self.contact = Some(Contact {
            name: name.into(),
            phone: phone.into(),
            email: email.into(),
        });
        self
    }
}

/// How an invoice will be paid — BG-16, and the one sub-group it carries.
///
/// Mandatory in `XRechnung` (`BR-DE-1`) and optional in the CEN core, which is the
/// ordinary shape of the German profile: the standard allows an invoice that
/// does not say how to pay it and a German buyer does not.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PaymentDetails {
    /// BG-17 — the buyer transfers to this account. UNCL 4461 code 58, SEPA
    /// credit transfer.
    CreditTransfer {
        /// BT-84 — the IBAN the money goes to.
        iban: String,
        /// BT-85 — the account name.
        holder: Option<String>,
    },
    /// BG-19 — the seller collects. UNCL 4461 code 59, SEPA direct debit.
    ///
    /// The three terms `BR-DE-30`, `BR-DE-31` and `PEPPOL-EN16931-R061` require,
    /// and the same three a pain.008 carries — so an invoice that states a
    /// collection and a collection that draws it cannot disagree about the
    /// mandate.
    DirectDebit {
        /// BT-89 — the mandate the debtor signed.
        mandate_reference: String,
        /// BT-90 — the creditor identifier the collection is made under.
        creditor_identifier: String,
        /// BT-91 — the account to be debited.
        debited_iban: String,
    },
}

impl PaymentDetails {
    /// The UNCL 4461 code — BT-81.
    #[must_use]
    pub const fn means_code(&self) -> &'static str {
        match self {
            Self::CreditTransfer { .. } => "58",
            Self::DirectDebit { .. } => "59",
        }
    }
}

/// One line of an invoice: one dimension of one record at one price.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct InvoiceLine {
    /// The line's identifier on the document — BT-126. `"3.2"` is the second
    /// priced dimension of the third record.
    pub id: String,
    /// Which record this came from.
    pub cdr: CdrKey,
    /// What it charges for.
    pub dimension: Dimension,
    /// A one-line description of the session, for a human reading the invoice.
    pub description: String,
    /// The window the record covers.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub started_at: time::OffsetDateTime,
    /// …and where it ends.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub ended_at: time::OffsetDateTime,
    /// How much, in the line's own unit — BT-129: kilowatt-hours, **whole
    /// seconds** for the two time dimensions, one for a session fee.
    ///
    /// Seconds rather than hours, because a duration in hours is usually not a
    /// decimal — 3600 has two factors of three — and a line whose quantity is
    /// rounded no longer reproduces its own amount. The price stays per hour,
    /// and the standard has the field that reconciles the two: [`Self::base_quantity`].
    pub quantity: Decimal,
    /// The item **net** price — BT-146: the price of one unit *excluding VAT*,
    /// in the unit the tariff quotes (per kWh, per **hour**, per session).
    ///
    /// Not the tariff's own figure where that is gross. BT-146 is defined
    /// exclusive of VAT, so a tariff quoting `0.49` gross at 19 % states
    /// `0.411764…` here — stripped at the same rate the line's amount is, so
    /// the two cannot disagree, and carried at full precision because the
    /// standard caps the field at no scale and rounding it is what breaks
    /// `BT-131 = BT-129 × BT-146 ÷ BT-149`.
    pub unit_price: Decimal,
    /// How many of [`Self::quantity`]'s units one [`Self::unit_price`] buys —
    /// BT-149, the item price base quantity, in the same unit code (BT-150).
    ///
    /// `3600` for a time line — "6.00 EUR per 3600 SEC" — and `1` otherwise.
    /// This is what lets `BT-131 = BT-129 × BT-146 ÷ BT-149` hold exactly for
    /// twenty-five minutes at six euros an hour, which no quantity in hours can
    /// do: `1500 × 6.00 ÷ 3600` is `2.50`, and `0.41666… × 6.00` is not.
    pub base_quantity: Decimal,
    /// The rate this line is taxed at — BT-152, and `None` where the category
    /// states none.
    ///
    /// The **component's** own rate where the tariff states one, because
    /// electricity and a service fee can sit in different categories and
    /// EN 16931 wants the taxable amount per rate. Where the component states
    /// none, the supply's rate: a gross price with no VAT on the component is
    /// the commonest tariff shape, and reading it as zero would grow the gross
    /// the driver was quoted the moment the invoice added the supply's tax.
    ///
    /// Zero under a category that does not levy tax but does state a rate —
    /// `BR-AE-5` and its siblings require exactly that, because under a reverse
    /// charge the tax is the recipient's rather than absent.
    ///
    /// **`None` under `O`**, which is the only category in UNCL 5305 that states
    /// no rate at all. `BR-O-05` refuses a line carrying BT-152, and a rate of
    /// zero is carrying it — so the two absences are two values here rather than
    /// one, because they are two different statements and an invoice that makes
    /// the wrong one comes back (D183).
    pub vat_rate: Option<Decimal>,
    /// The line's **net** amount, rounded to the currency's minor unit —
    /// BT-131.
    pub net: Decimal,
    /// What the rated line came to, exactly, in the invoice's basis, before
    /// this line rounded it.
    ///
    /// Kept because the difference between the two is the residual, and a
    /// residual whose two terms are not both on the document is a number nobody
    /// can check.
    pub exact_net: Decimal,
}

impl InvoiceLine {
    /// The UN/ECE Recommendation 20 code the quantity is measured in — BT-130.
    ///
    /// Derived from the dimension rather than stored beside it, for the reason
    /// `emob_tariff::DisplayLine::unit` is: a unit field and a dimension field
    /// are two statements about one thing and can be made to disagree.
    #[must_use]
    pub const fn unit_code(&self) -> &'static str {
        unit_code(self.dimension)
    }

    /// Whether the line's own numbers reproduce its exact net —
    /// `BT-129 × BT-146 ÷ BT-149`, the identity `BT-131` is derived from
    /// (`PEPPOL-EN16931-R120`), before the document rounds it to the minor
    /// unit.
    ///
    /// True by construction, and re-checkable because an [`Invoice`] can be
    /// deserialised from somewhere this crate did not build it —
    /// [`Invoice::reconciles`] asks it of every line. EN 16931 itself has **no**
    /// rule tying BT-131 to quantity × price; Peppol and `XRechnung` do, at a
    /// tolerance of two minor units, and a document that fails it is one an
    /// access point returns.
    #[must_use]
    pub fn reconciles(&self) -> bool {
        self.quantity * self.unit_price / self.base_quantity == self.exact_net
    }
}

/// Whether a document-level adjustment reduces or increases the taxable
/// amount — BG-20 or BG-21.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DocumentAdjustmentKind {
    /// BG-20 — a document level **allowance**, which reduces BT-109.
    Allowance,
    /// BG-21 — a document level **charge**, which increases it.
    Charge,
}

impl DocumentAdjustmentKind {
    /// The sign this side contributes to a taxable amount — and to the revenue
    /// the books credit, which is the same figure.
    #[must_use]
    pub const fn sign(self) -> Decimal {
        match self {
            Self::Allowance => Decimal::NEGATIVE_ONE,
            Self::Charge => Decimal::ONE,
        }
    }
}

/// A tariff's minimum or maximum, as the document states it — BG-20 / BG-21.
///
/// # Why a bound is not a line
///
/// `[OCPI 2.3.0 §Tariff]`'s `min_price` and `max_price` move the session's
/// total without changing what was delivered, and a cap moves it **down**. Put
/// on the document as an invoice line, a cap is a line with a negative amount
/// and a negative BT-146 — which `BR-27` refuses outright, so the whole invoice
/// is invalid. EN 16931 models exactly this as a document level allowance, whose
/// amount is stated as a **positive magnitude** and subtracted by the totals
/// chain (`BR-CO-11`, `BR-CO-13`).
///
/// # And the amount is derived from what the document states
///
/// A cap says the session costs at most €10.00. Rounding the lines and the
/// exact cap difference independently and subtracting one from the other misses
/// that by a cent — the invoice then demands €10.01 for a session the tariff
/// capped at ten euros, which is the one number the driver was promised. So the
/// amount is the difference between what this record's lines state on the
/// document and what the bound says the record comes to, both rounded to the
/// minor unit: the document reaches the tariff's own figure exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DocumentAdjustment {
    /// Which side of the totals chain it sits on.
    pub kind: DocumentAdjustmentKind,
    /// Which record's bound this is.
    pub cdr: CdrKey,
    /// BT-92 / BT-99 — the amount, always a **positive** magnitude.
    pub amount: Decimal,
    /// What it came to exactly, before the document rounded it.
    pub exact_amount: Decimal,
    /// BT-96 / BT-103 — the rate it is taxed at, absent under the one category
    /// that states none. See [`InvoiceLine::vat_rate`].
    pub vat_rate: Option<Decimal>,
    /// BT-97 / BT-104 — the reason, in words.
    pub reason: String,
}

/// The UN/ECE Recommendation 20 unit code a dimension is measured in.
///
/// `KWH` for energy and `C62` — "one", a dimensionless count — for a session
/// fee, which is the code the standard's own examples use for anything billed
/// per occurrence. The two time dimensions are `SEC`, not `HUR`: a duration in
/// hours is usually not a decimal, and a quantity in whole seconds against a
/// price per 3600 of them ([`InvoiceLine::base_quantity`]) reproduces its line
/// exactly.
#[must_use]
pub const fn unit_code(dimension: Dimension) -> &'static str {
    match dimension {
        Dimension::Energy => "KWH",
        Dimension::Time | Dimension::ParkingTime => "SEC",
        Dimension::Flat => "C62",
    }
}

/// One VAT category of an invoice — BG-23.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TaxSubtotal {
    /// The category — BT-118.
    pub category: VatCategory,
    /// The rate — BT-119, and `None` under the one category that states none.
    ///
    /// See [`InvoiceLine::vat_rate`]: `BR-O-05` and its breakdown sibling refuse
    /// a rate under `O`, and zero is a rate.
    pub rate: Option<Decimal>,
    /// The taxable amount: the sum of the lines in this category — BT-116.
    pub taxable: Decimal,
    /// The tax on it — BT-117.
    pub tax: Decimal,
}

/// An invoice for a period.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Invoice {
    /// The invoice number — BT-1.
    pub number: String,
    /// The day it is issued — BT-2.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub issued_on: time::Date,
    /// The day it is due — BT-9.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date::option"))]
    pub due_on: Option<time::Date>,
    /// The buyer's own reference — BT-10.
    ///
    /// A German public buyer's Leitweg-ID, which `BR-DE-15` makes mandatory.
    pub buyer_reference: Option<String>,
    /// How the invoice will be paid — BG-16.
    pub payment: Option<PaymentDetails>,
    /// The payment terms in words — BT-20.
    ///
    /// The other half of `BR-CO-25`: an invoice with something owing has to say
    /// when, and the standard accepts either a date or a sentence. A caller
    /// whose terms are "30 days net from receipt" has no date to give.
    pub payment_terms: Option<String>,
    /// The period it covers — BG-14.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub period_from: time::Date,
    /// …and its end.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub period_to: time::Date,
    /// Who is billing.
    pub seller: Counterparty,
    /// Who is billed.
    pub buyer: Counterparty,
    /// The one currency every amount is in — BT-5.
    pub currency: Currency,
    /// The tax treatment every line carries, and why.
    pub treatment: TaxTreatment,
    /// The lines — BG-25.
    pub lines: Vec<InvoiceLine>,
    /// The document level allowances and charges — BG-20 and BG-21.
    ///
    /// One per record whose tariff bound moved its total. See
    /// [`DocumentAdjustment`] for why a cap cannot be a line.
    pub adjustments: Vec<DocumentAdjustment>,
    /// The VAT breakdown — BG-23. Derived from the lines.
    pub tax: Vec<TaxSubtotal>,
    /// What the lines came to **exactly**, before each was rounded to the
    /// currency's minor unit.
    ///
    /// The other end of the one approximation this document makes. Kept on the
    /// invoice rather than recomputed, because a residual whose two terms are
    /// not both on the document is a number nobody can check.
    pub exact_taxable: Decimal,
}

impl Invoice {
    /// The sum of the line amounts — BT-106.
    #[must_use]
    pub fn line_total(&self) -> Money {
        // Every term is already a minor-unit figure; the rounding only sets the
        // scale, so a document total reads `0.00 EUR` rather than `0 EUR`.
        Money::new(self.lines.iter().map(|line| line.net).sum(), self.currency)
            .round_to_minor_unit()
    }

    /// The sum of the document level allowances — BT-107, `BR-CO-11`.
    #[must_use]
    pub fn allowance_total(&self) -> Money {
        Money::new(
            self.sum_of(DocumentAdjustmentKind::Allowance),
            self.currency,
        )
    }

    /// The sum of the document level charges — BT-108, `BR-CO-12`.
    #[must_use]
    pub fn charge_total(&self) -> Money {
        Money::new(self.sum_of(DocumentAdjustmentKind::Charge), self.currency)
    }

    fn sum_of(&self, kind: DocumentAdjustmentKind) -> Decimal {
        Money::new(
            self.adjustments
                .iter()
                .filter(|adjustment| adjustment.kind == kind)
                .map(|adjustment| adjustment.amount)
                .sum(),
            self.currency,
        )
        .round_to_minor_unit()
        .amount()
    }

    /// The taxable amount — BT-109: `BT-106 − BT-107 + BT-108` (`BR-CO-13`).
    #[must_use]
    pub fn taxable_total(&self) -> Money {
        Money::new(
            self.line_total().amount() - self.allowance_total().amount()
                + self.charge_total().amount(),
            self.currency,
        )
        .round_to_minor_unit()
    }

    /// The tax across every category — BT-110.
    #[must_use]
    pub fn tax_total(&self) -> Money {
        Money::new(self.tax.iter().map(|t| t.tax).sum(), self.currency).round_to_minor_unit()
    }

    /// What the buyer pays — BT-112, and BT-115 because nothing is prepaid.
    #[must_use]
    pub fn gross_total(&self) -> Money {
        Money::new(
            self.taxable_total().amount() + self.tax_total().amount(),
            self.currency,
        )
        .round_to_minor_unit()
    }

    /// What the lines came to exactly, before the document rounded them.
    #[must_use]
    pub const fn exact_taxable_total(&self) -> Money {
        Money::new(self.exact_taxable, self.currency)
    }

    /// The difference the line-level rounding made to the taxable amount.
    ///
    /// The **whole** of what this document approximates: the tax is computed
    /// from the rounded taxable amount by the standard's own rule, so it adds
    /// nothing further. Zero on almost every invoice, and never more than a
    /// minor unit per line.
    ///
    /// It is not an error and it is not hidden. The document is authoritative —
    /// it is what the tax office and the partner both read — and the records are
    /// what it was built from, so the gap between them is a figure a
    /// reconciliation needs rather than one it has to discover.
    #[must_use]
    pub fn rounding_residual(&self) -> Money {
        Money::new(
            self.taxable_total().amount() - self.exact_taxable,
            self.currency,
        )
    }

    /// Whether the invoice's own numbers add up, at every level the standard
    /// states one:
    ///
    /// - every line reproduces its own amount from its own quantity and price
    ///   ([`InvoiceLine::reconciles`], `PEPPOL-EN16931-R120`);
    /// - the VAT breakdown's taxable amounts sum to BT-109 (`BR-CO-13` with
    ///   `BR-S-08` and its siblings);
    /// - and the payable total is the taxable amount plus the tax
    ///   (`BR-CO-15`).
    ///
    /// True by construction, and re-checkable, because an [`Invoice`] can be
    /// deserialised from somewhere this crate did not build it.
    #[must_use]
    pub fn reconciles(&self) -> bool {
        let by_category: Decimal = self.tax.iter().map(|t| t.taxable).sum();
        self.lines.iter().all(InvoiceLine::reconciles)
            && by_category == self.taxable_total().amount()
            && self.gross_total().amount()
                == self.taxable_total().amount() + self.tax_total().amount()
    }

    /// The records this invoice bills, in line order and without repeats.
    #[must_use]
    pub fn records(&self) -> Vec<&CdrKey> {
        let mut out: Vec<&CdrKey> = Vec::new();
        for line in &self.lines {
            if !out.contains(&&line.cdr) {
                out.push(&line.cdr);
            }
        }
        out
    }
}

/// Assemble an invoice from rated records.
///
/// # Errors
///
/// [`BillingError`] when a record carries no price, when two records are in
/// different currencies, when the parties have no VAT treatment between them,
/// or when nothing was accepted.
pub struct InvoiceBuilder<'a> {
    number: String,
    issued_on: time::Date,
    due_on: Option<time::Date>,
    payment_terms: Option<String>,
    buyer_reference: Option<String>,
    payment: Option<PaymentDetails>,
    period: (time::Date, time::Date),
    seller: Counterparty,
    buyer: Counterparty,
    treatment: Option<TaxTreatment>,
    point_country: String,
    rates: VatRates,
    records: Vec<&'a Cdr>,
}

impl<'a> InvoiceBuilder<'a> {
    /// Start an invoice.
    ///
    /// `period` is the window the invoice covers — BG-14 — and it is stated
    /// rather than derived from the records, because a month with no sessions in
    /// its last week still ends when the month does.
    #[must_use]
    pub fn new(
        number: impl Into<String>,
        issued_on: time::Date,
        period: (time::Date, time::Date),
        seller: Counterparty,
        buyer: Counterparty,
    ) -> Self {
        let point_country = seller.country.clone();
        Self {
            number: number.into(),
            issued_on,
            due_on: None,
            payment_terms: None,
            buyer_reference: None,
            payment: None,
            period,
            seller,
            buyer,
            treatment: None,
            point_country,
            rates: VatRates::new(),
            records: Vec::new(),
        }
    }

    /// When payment is due — BT-9.
    #[must_use]
    pub const fn due_on(mut self, date: time::Date) -> Self {
        self.due_on = Some(date);
        self
    }

    /// The payment terms in words — BT-20.
    ///
    /// The alternative `BR-CO-25` accepts, for a caller whose terms are a
    /// sentence rather than a date.
    #[must_use]
    pub fn payment_terms(mut self, terms: impl Into<String>) -> Self {
        self.payment_terms = Some(terms.into());
        self
    }

    /// The buyer's own reference — BT-10, a Leitweg-ID for a German public
    /// buyer.
    #[must_use]
    pub fn buyer_reference(mut self, reference: impl Into<String>) -> Self {
        self.buyer_reference = Some(reference.into());
        self
    }

    /// How the invoice will be paid — BG-16.
    #[must_use]
    pub fn paid_by(mut self, payment: PaymentDetails) -> Self {
        self.payment = Some(payment);
        self
    }

    /// The country the charge points stand in, and the standard VAT rate in
    /// force there.
    ///
    /// Defaults to the seller's own country with **no** rate stated, which
    /// builds a reverse-charge or outside-scope invoice and refuses a
    /// standard-rated one — because a rate nobody supplied is not zero, and an
    /// invoice that silently under-declares its VAT is worse than one that will
    /// not build.
    ///
    /// The rate is an argument for the reason every instant in this workspace
    /// is: rates move, and an invoice replayed two years later has to reproduce
    /// the rate that was in force rather than today's.
    ///
    /// # …and this is not always the rate the invoice carries
    ///
    /// `[UStG §3g]` taxes a supply to a reseller where the **reseller** is
    /// established, so an operator whose points stand in one country and whose
    /// roaming partner is established in another needs the second country's
    /// rate. State it with [`Self::vat_rate_in`]; the builder picks whichever
    /// belongs to the place of supply it derives.
    #[must_use]
    pub fn supplied_from(mut self, country: impl Into<String>, standard_rate: Decimal) -> Self {
        let country = country.into();
        self.rates = std::mem::take(&mut self.rates).at(&country, standard_rate);
        self.point_country = country;
        self
    }

    /// Another country's standard rate, for a place of supply `[UStG §3g]`
    /// moves away from the charge point.
    #[must_use]
    pub fn vat_rate_in(mut self, country: impl AsRef<str>, standard_rate: Decimal) -> Self {
        self.rates = std::mem::take(&mut self.rates).at(country, standard_rate);
        self
    }

    /// State the tax treatment outright instead of deriving it from the
    /// parties.
    #[must_use]
    pub fn taxed_as(mut self, treatment: TaxTreatment) -> Self {
        self.treatment = Some(treatment);
        self
    }

    /// Add a record.
    #[must_use]
    pub fn record(mut self, cdr: &'a Cdr) -> Self {
        self.records.push(cdr);
        self
    }

    /// Add every record a ledger holds that nothing supersedes.
    ///
    /// [`CdrLedger::live`] rather than `iter`, and that is the whole reason this
    /// method exists: a correction is a *new* record, so a ledger holding a
    /// session and its correction holds both, and an invoice run that sums
    /// everything bills that session twice.
    #[must_use]
    pub fn ledger(mut self, ledger: &'a CdrLedger) -> Self {
        self.records.extend(ledger.live());
        self
    }

    /// Build it, with the account of what the rounding cost.
    ///
    /// # Errors
    ///
    /// [`BillingError`] — see the type.
    pub fn build(self) -> Result<Crossing<Invoice>, BillingError> {
        if self.records.is_empty() {
            return Err(BillingError::NoLines);
        }

        refuse_superseded(&self.records)?;

        let currency = first_currency(&self.records)?;
        let treatment = match self.treatment {
            Some(treatment) => treatment,
            None => TaxTreatment::decide(
                &self.seller.tax,
                &self.buyer.tax,
                &self.point_country,
                &self.rates,
            )?,
        };

        let mut crossing = Crossing::lossless(());
        let mut lines: Vec<InvoiceLine> = Vec::new();
        let mut adjustments: Vec<DocumentAdjustment> = Vec::new();

        for (index, cdr) in self.records.iter().enumerate() {
            let cost = cdr.cost.as_ref().ok_or_else(|| BillingError::NotRated {
                cdr: cdr.key.to_string(),
            })?;
            if cost.rated.currency != currency {
                return Err(BillingError::CurrencyMismatch {
                    invoice: currency,
                    cdr: cdr.key.to_string(),
                    found: cost.rated.currency,
                });
            }

            let before = lines.len();
            record_lines(
                cdr,
                &cost.rated,
                index + 1,
                &treatment,
                currency,
                &mut lines,
                &mut adjustments,
            );
            note_rounding(cdr, &lines[before..], before, &mut crossing);
        }

        if lines.is_empty() {
            return Err(BillingError::NoLines);
        }

        let tax = breakdown(&lines, &adjustments, &treatment, currency);
        // What the records came to, exactly: the lines, less what the bounds
        // took off them and plus what they added.
        let exact_taxable: Decimal = lines.iter().map(|line| line.exact_net).sum::<Decimal>()
            + adjustments
                .iter()
                .map(|a| a.kind.sign() * a.exact_amount)
                .sum::<Decimal>();
        let invoice = Invoice {
            number: self.number,
            issued_on: self.issued_on,
            due_on: self.due_on,
            period_from: self.period.0,
            period_to: self.period.1,
            seller: self.seller,
            buyer: self.buyer,
            currency,
            treatment,
            lines,
            adjustments,
            tax,
            exact_taxable,
            payment_terms: self.payment_terms,
            buyer_reference: self.buyer_reference,
            payment: self.payment,
        };

        // `BR-CO-25`. Asked here, where the answer is a commercial term the
        // caller holds, rather than left for `en16931::validate` to report as a
        // rule id against a document that is otherwise finished.
        if invoice.gross_total().amount() > Decimal::ZERO
            && invoice.due_on.is_none()
            && invoice.payment_terms.is_none()
        {
            return Err(BillingError::NoDueDate {
                amount: invoice.gross_total(),
            });
        }

        debug_assert!(
            invoice.reconciles(),
            "an invoice this crate builds adds up by construction"
        );

        if !invoice.rounding_residual().is_zero() {
            crossing.note(
                "/totals/taxable",
                format!(
                    "the records came to {} exactly and this invoice states {}: a difference of \
                     {}, from rounding each line to the minor unit of {currency}. The tax follows \
                     from the rounded figure, so this is the whole of what the document \
                     approximates, and the document is what settles",
                    invoice.exact_taxable_total(),
                    invoice.taxable_total(),
                    invoice.rounding_residual()
                ),
            );
        }

        Ok(crossing.map(|()| invoice))
    }
}

/// Refuse a set of records that holds both a session and its correction.
///
/// Billing both is the double billing [`CdrLedger::live`] exists to prevent, and
/// a caller that assembled the list by hand is checked too, because the fault is
/// in the list rather than in where it came from.
///
/// # It is not the same check, and it cannot be
///
/// [`CdrLedger::live`] sees the whole ledger, so it drops everything any held
/// record supersedes — a chain `A ← B ← C` leaves only `C`, however long it
/// runs. This sees a *list*, and a list of `[A, C]` states nothing about `A`
/// being stale: `C` names `B`, and `B` is not here.
///
/// Refusing a record whose `supersedes` names a key the list does not hold
/// would close that, and would refuse the ordinary case with it — an original
/// billed in June and its correction issued in July, where the July invoice
/// legitimately carries only the correction. So the check is as strong as the
/// input allows, and the way to have the ledger's answer is to hand it the
/// ledger ([`InvoiceBuilder::ledger`]).
fn refuse_superseded(records: &[&Cdr]) -> Result<(), BillingError> {
    for cdr in records {
        if let Some(previous) = &cdr.supersedes
            && let Some(superseded) = records.iter().find(|other| other.key == *previous)
        {
            return Err(BillingError::SupersededRecord {
                cdr: superseded.key.to_string(),
                corrector: cdr.key.to_string(),
            });
        }
    }
    Ok(())
}

/// Say what one record's rounding cost, when it cost anything.
///
/// Per record rather than per invoice, so the note names the session a
/// reconciliation will be looking for. Compared in the document's own basis: the
/// lines are net and the category's tax is computed from them, which is the
/// arithmetic the document itself performs.
fn note_rounding(cdr: &Cdr, lines: &[InvoiceLine], first: usize, crossing: &mut Crossing<()>) {
    let exact: Decimal = lines.iter().map(|line| line.exact_net).sum();
    let rounded: Decimal = lines.iter().map(|line| line.net).sum();
    if rounded == exact {
        return;
    }
    crossing.note(
        format!("/lines/{first}"),
        format!(
            "record {} was priced at {exact} net exactly and this invoice states {rounded}: an \
             EN 16931 line amount has two decimals and the standard's own totals are sums of the \
             line amounts, so the rounding happens here. The difference is {}",
            cdr.key,
            rounded - exact
        ),
    );
}

/// The currency the first priced record is in.
fn first_currency(records: &[&Cdr]) -> Result<Currency, BillingError> {
    let first = records[0];
    first
        .cost
        .as_ref()
        .map(|cost| cost.rated.currency)
        .ok_or_else(|| BillingError::NotRated {
            cdr: first.key.to_string(),
        })
}

/// One record's lines, and what it said it came to in the invoice's basis.
///
/// The basis is **net**, because BT-131 is. A gross tariff's net comes from
/// [`Rated::tax_summary`], which `emob-tariff` computed once per VAT category —
/// so the split is not performed a second time here, only apportioned to the
/// lines it was computed from.
fn record_lines(
    cdr: &Cdr,
    rated: &Rated,
    ordinal: usize,
    treatment: &TaxTreatment,
    currency: Currency,
    into: &mut Vec<InvoiceLine>,
    adjustments: &mut Vec<DocumentAdjustment>,
) {
    let round = |amount: Decimal| Money::new(amount, currency).round_to_minor_unit().amount();
    let description = describe(cdr);
    let first = into.len();

    for (position, line) in rated.lines.iter().enumerate() {
        // The rate the gross is *stripped at* and the rate the line is *taxed
        // at* are the same number, which is what keeps the gross the driver was
        // quoted intact through the document — and it strips the **unit price**
        // as well as the amount, because BT-146 is defined exclusive of VAT and
        // a document whose price does not reproduce its own line is one a
        // Peppol access point returns (`R120`).
        let rate = effective_rate(line.vat, treatment);
        let unit_price = net_of(line.unit_price, rate, rated.tax_included);
        let base_quantity = emob_tariff::Line::base_units_per_unit(line.dimension);
        // Derived from the two figures the document states, so the identity
        // `BT-131 = BT-129 × BT-146 ÷ BT-149` holds by construction rather than
        // to within a rounding.
        let exact_net = line.base_quantity * unit_price / base_quantity;

        into.push(InvoiceLine {
            vat_rate: stated_rate(rate, treatment),
            id: format!("{ordinal}.{}", position + 1),
            cdr: cdr.key.clone(),
            dimension: line.dimension,
            description: format!("{description} — {}", dimension_name(line.dimension)),
            started_at: cdr.started_at,
            ended_at: cdr.ended_at,
            // The base quantity — whole seconds for time — rather than the
            // hours a driver reads, because it is the figure the amount was
            // computed from and the only one that reproduces it.
            quantity: line.base_quantity,
            unit_price,
            base_quantity,
            net: round(exact_net),
            exact_net,
        });
    }

    // A minimum or maximum is a term of the **total** rather than of any one
    // line, and a maximum moves it down — which as a line would be a negative
    // BT-146 and an invoice `BR-27` refuses outright. EN 16931 models it as a
    // document level allowance or charge; see `DocumentAdjustment`.
    if let Some(adjustment) = rated.adjustment {
        let rate = effective_rate(adjustment.vat, treatment);
        // What this record's lines state on the document, and what the bound
        // says the record comes to. Both rounded, so the document reaches the
        // tariff's own figure exactly rather than a cent past it.
        let stated: Decimal = into[first..].iter().map(|line| line.net).sum();
        let exact_target = net_of(rated.exact_total().amount(), rate, rated.tax_included);
        let signed = stated - round(exact_target);
        let exact_signed: Decimal = into[first..]
            .iter()
            .map(|line| line.exact_net)
            .sum::<Decimal>()
            - exact_target;

        // A bound with nothing to adjust **is** the line. A driver who plugged
        // in, drew nothing and owes the minimum has no priced dimension, and
        // `BR-16` requires an invoice to have at least one line — so a charge
        // cannot stand alone here, and there is no document for it to be a
        // charge *on*. The mirror case cannot arise: a maximum only moves a
        // total the lines already exceeded, so lines exist by construction.
        if into.len() == first {
            into.push(InvoiceLine {
                vat_rate: stated_rate(rate, treatment),
                id: format!("{ordinal}.1"),
                cdr: cdr.key.clone(),
                dimension: Dimension::Flat,
                description: format!("{description} — {}", adjustment_name(adjustment.kind)),
                started_at: cdr.started_at,
                ended_at: cdr.ended_at,
                quantity: Decimal::ONE,
                unit_price: exact_target,
                base_quantity: Decimal::ONE,
                net: round(exact_target),
                exact_net: exact_target,
            });
            return;
        }

        if !signed.is_zero() || !exact_signed.is_zero() {
            let kind = if signed.is_sign_negative() {
                DocumentAdjustmentKind::Charge
            } else {
                DocumentAdjustmentKind::Allowance
            };
            adjustments.push(DocumentAdjustment {
                kind,
                cdr: cdr.key.clone(),
                amount: signed.abs(),
                exact_amount: exact_signed.abs(),
                vat_rate: stated_rate(rate, treatment),
                reason: format!("{description} — {}", adjustment_name(adjustment.kind)),
            });
        }
    }
}

/// The net figure behind an amount, in the basis the tariff stated it in.
fn net_of(amount: Decimal, rate: Decimal, basis: emob_tariff::TaxIncluded) -> Decimal {
    match basis {
        // Already net, or stated by a party outside a tax regime.
        emob_tariff::TaxIncluded::No | emob_tariff::TaxIncluded::NotApplicable => amount,
        emob_tariff::TaxIncluded::Yes => {
            let factor = Decimal::ONE + rate / HUNDRED;
            // A rate of exactly −100 % makes the factor zero, and `emob-tariff`
            // has already reported it as `RatingNote::VatRateNotUsable` and
            // charged the amount whole. Reporting it a second time here would
            // duplicate a note; dividing into it would panic.
            if factor.is_zero() {
                amount
            } else {
                amount / factor
            }
        }
    }
}

/// The rate a line's gross is stripped at and its tax computed from.
///
/// The component's own where the tariff states one; the supply's where it does
/// not. Those are two different facts and both are real: a tariff that says
/// `7 %` on a service fee is stating something about *that component*, and a
/// tariff that says nothing has deferred to the supply.
///
/// # Why this is not `emob-tariff`'s reading
///
/// `emob_tariff::Rated::tax_summary` treats an unstated rate as zero, and it is
/// right to: a price list has no idea who the buyer is or where the supply
/// happens, so the only split it can perform is the one its own numbers support.
/// This crate **does** know — [`TaxTreatment`] is the whole point — so it is the
/// layer entitled to say what a gross price contains, and the difference is
/// reported rather than absorbed.
fn effective_rate(component: Option<Decimal>, treatment: &TaxTreatment) -> Decimal {
    // A category that does not levy tax strips at whatever the tariff quoted —
    // a gross price quoted with 19 % in it is not a price the recipient of a
    // reverse charge owes 19 % on, and the taxable amount is what is left.
    if !treatment.category.carries_tax() {
        return component.unwrap_or(Decimal::ZERO);
    }
    component.unwrap_or(treatment.rate)
}

/// The rate the **document** states for a line — BT-152.
///
/// Zero under every category that does not levy tax. `BR-AE-5`, `BR-E-5`,
/// `BR-G-5`, `BR-IC-5` and `BR-O-5` each require it, and they are right: under a
/// reverse charge the tax exists and is somebody else's, so this document's rate
/// for it is nothing.
/// The rate a line states, which is three answers rather than two.
///
/// The effective rate where the category levies tax; **zero** where it does not
/// but still states one — `BR-AE-5` and its siblings require the figure to be
/// there and to be zero, because under a reverse charge the tax exists and is
/// the recipient's; and **absent** under `O`, the only category in UNCL 5305
/// that states no rate at all, where `BR-O-05` refuses the field outright.
///
/// Count the answers before choosing the type, and where there are three, say
/// three.
const fn stated_rate(effective: Decimal, treatment: &TaxTreatment) -> Option<Decimal> {
    if !treatment.category.states_rate() {
        None
    } else if treatment.category.carries_tax() {
        Some(effective)
    } else {
        Some(Decimal::ZERO)
    }
}

/// The VAT breakdown, from the lines.
///
/// One entry, because a [`TaxTreatment`] is a property of the invoice: a
/// document cannot be a reverse charge for one of its lines. A tariff whose
/// dimensions sit in different VAT categories is a tariff `emob-tariff` already
/// reports and the crossings already refuse; here it would be an invoice with
/// two categories and one treatment, which is a document nobody can sign.
fn breakdown(
    lines: &[InvoiceLine],
    adjustments: &[DocumentAdjustment],
    treatment: &TaxTreatment,
    currency: Currency,
) -> Vec<TaxSubtotal> {
    // One entry per **rate**, in ascending order. The *category* is a property
    // of the whole document — a supply is a reverse charge or it is not — and
    // the rate is a property of the line, because electricity and a service fee
    // can sit in different categories and EN 16931 states a taxable amount for
    // each. An invoice that taxed both at one rate would over-declare on one of
    // them and state a figure no accountant can reproduce.
    let mut groups: Vec<(Option<Decimal>, Decimal)> = Vec::new();
    let mut add = |rate: Option<Decimal>, amount: Decimal| match groups
        .iter_mut()
        .find(|(group, _)| *group == rate)
    {
        Some((_, taxable)) => *taxable += amount,
        None => groups.push((rate, amount)),
    };
    for line in lines {
        add(line.vat_rate, line.net);
    }
    // `BR-S-08` and its nine siblings: a category's taxable amount is the sum
    // of its lines **minus** the allowances and **plus** the charges in it.
    for adjustment in adjustments {
        add(
            adjustment.vat_rate,
            adjustment.kind.sign() * adjustment.amount,
        );
    }
    groups.sort_by_key(|(rate, _)| *rate);

    groups
        .into_iter()
        .map(|(rate, taxable)| TaxSubtotal {
            category: treatment.category,
            rate,
            // From the category's own taxable amount, which is what `BR-CO-17`
            // checks and `BR-S-09` computes — not the sum of per-line taxes:
            // two lines of 0.005 each round to zero apiece and to one cent
            // together, and the standard states the rule on the subtotal.
            tax: match rate {
                Some(rate) if treatment.category.carries_tax() => {
                    Money::new(taxable * rate / HUNDRED, currency)
                        .round_to_minor_unit()
                        .amount()
                }
                _ => Decimal::ZERO,
            },
            taxable,
        })
        .collect()
}

/// A one-line description of a session, for a human reading the invoice.
fn describe(cdr: &Cdr) -> String {
    format!(
        "{} {} ({})",
        cdr.evse_id,
        cdr.started_at.date(),
        cdr.session_id
    )
}

const fn dimension_name(dimension: Dimension) -> &'static str {
    match dimension {
        Dimension::Energy => "energy",
        Dimension::Time => "charging time",
        Dimension::ParkingTime => "occupancy",
        Dimension::Flat => "session fee",
    }
}

const fn adjustment_name(kind: emob_tariff::AdjustmentKind) -> &'static str {
    match kind {
        emob_tariff::AdjustmentKind::Minimum => "minimum charge",
        emob_tariff::AdjustmentKind::Maximum => "maximum charge adjustment",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tax::TaxStatus;
    use emob_cdr::{ChargingPeriod, Cost};
    use emob_core::{Direction, Energy, PartyId, QuarterHour};
    use emob_session::{AuthPath, Provenance};
    use emob_tariff::{Chargeable, Period, PriceComponent, Tariff, TariffKind, TaxIncluded, rate};
    use rust_decimal::prelude::FromStr;
    use time::macros::{date, datetime};

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-06-01 10:00 +2) + time::Duration::minutes(minute)
    }

    /// A record priced with `tariff`, delivering `energy` kWh over half an hour.
    fn record(id: &str, energy: &str, tariff: &Tariff) -> Cdr {
        let kwh = Energy::from_kwh(dec(energy)).unwrap();
        let chargeable = Chargeable::new(vec![Period::charging(at(0), at(30), kwh)]).unwrap();
        Cdr {
            key: CdrKey {
                party: PartyId::new("DE", "ABC").unwrap(),
                id: id.parse().unwrap(),
            },
            session_id: "s-1".parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(30),
            auth_path: AuthPath::AdHoc,
            authorization_reference: None,
            periods: vec![ChargingPeriod {
                quarter_hour: QuarterHour::containing(at(0)),
                start: at(0),
                end: at(30),
                energy: kwh,
                charging: true,
                provenance: Provenance::Measured,
            }],
            total_energy: kwh,
            direction: Direction::Import,
            evidence: None,
            cost: Some(Cost {
                tariff_id: tariff.id.clone(),
                tariff_fingerprint: tariff.fingerprint(),
                rated: rate(tariff, &chargeable),
            }),
            supersedes: None,
        }
    }

    /// A gross tariff whose component states no VAT rate — the commonest shape,
    /// where the rate is stated on the invoice rather than on the price list.
    fn gross_tariff_without_vat() -> Tariff {
        Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("1.19"))],
        )
    }

    fn gross_tariff() -> Tariff {
        Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
        )
    }

    fn builder<'a>(records: &[&'a Cdr]) -> InvoiceBuilder<'a> {
        let mut builder = InvoiceBuilder::new(
            "R-1",
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            Counterparty::new(
                "CPO",
                "Musterstadt",
                TaxStatus::business("DE", "DE123456789"),
            ),
            Counterparty::new("Driver", "Beispielstadt", TaxStatus::consumer("DE")),
        )
        .supplied_from("DE", dec("19"))
        .due_on(date!(2026 - 07 - 15));
        for cdr in records {
            builder = builder.record(cdr);
        }
        builder
    }

    #[test]
    fn a_gross_tariff_is_converted_once_and_rounded_at_the_line() {
        // 29.5 × 0.49 = 14.455 gross exactly. Net is 14.455 / 1.19 =
        // 12.1470588…, which the line states as 12.15 — and the difference is
        // the whole of what the document approximates.
        let tariff = gross_tariff();
        let cdr = record("c-1", "29.500", &tariff);
        let crossing = builder(&[&cdr]).build().unwrap();
        let invoice = &crossing.value;

        assert_eq!(invoice.lines.len(), 1);
        assert_eq!(invoice.lines[0].net, dec("12.15"));
        assert_eq!(invoice.taxable_total().to_string(), "12.15 EUR");
        assert_eq!(invoice.tax_total().to_string(), "2.31 EUR");
        assert_eq!(invoice.gross_total().to_string(), "14.46 EUR");
        assert!(invoice.reconciles());

        // …and it is reported rather than absorbed.
        assert!(!invoice.rounding_residual().is_zero());
        assert!(
            crossing.reasons().any(|r| r.contains("c-1")),
            "{:?}",
            crossing.notes()
        );
    }

    #[test]
    fn a_tiered_session_keeps_its_tiers_on_the_invoice() {
        // `emob-tariff` yields one line per distinct price, "which is what a
        // tiered invoice has to show". Summing them away here would take that
        // back one layer along.
        let tariff = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::No,
            elements: vec![
                emob_tariff::TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                    restrictions: emob_tariff::Restrictions {
                        max_kwh: Some(dec("10")),
                        ..emob_tariff::Restrictions::default()
                    },
                },
                emob_tariff::TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.50"),
                )]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };
        let cdr = record("c-1", "30", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;

        assert_eq!(invoice.lines.len(), 2, "{:?}", invoice.lines);
        assert_eq!(invoice.lines[0].unit_price, dec("0.30"));
        assert_eq!(invoice.lines[0].net, dec("3.00"));
        assert_eq!(invoice.lines[1].unit_price, dec("0.50"));
        assert_eq!(invoice.lines[1].net, dec("10.00"));
        assert_eq!(invoice.lines[0].id, "1.1");
        assert_eq!(invoice.lines[1].id, "1.2");
    }

    #[test]
    fn a_minimum_charge_is_a_document_level_charge_and_not_a_line() {
        // It is a term of the **total** and not of any dimension — the rating
        // engine says so — so an invoice that hid it inside the energy line
        // would state a price per kWh nobody charged. EN 16931 has the group
        // for it: BG-21, added to the taxable amount by `BR-CO-13`.
        let mut tariff = gross_tariff();
        tariff.tax_included = TaxIncluded::No;
        tariff.min_price = Some(dec("20.00"));
        let cdr = record("c-1", "10", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;

        assert_eq!(invoice.lines.len(), 1, "{:?}", invoice.lines);
        assert_eq!(invoice.lines[0].net, dec("4.90"));
        assert_eq!(invoice.adjustments.len(), 1);
        let charge = &invoice.adjustments[0];
        assert_eq!(charge.kind, DocumentAdjustmentKind::Charge);
        assert_eq!(
            charge.amount,
            dec("15.10"),
            "stated as a positive magnitude"
        );
        assert!(charge.reason.contains("minimum charge"));

        // BT-106 − BT-107 + BT-108 = BT-109.
        assert_eq!(invoice.line_total().to_string(), "4.90 EUR");
        assert_eq!(invoice.charge_total().to_string(), "15.10 EUR");
        assert_eq!(invoice.allowance_total().to_string(), "0.00 EUR");
        assert_eq!(invoice.taxable_total().to_string(), "20.00 EUR");
        assert!(invoice.reconciles());
    }

    #[test]
    fn a_capped_session_is_an_allowance_and_the_document_reaches_the_cap_exactly() {
        // A maximum moves the total **down**, and as a line that is a negative
        // BT-146 — which `BR-27` refuses outright, so the whole invoice is
        // invalid. It is a document level allowance, and the amount is derived
        // from what the document states rather than from the exact difference:
        // rounding the line and the difference independently lands a cent past
        // the cap, and the cap is the one number the driver was promised.
        let mut tariff = gross_tariff(); // 0.49 per kWh, gross, 19 %
        tariff.max_price = Some(dec("10.00"));
        let cdr = record("c-1", "29.500", &tariff); // 14.455 gross, capped to 10.00
        let invoice = builder(&[&cdr]).build().unwrap().value;

        assert_eq!(invoice.lines.len(), 1, "{:?}", invoice.lines);
        assert_eq!(invoice.adjustments.len(), 1);
        let allowance = &invoice.adjustments[0];
        assert_eq!(allowance.kind, DocumentAdjustmentKind::Allowance);
        assert_eq!(allowance.amount, dec("3.75"), "a positive magnitude");
        assert!(allowance.reason.contains("maximum"));

        assert_eq!(invoice.line_total().to_string(), "12.15 EUR");
        assert_eq!(invoice.taxable_total().to_string(), "8.40 EUR");
        assert_eq!(
            invoice.gross_total().to_string(),
            "10.00 EUR",
            "the tariff capped this session at ten euros, and the document says ten euros"
        );
        assert!(invoice.reconciles());

        // …and the document the standard judges is valid, which a negative
        // line amount is not — at the CEN core a partner settles against and
        // under the German profile a Rechnungseingangsplattform validates.
        let crossed = crate::en16931::to_en16931(&invoice, crate::en16931::CEN_CORE).unwrap();
        assert!(
            crossed.value.is_valid(),
            "{:?}",
            crossed.value.reasons().collect::<Vec<_>>()
        );
        let german = crate::en16931::to_en16931(&invoice, crate::en16931::XRECHNUNG_3).unwrap();
        assert!(
            german
                .value
                .report
                .fatal()
                .all(|finding| finding.rule.starts_with("BR-DE")),
            "an allowance must not itself be a finding: {:?}",
            german.value.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_gross_tariffs_unit_price_is_stated_net_so_the_line_reproduces_itself() {
        // BT-146 is defined **exclusive of VAT**. A gross tariff's own figure
        // put there states a price that does not produce the line amount:
        // 29.500 × 0.49 is 14.455 and the line is 12.15, which
        // `PEPPOL-EN16931-R120` refuses at a hundred times its tolerance — and
        // which a partner multiplying the two columns simply reads as wrong.
        let cdr = record("c-1", "29.500", &gross_tariff());
        let invoice = builder(&[&cdr]).build().unwrap().value;
        let line = &invoice.lines[0];

        assert_eq!(line.quantity, dec("29.500"));
        assert_eq!(line.base_quantity, Decimal::ONE);
        assert_eq!(line.unit_price, dec("0.49") / dec("1.19"));
        assert_eq!(
            line.vat_rate,
            Some(dec("19")),
            "…and the rate it was stripped at"
        );
        assert!(
            line.reconciles(),
            "BT-129 × BT-146 ÷ BT-149 = the exact BT-131"
        );
        assert_eq!(line.net, dec("12.15"));
        assert!(invoice.reconciles());
    }

    #[test]
    fn a_tariff_with_two_vat_rates_produces_two_taxable_amounts() {
        // `emob-tariff` states the rate per component, because electricity and
        // a service fee can sit in different categories — and EN 16931 wants the
        // taxable amount **per rate**. An invoice that taxed both at one rate
        // would over-declare on one of them and state a figure no accountant
        // can reproduce.
        let tariff = Tariff::simple(
            "mixed".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("1.19")).with_vat(dec("19")),
                PriceComponent::new(Dimension::Flat, dec("1.07")).with_vat(dec("7")),
            ],
        );
        let cdr = record("c-1", "10", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;

        // 10 kWh at 1.19 gross is 11.90 → 10.00 net at 19 %.
        // The session fee is 1.07 gross → 1.00 net at 7 %.
        assert_eq!(invoice.lines.len(), 2, "{:?}", invoice.lines);
        assert_eq!(invoice.lines[0].vat_rate, Some(dec("19")));
        assert_eq!(invoice.lines[1].vat_rate, Some(dec("7")));

        let rates: Vec<Option<Decimal>> = invoice.tax.iter().map(|t| t.rate).collect();
        assert_eq!(
            rates,
            vec![Some(dec("7")), Some(dec("19"))],
            "one subtotal per rate"
        );
        assert_eq!(invoice.tax_total().to_string(), "1.97 EUR", "1.90 + 0.07");
        assert_eq!(invoice.taxable_total().to_string(), "11.00 EUR");
        assert_eq!(invoice.gross_total().to_string(), "12.97 EUR");
        assert!(invoice.reconciles());
    }

    #[test]
    fn a_gross_line_that_states_no_rate_is_taxed_at_the_supplys_rate() {
        // The commonest tariff shape: a gross price with no VAT on the
        // component, and the rate stated on the invoice. Converting at zero and
        // then taxing at nineteen would grow the gross the driver was quoted.
        let tariff = gross_tariff_without_vat();
        let cdr = record("c-1", "10", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;

        assert_eq!(invoice.lines[0].vat_rate, Some(dec("19")));
        // 10 × 1.19 = 11.90 gross → 10.00 net at 19 %, and the gross the driver
        // was quoted survives the invoice.
        assert_eq!(invoice.taxable_total().to_string(), "10.00 EUR");
        assert_eq!(invoice.tax_total().to_string(), "1.90 EUR");
        assert_eq!(invoice.gross_total().to_string(), "11.90 EUR");
    }

    #[test]
    fn two_currencies_are_two_invoices() {
        let eur = gross_tariff();
        let mut chf = gross_tariff();
        chf.currency = Currency::new("CHF").unwrap();
        let a = record("c-1", "10", &eur);
        let b = record("c-2", "10", &chf);

        let err = builder(&[&a, &b]).build().unwrap_err();
        assert!(
            matches!(err, BillingError::CurrencyMismatch { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_invoice_with_no_records_is_not_an_invoice() {
        let err = builder(&[]).build().unwrap_err();
        assert!(matches!(err, BillingError::NoLines), "{err}");
    }

    #[test]
    fn the_unit_code_is_derived_from_the_dimension_and_never_stored_beside_it() {
        assert_eq!(unit_code(Dimension::Energy), "KWH");
        assert_eq!(unit_code(Dimension::Time), "SEC");
        assert_eq!(unit_code(Dimension::ParkingTime), "SEC");
        assert_eq!(unit_code(Dimension::Flat), "C62");
    }

    #[test]
    fn the_records_an_invoice_bills_are_listed_once_each() {
        let tariff = gross_tariff();
        let a = record("c-1", "10", &tariff);
        let b = record("c-2", "10", &tariff);
        let invoice = builder(&[&a, &b]).build().unwrap().value;
        assert_eq!(invoice.records().len(), 2);
        assert_eq!(invoice.lines.len(), 2);
    }
}
