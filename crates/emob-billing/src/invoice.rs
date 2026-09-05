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

use emob_cdr::{Cdr, CdrKey, CdrLedger, Cost};
use emob_core::{CdrId, Crossing, Currency, Energy, Money, PartyId};
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
    /// The VAT category this line's supply falls in — BT-151.
    ///
    /// # Why this is on the line and not on the document
    ///
    /// It was on the document, and that was right for every invoice this crate
    /// could build: an invoice is assembled from rated CDRs and every one of
    /// them is electricity, so one supply meant one treatment. C-60/23 is where
    /// that stops — a periodic subscription is a **separate and independent**
    /// supply of services beside the electricity, and a service follows a
    /// different place-of-supply rule, so one document carries two categories at
    /// two rates in two countries (D269).
    ///
    /// EN 16931 has always modelled it this way: BT-151 is a *line* field and
    /// BG-23 repeats. What changed is that this crate can now build the document
    /// the standard was already shaped for.
    pub vat_category: VatCategory,
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
    /// Energy inside this line's measured value that the meter **compensated** —
    /// cable resistance, or the rectification a DC station metered on the AC
    /// side of `[OCMF Tab. 7, CL]`.
    ///
    /// **Nothing is subtracted.** The compensation is already inside the
    /// register the session billed; what this carries is the duty to say so.
    /// `[REA 6-A §3.2]`:
    ///
    /// > Die von einem Messwert **oder einer Rechnung** Betroffenen sind in
    /// > geeigneter Weise darauf hinzuweisen, dass die … Energie für die
    /// > Gleichrichtung … Bestandteil des angegebenen Messwerts ist.
    ///
    /// The paragraph names the invoice in as many words, so the figure goes on
    /// the document rather than only to a roaming partner — and it comes from
    /// the signed record rather than from a flag somebody sets, which would be
    /// a check fed by a caller (D253). It becomes part of BT-127, this line's
    /// own free-text note.
    ///
    /// Only ever set on an energy line: the compensation is inside a register in
    /// kilowatt-hours, and a duration does not contain it.
    pub compensated_loss: Option<Energy>,
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

/// A periodic fee an e-mobility provider charges its own driver.
///
/// # Not a session, and deliberately not derived from one
///
/// C-60/23 turns on the fact that this is charged *"regardless of whether the
/// user actually purchased electricity during the relevant period"*. It has no
/// record behind it, no meter, no evidence and no `[MessEG §33]` question,
/// because it rests on no measured value — which is the same reasoning that
/// already lets a session fee onto a document without one (D232), applied to a
/// supply that is not electricity at all.
///
/// The amount is **net**, because BT-131 is and because a subscription is quoted
/// as a price rather than derived from a tariff: there is no `Rated` to strip a
/// gross figure out of and no basis to read one from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Subscription {
    /// What it is, in words a driver recognises — BT-153.
    pub description: String,
    /// The net amount for the period.
    pub net: Decimal,
    /// The first day it covers — BT-134.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub from: time::Date,
    /// …and the last — BT-135.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub to: time::Date,
}

impl Subscription {
    /// A fee for one period.
    #[must_use]
    pub fn new(
        description: impl Into<String>,
        net: Decimal,
        from: time::Date,
        to: time::Date,
    ) -> Self {
        Self {
            description: description.into(),
            net,
            from,
            to,
        }
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
    /// BT-95 / BT-102 — the category it falls in.
    ///
    /// One allowance sits in one category `[BR-S-08]`, and which one is
    /// `emob_tariff::Adjustment::vat`'s answer carried through: a bound is
    /// economically more of whatever that session mostly was.
    pub vat_category: VatCategory,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Why this category levies no tax — BT-120.
    ///
    /// On the subtotal because that is where the standard puts it: BT-120 is a
    /// field of BG-23, and BG-23 repeats. It was on the document, which worked
    /// while a document had one treatment and states the wrong thing the moment
    /// it has two — a reverse-charged subscription beside standard-rated
    /// electricity needs the sentence on the group it explains and nowhere else
    /// (D269).
    pub exemption_reason: Option<String>,
    /// Where this category's supply is taxed.
    ///
    /// Not an EN 16931 field. Carried because it is the *conclusion*
    /// [`TaxTreatment::decide`] and [`TaxTreatment::decide_service`] reached, and
    /// a document whose stated rate and stated place of supply belong to two
    /// different countries is one that reconciles against nothing — which is a
    /// thing an auditor asks and the standard has no room for.
    pub place_of_supply: String,
}

/// What a billing document is — BT-3, and the UBL root element.
///
/// # Why this is a kind rather than a sign
///
/// EN 16931 states a credit note's amounts as **positive** figures and carries
/// the direction in the document type. Negating them instead produces a document
/// that fails `BR-S-08` against its own lines and says the wrong thing twice —
/// and, one layer on, a `BR-27` violation for the negative BT-146 a reversed
/// line would carry. The same argument that makes a tariff's cap a document
/// level allowance rather than a negative line ([`DocumentAdjustment`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DocumentKind {
    /// A commercial invoice — UNCL 1001 code `380`. The default, because every
    /// document [`InvoiceBuilder`] assembles from rated records is one.
    #[default]
    Invoice,
    /// A credit note — UNCL 1001 code `381`, a German *Stornorechnung*.
    CreditNote,
}

impl DocumentKind {
    /// Whether this document reverses another.
    #[must_use]
    pub const fn is_credit_note(self) -> bool {
        matches!(self, Self::CreditNote)
    }
}

impl core::fmt::Display for DocumentKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Invoice => "invoice",
            Self::CreditNote => "credit note",
        })
    }
}

/// The document a credit note cancels — BG-3.
///
/// Its number is BT-25 and its issue date BT-26. `BR-55` requires BT-25 to have
/// content, which is why this is a struct rather than two optional fields: a
/// reference with no number is a reference nobody can follow, and inventing a
/// blank one turns a missing number into a second, more confusing finding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cancelled {
    /// BT-25 — the cancelled document's own number.
    pub number: String,
    /// BT-26 — the day it was issued.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub issued_on: time::Date,
}
/// Something the rating had to report that **the payer is entitled to read**.
///
/// `emob-tariff` produces a note for everything it had to assume or refuse, and
/// [`emob_tariff::RatingNote::concerns_the_payer`] is the half of them that say a
/// quantity was billed differently from how it was measured — which is what
/// `[AFIR Art. 5(4)]` and `[PAngV]` entitle a driver to reconcile, and what a
/// roaming partner disputes.
///
/// It names its record, because a month's invoice carries many and a note that
/// does not say which session it is about is one nobody can act on. It becomes a
/// BG-1 invoice note (BT-22) under the UNCL 4451 subject code for general
/// information, so a receiving system routes it rather than reading it.
///
/// The other half stay on the [`Crossing`] the build returns: a rate no split
/// can be computed from, two bounds that contradict, a restriction this build
/// cannot evaluate. Those are faults in a document the payer did not write, and
/// an operator queue is where they are answerable.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DocumentNote {
    /// Which record it is about, where it is about one.
    ///
    /// `None` for a note about the **document** rather than a session — the
    /// reason a `Stornorechnung` was issued is the case this crate makes, and a
    /// reason tagged with a record would tell the reader the cancellation was
    /// about that one session when it reverses the whole month.
    pub cdr: Option<CdrKey>,
    /// What the rating, or the issuer, said.
    pub text: String,
}

impl DocumentNote {
    /// A note about one record.
    #[must_use]
    pub fn about(cdr: CdrKey, text: impl Into<String>) -> Self {
        Self {
            cdr: Some(cdr),
            text: text.into(),
        }
    }

    /// A note about the document as a whole.
    #[must_use]
    pub fn on_the_document(text: impl Into<String>) -> Self {
        Self {
            cdr: None,
            text: text.into(),
        }
    }
}

/// An invoice for a period, or the credit note that cancels one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Invoice {
    /// What this document is — a demand, or the credit note that cancels one.
    ///
    /// EN 16931 carries the direction in the **document type** rather than in
    /// the sign of its amounts, so a credit note states positive figures like
    /// any other document and BT-3 says what they mean. See
    /// [`Invoice::cancellation`].
    pub kind: DocumentKind,
    /// BG-3 — the document this one cancels, when it is a credit note.
    ///
    /// `None` on an invoice. Always `Some` on a credit note this crate built,
    /// because a `Stornorechnung` that does not name what it reverses is one an
    /// auditor cannot pair.
    pub cancels: Option<Cancelled>,
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
    /// What the ratings behind this document had to report to **the payer** —
    /// BG-1, one note apiece.
    ///
    /// See [`DocumentNote`]. Empty on the ordinary invoice, because the ordinary
    /// rating has nothing to assume: a note here means a quantity was billed
    /// differently from how it was measured, and that is a sentence the person
    /// paying is entitled to find on the document rather than to discover.
    pub notes: Vec<DocumentNote>,
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
    /// Whether this document's tax statement admits anything beside itself.
    ///
    /// `O` — outside the scope of VAT — is the one category `BR-O-11` … `BR-O-14`
    /// forbid to share a document, and [`VatCategory::is_exclusive`] is the
    /// standard's own predicate for it rather than this crate's reading. Kept as
    /// a derived flag so a reader of an [`Invoice`] can see the constraint the
    /// builder enforced without re-deriving it from the breakdown.
    pub exclusive_category: bool,
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

    /// The credit note that cancels this document — a German
    /// *Stornorechnung*.
    ///
    /// # What it changes, and what it deliberately does not
    ///
    /// Everything is copied. Four things change: the [`kind`](Self::kind), which
    /// is BT-3 and the UBL root element; the number and the issue date, because
    /// a credit note is its own document with its own identity; and
    /// [`cancels`](Self::cancels), which becomes BG-3 naming what is being
    /// reversed.
    ///
    /// **The amounts are not negated.** EN 16931 carries the direction in the
    /// document type and states what is being credited as a positive figure. A
    /// reversed line would be a negative BT-146, which `BR-27` refuses outright,
    /// so a "negated" credit note is not a document at all — the same argument
    /// that makes a tariff's cap a document level allowance rather than a line
    /// (D201).
    ///
    /// **The records travel with it.** [`Self::records`] answers the same on the
    /// credit note as on the invoice, which is what lets a ledger pair the two
    /// and see that the sessions have been reversed rather than re-billed.
    ///
    /// # What the rest of this crate then does differently
    ///
    /// [`crate::postings::postings_for`] reverses every side, because a
    /// cancellation reverses the entry — and [`crate::payment::instruct`]
    /// **refuses** it, because a `Stornorechnung` is not collected by direct
    /// debit. A platform that let one through would draw the money a second
    /// time from the driver it was owed back to.
    ///
    /// # Errors
    ///
    /// [`BillingError::NotCancellable`] when this document is already a credit
    /// note. Cancelling a cancellation is a re-issued invoice rather than a
    /// second reversal, and this crate has no way to tell which was meant.
    pub fn cancellation(
        &self,
        number: impl Into<String>,
        issued_on: time::Date,
        reason: impl Into<String>,
    ) -> Result<Self, BillingError> {
        if self.kind.is_credit_note() {
            return Err(BillingError::NotCancellable {
                number: self.number.clone(),
            });
        }
        let mut notes = self.notes.clone();
        notes.insert(0, DocumentNote::on_the_document(reason));
        Ok(Self {
            kind: DocumentKind::CreditNote,
            cancels: Some(Cancelled {
                number: self.number.clone(),
                issued_on: self.issued_on,
            }),
            number: number.into(),
            issued_on,
            notes,
            ..self.clone()
        })
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
    subscriptions: Vec<Subscription>,
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
            subscriptions: Vec::new(),
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

    /// Bill a periodic fee that is **not electricity**.
    ///
    /// # The line C-60/23 keeps apart
    ///
    /// *Digital Charging Solutions* (C-60/23, 17 October 2024) describes exactly
    /// this document: a provider that bills its users *"first for the quantity
    /// of electricity supplied on a monthly basis, and second for access to the
    /// network and adjacent services"*, where the fixed fee is charged
    /// *"regardless of whether the user actually purchased electricity during
    /// the relevant period"*. The Court held the access to be a **separate and
    /// independent** supply of services.
    ///
    /// Separate means it does not follow the electricity anywhere. Electricity
    /// is a good and lands where Article 38 or 39 puts it; the fee is a service
    /// and lands where `[UStG §3a]` puts it — for a private driver where the
    /// **supplier** is established, for a business customer where the
    /// **customer** is. One document, two places of supply, and in general two
    /// categories at two rates.
    ///
    /// Its treatment is decided here from the parties rather than taken as an
    /// argument, for the same reason the electricity's is: it is a conclusion
    /// drawn from facts about the two companies, and a field somebody fills in
    /// is a field somebody fills in wrongly.
    ///
    /// A subscription needs **no** records. It is charged whether or not the
    /// driver ever plugged in — the fact the Court turned on — so an invoice
    /// that is nothing but subscriptions is a lawful invoice.
    #[must_use]
    pub fn subscription(mut self, subscription: Subscription) -> Self {
        self.subscriptions.push(subscription);
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

    /// Append the subscription lines, and the treatment that governs them.
    ///
    /// The **services** treatment beside the goods one, which C-60/23 keeps
    /// apart and which lands somewhere else entirely. Decided once for the whole
    /// set: a provider's subscriptions to one customer are one supply
    /// relationship, however many periods are billed at a time.
    fn append_subscriptions(
        &self,
        lines: &mut Vec<InvoiceLine>,
        goods: TaxTreatment,
    ) -> Result<Vec<TaxTreatment>, BillingError> {
        let mut treatments = vec![goods];
        if self.subscriptions.is_empty() {
            return Ok(treatments);
        }
        let service = TaxTreatment::decide_service(&self.seller.tax, &self.buyer.tax, &self.rates)?;
        let start = lines.len();
        for (offset, subscription) in self.subscriptions.iter().enumerate() {
            lines.push(subscription_line(
                subscription,
                start + offset + 1,
                &service,
            ));
        }
        treatments.push(service);
        Ok(treatments)
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
        refuse_export(&self.records)?;
        refuse_unsettleable(&self.records)?;
        refuse_uncarryable_bound(&self.records)?;
        refuse_unverifiable(&self.records, &self.point_country)?;

        let currency = first_currency(&self.records)?;
        // The **goods** treatment, which governs every line that came out of a
        // rated record. A caller whose own tax engine decided states it.
        let treatment = match self.treatment.clone() {
            Some(treatment) => treatment,
            None => TaxTreatment::decide(
                &self.seller.tax,
                &self.buyer.tax,
                &self.point_country,
                &self.rates,
            )?,
        };

        let mut crossing = Crossing::lossless(());
        let Assembled {
            lines,
            adjustments,
            notes,
        } = assemble(
            &self.records,
            &Basis {
                treatment: &treatment,
                currency,
            },
            &mut crossing,
        )?;
        let mut lines = lines;
        let treatments = self.append_subscriptions(&mut lines, treatment)?;

        if lines.is_empty() {
            return Err(BillingError::NoLines);
        }

        // `BR-O-11` … `BR-O-14`: `O` is the one category that may not share a
        // document, and `VatCategory::is_exclusive` is the standard's own
        // predicate for it rather than this crate's reading of the rules. A
        // subscription outside the scope of EU VAT beside standard-rated
        // electricity is a document no validator accepts, and refusing it here
        // names the two supplies rather than leaving `to_en16931` to report a
        // rule id (D269).
        let exclusive_category = refuse_mixed_exclusive(&treatments)?;

        let tax = breakdown(&lines, &adjustments, &treatments, currency);
        // What the records came to, exactly: the lines, less what the bounds
        // took off them and plus what they added.
        let exact_taxable: Decimal = lines.iter().map(|line| line.exact_net).sum::<Decimal>()
            + adjustments
                .iter()
                .map(|a| a.kind.sign() * a.exact_amount)
                .sum::<Decimal>();
        let invoice = Invoice {
            // Everything this builder assembles is a demand. A credit note is
            // made from one, by `Invoice::cancellation`, so that its lines are
            // the lines that were billed rather than lines somebody re-derived.
            kind: DocumentKind::Invoice,
            cancels: None,
            number: self.number,
            issued_on: self.issued_on,
            due_on: self.due_on,
            period_from: self.period.0,
            period_to: self.period.1,
            seller: self.seller,
            buyer: self.buyer,
            currency,
            exclusive_category,
            lines,
            adjustments,
            tax,
            exact_taxable,
            payment_terms: self.payment_terms,
            buyer_reference: self.buyer_reference,
            notes,
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

/// The market whose metrology law this crate carries.
///
/// `[MessEG §33]` binds the use of measured values in **German** commercial
/// dealings. Other member states have their own transposition of the Measuring
/// Instruments Directive, and asserting this rule over all of them would refuse
/// lawful invoices elsewhere — the same reason the obligation calendar dates
/// NIS2 from the German transposition rather than from the Directive.
const METROLOGY_REGIME: &str = "DE";

/// Whether a dimension is a measured quantity — a *Messgröße*.
///
/// Energy and the two time dimensions are measured; a per-session fee is a
/// price for an occurrence and rests on no measurement at all, which is why an
/// invoice carrying only one needs nothing behind it.
const fn is_measured(dimension: Dimension) -> bool {
    match dimension {
        Dimension::Energy | Dimension::Time | Dimension::ParkingTime => true,
        Dimension::Flat => false,
    }
}

/// Refuse an invoice based on measured values that the recipient cannot check
/// — `[MessEG §33]`.
///
/// # The paragraph names invoices, in as many words
///
/// §33(1) permits values for measured quantities to be *stated or used* in
/// commercial dealings only where a measuring instrument was used as intended
/// and the values are traceable to the measurement result. §33(3) Nr. 1 then
/// puts the duty on the document:
///
/// > Wer Messwerte verwendet, hat dafür zu sorgen, dass **Rechnungen, soweit sie
/// > auf Messwerten beruhen**, von demjenigen, für den die Rechnungen bestimmt
/// > sind, in einfacher Weise zur Überprüfung angegebener Messwerte nachvollzogen
/// > werden können.
///
/// A line priced per kilowatt-hour or per minute rests on a measured value. With
/// no signed record behind it there is nothing for the recipient to check it
/// *against* — `emob-eichrecht` produces the transparency file from exactly that
/// evidence — so the invoice cannot be made verifiable at all.
///
/// # Why the decision is here rather than in the validator
///
/// `emob_cdr::validate` grades a missing signature as a **warning** on purpose,
/// and says why in its own source: it is blocking for a German energy invoice
/// and merely notable elsewhere, so the decision belongs to the layer that knows
/// which regime applies. This is that layer: it holds the place the electricity
/// was drawn (D232).
///
/// Judged on `point_country`, which is where the *measurement* happened, and not
/// on the place of supply. A German operator settling with a French reseller has
/// moved the place of supply `[UStG §3g]` and has not moved its meters.
fn refuse_unverifiable(records: &[&Cdr], point_country: &str) -> Result<(), BillingError> {
    if !point_country.eq_ignore_ascii_case(METROLOGY_REGIME) {
        return Ok(());
    }
    for cdr in records {
        if cdr.evidence.is_some() {
            continue;
        }
        let Some(cost) = &cdr.cost else {
            // An unrated record has no line to be based on a measured value,
            // and `NotRated` is the finding it already has.
            continue;
        };
        if let Some(line) = cost
            .rated
            .lines
            .iter()
            .find(|line| is_measured(line.dimension))
        {
            return Err(BillingError::NotVerifiable {
                cdr: cdr.key.to_string(),
                dimension: dimension_name(Part::Session, line.dimension).to_owned(),
            });
        }
    }
    Ok(())
}

/// Refuse a record its own validator blocks.
///
/// # The composition, rather than fifteen conditions
///
/// `emob_cdr::validate` already asks everything that makes a record unsettleable
/// — periods that overlap and bill a minute twice, a line whose own numbers do
/// not produce its own amount, a price computed for a different quantity than
/// the record states, an authorisation stronger than the signed record supports,
/// a direction the evidence contradicts. `CdrBuilder` refuses to *issue* such a
/// record, and this is the layer that *sends the demand* — and it was accepting
/// any `Cdr` that happened to carry a `Cost`.
///
/// A record built by this workspace passes its own validator by construction, so
/// this changes nothing about the ordinary path. It is about the other two: a
/// record assembled from a partner's document (`emob_roam::ocpi::from_ocpi`
/// never goes through the builder) and one deserialised from wherever a service
/// kept it. Rule 5 in one line rather than in fifteen (D232).
///
/// Warnings pass. `Severity::Warning` is deliberately where `validate` puts the
/// findings whose consequence is a **regime** question rather than an arithmetic
/// one — missing evidence above all, which is blocking for a German energy
/// invoice `[MessEG §33]` and merely notable elsewhere. That decision is made
/// below, where the place of supply is known.
fn refuse_unsettleable(records: &[&Cdr]) -> Result<(), BillingError> {
    for cdr in records {
        let report = emob_cdr::validate(cdr);
        if !report.is_settleable() {
            return Err(BillingError::NotSettleable {
                cdr: cdr.key.to_string(),
                reasons: report.blocking().map(ToString::to_string).collect(),
            });
        }
    }
    Ok(())
}

/// Refuse a record whose bound no set of allowances can carry.
///
/// # The document that passes every rule and is still unacceptable
///
/// `BR-S-08` makes a VAT category's taxable amount its lines minus its
/// allowances, and a cut deeper than a category's own lines drives BT-116
/// negative. Nothing in the standard forbids it: the invoice reconciles, the
/// totals chain holds, `to_en16931` produces the document and **all 317 rules
/// pass** — and no tax office accepts a negative taxable amount under a positive
/// invoice (D283).
///
/// For a record this workspace rated that case no longer arises.
/// `emob_tariff::Rated::adjustment_parts` draws the bound from as many
/// categories as it needs (BG-20 is repeatable) and `bound` solves the price
/// against the same walk, so the parts sum to the bound and no part is deeper
/// than its own category (D284).
///
/// What remains is the door those values arrive by. A `Rated` **deserialised**
/// from a partner's document went through no rating and no clamp, so its
/// adjustment can exceed everything the record charges — and then the split has
/// a remainder it leaves on the first part rather than losing. That is the
/// record this refuses, by the same rule the document is built with: **no part
/// deeper than its own category.**
///
/// Both parts of a record are checked. A reservation is priced against its own
/// window by its own elements and `rate_reservation` sets no bound today — but
/// "today" is the shape of a bug that comes back, and the loop costs nothing.
fn refuse_uncarryable_bound(records: &[&Cdr]) -> Result<(), BillingError> {
    for cdr in records {
        let Some(cost) = &cdr.cost else { continue };
        for rated in core::iter::once(&cost.rated).chain(cost.reservation.as_ref()) {
            for part in rated.adjustment_parts() {
                // Only a cut can take a category below zero; a minimum adds.
                if !part.amount.is_sign_negative() {
                    continue;
                }
                let available: Decimal = rated
                    .lines
                    .iter()
                    .filter(|line| line.vat == part.vat)
                    .map(|line| line.amount)
                    .sum();
                let shortfall = available + part.amount;
                if shortfall.is_sign_negative() {
                    return Err(BillingError::AdjustmentExceedsCategory {
                        cdr: cdr.key.to_string(),
                        vat: part.vat,
                        amount: part.amount,
                        available,
                        shortfall,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Refuse a record of energy that flowed **out** of the vehicle.
///
/// # Why this is a refusal rather than a sign
///
/// A V2G discharge is not a smaller sale or a negative one. It is a supply in the
/// **other direction**: the driver is the supplier and the operator the customer,
/// which moves the party, the place of supply and the VAT liability all at once,
/// and in Germany it is ordinarily settled as a self-billed *Gutschrift*
/// `[UStG §14]` — a document with the parties the other way round. Every one of
/// those is a fact about an arrangement this crate cannot read off a CDR.
///
/// Priced through the same tariff and put on an invoice unchanged, it becomes a
/// **demand** addressed to the person who supplied the energy, and nothing
/// downstream objects: the document is valid EN 16931, the postings balance, and
/// the direct debit collects. `emob_roam::ocpi::to_ocpi` already refuses the same
/// record by name — OCPI cannot express it either — so the exception was carried
/// by one layer of the chain and dropped by the one that sends the document
/// (D230).
fn refuse_export(records: &[&Cdr]) -> Result<(), BillingError> {
    for cdr in records {
        if cdr.direction == emob_core::Direction::Export {
            return Err(BillingError::ExportNotBillable {
                cdr: cdr.key.to_string(),
                energy: cdr.total_energy,
            });
        }
    }
    Ok(())
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

/// Every record's lines and document-level adjustments, in the order the records
/// were added.
///
/// Split out of [`InvoiceBuilder::build`] because it is the one loop in that
/// function: `build` decides *what document this is* — the parties, the tax
/// treatment, the currency, the terms — and this turns the records into the
/// lines it states.
///
/// # One currency, asked of both of a record's ratings
///
/// A document states one currency, and a record carries **two** ratings. Asking
/// it only of the session let a reservation priced in another currency reach a
/// document that cannot express it; `emob_cdr::validate()` blocks the record
/// before this layer sees it, and this is the same question asked where the
/// document's own currency is known (D250).
fn assemble(
    records: &[&Cdr],
    basis: &Basis<'_>,
    crossing: &mut Crossing<()>,
) -> Result<Assembled, BillingError> {
    let mut lines: Vec<InvoiceLine> = Vec::new();
    let mut adjustments: Vec<DocumentAdjustment> = Vec::new();
    let mut notes: Vec<DocumentNote> = Vec::new();

    for (index, cdr) in records.iter().enumerate() {
        let cost = cdr.cost.as_ref().ok_or_else(|| BillingError::NotRated {
            cdr: cdr.key.to_string(),
        })?;
        for rated in core::iter::once(&cost.rated).chain(cost.reservation.as_ref()) {
            if rated.currency != basis.currency {
                return Err(BillingError::CurrencyMismatch {
                    invoice: basis.currency,
                    cdr: cdr.key.to_string(),
                    found: rated.currency,
                });
            }
        }

        // A reservation is priced against its own window by its own elements,
        // and `[OCPI 2.3.0]` gives it its own `total_reservation_cost` — but it
        // is money the same driver owes on the same record, and an invoice that
        // omits it bills less than the record says (D250). Both parts are
        // stripped, numbered and taxed by one function, because two spellings of
        // "turn a rating into lines" is the drift this crate exists to prevent.
        let before = lines.len();
        let mut position = 0usize;
        for supply in Supply::of(cdr, cost) {
            record_lines(
                &supply,
                index + 1,
                &mut position,
                basis,
                &mut lines,
                &mut adjustments,
                crossing,
            );
        }
        note_rounding(cdr, &lines[before..], before, crossing);

        // What the rating had to report, split by who can act on it. A quantity
        // billed differently from how it was measured is a sentence the payer
        // is entitled to find on the document; a fault in the tariff or the
        // record is one only the operator can answer, and it goes to the queue
        // that reads the crossing (D253).
        for rated in core::iter::once(&cost.rated).chain(cost.reservation.as_ref()) {
            for note in &rated.notes {
                if note.concerns_the_payer() {
                    notes.push(DocumentNote::about(cdr.key.clone(), note.to_string()));
                } else {
                    crossing.note(
                        format!("/lines/{before}"),
                        format!("record {}: {note}", cdr.key),
                    );
                }
            }
        }
    }

    Ok(Assembled {
        lines,
        adjustments,
        notes,
    })
}

/// What [`assemble`] produces: the document's lines, the bounds that became
/// allowances or charges, and the notes the payer is owed.
struct Assembled {
    lines: Vec<InvoiceLine>,
    adjustments: Vec<DocumentAdjustment>,
    notes: Vec<DocumentNote>,
}

/// What every line of one document shares: the tax treatment the parties settled
/// on, and the currency its amounts are stated in.
///
/// Together rather than apart because they are decided together — the treatment
/// names a place of supply and the currency is that document's, and a function
/// that took one without the other could be handed a mismatched pair.
struct Basis<'a> {
    treatment: &'a TaxTreatment,
    currency: Currency,
}

/// One priced part of a record: the session, or the reservation that preceded it.
///
/// `[OCPI 2.3.0 §mod_cdrs_cdr_object]` prices the two separately and states them
/// in two fields, because the reservation's clock ran **before** the cable went
/// in — so the two parts do not share a window, and the same `TIME` dimension
/// means "minutes charging" in one and "minutes held" in the other. What they do
/// share is the driver, the record and the document, which is why both reach the
/// invoice through one function rather than two (D250).
struct Supply<'a> {
    /// The record both parts belong to.
    cdr: &'a Cdr,
    rated: &'a Rated,
    part: Part,
    from: time::OffsetDateTime,
    to: time::OffsetDateTime,
}

/// Which of a record's two priced parts a [`Supply`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The charging session — the energy, its minutes and its fees.
    Session,
    /// The reservation that ran before it.
    Reservation,
}

impl<'a> Supply<'a> {
    /// A record's parts, in the order they are billed: the reservation ran
    /// first, and it is stated first.
    ///
    /// A reservation that came to **nothing** is left out rather than adding an
    /// empty group — `BR-16` counts lines, not intentions. "Nothing" is asked of
    /// the lines *and* the adjustment, because those are the two terms of a
    /// total `emob-tariff` produces and skipping on the lines alone would be a
    /// second place assuming that `rate_reservation` never sets a bound.
    ///
    /// A priced reservation the record does not place in time never reaches
    /// here: it is a blocking finding, and `refuse_unsettleable` has already
    /// turned every one of those away.
    fn of(cdr: &'a Cdr, cost: &'a Cost) -> Vec<Self> {
        let mut out = Vec::with_capacity(2);
        if let Some(rated) = &cost.reservation
            && let Some(held) = cdr.reservation
            && !(rated.lines.is_empty() && rated.adjustment.is_none())
        {
            out.push(Self {
                cdr,
                rated,
                part: Part::Reservation,
                from: held.from,
                to: held.to,
            });
        }
        out.push(Self {
            cdr,
            rated: &cost.rated,
            part: Part::Session,
            from: cdr.started_at,
            to: cdr.ended_at,
        });
        out
    }
}

/// One priced part's lines, and what it said it came to in the invoice's basis.
///
/// The basis is **net**, because BT-131 is. A gross tariff's net comes from
/// [`Rated::tax_summary`], which `emob-tariff` computed once per VAT category —
/// so the split is not performed a second time here, only apportioned to the
/// lines it was computed from.
///
/// `position` is carried across a record's parts rather than restarting, so a
/// record's line identifiers are `1.1`, `1.2`, `1.3` in one sequence whether or
/// not a reservation preceded it. Two lines numbered `1.1` on one document is a
/// document `BR-21` refuses.
fn record_lines(
    supply: &Supply<'_>,
    ordinal: usize,
    position: &mut usize,
    basis: &Basis<'_>,
    into: &mut Vec<InvoiceLine>,
    adjustments: &mut Vec<DocumentAdjustment>,
    crossing: &mut Crossing<()>,
) {
    let (cdr, rated) = (supply.cdr, supply.rated);
    let (treatment, currency) = (basis.treatment, basis.currency);
    let round = |amount: Decimal| Money::new(amount, currency).round_to_minor_unit().amount();
    let description = describe(cdr);
    let first = into.len();

    for line in &rated.lines {
        *position += 1;
        // The rate the gross is *stripped at* and the rate the line is *taxed
        // at* are the same number, which is what keeps the gross the driver was
        // quoted intact through the document — and it strips the **unit price**
        // as well as the amount, because BT-146 is defined exclusive of VAT and
        // a document whose price does not reproduce its own line is one a
        // Peppol access point returns (`R120`).
        let rate = effective_rate(line.vat, treatment);
        // A tariff written for a German estate and priced at a French point
        // states 19 % inside a supply France taxes at 20 % — the case
        // `[UStG §3g]` creates. The tariff's rate governs, because this crate
        // never moves a gross price and overriding would change what the driver
        // pays; the disagreement is reported rather than settled in silence,
        // because ignoring it leaves a document whose place of supply and rate
        // belong to two different countries (D271).
        if let Some(stated) = line.vat
            && treatment.category.carries_tax()
            && stated != treatment.rate
        {
            crossing.note(
                format!("/lines/{position}"),
                format!(
                    "this line is taxed at the {stated} % the tariff quotes and its supply is \
                     taxed in {} at {} %: the gross price a driver was shown is not moved, so the \
                     document states a rate the place of supply does not levy",
                    treatment.place_of_supply, treatment.rate
                ),
            );
        }
        let unit_price = net_of(line.unit_price, rate, rated.tax_included);
        let base_quantity = emob_tariff::Line::base_units_per_unit(line.dimension);
        // Derived from the two figures the document states, so
        // `BT-129 × BT-146 ÷ BT-149` is this line's own arithmetic rather than a
        // second computation of it — `InvoiceLine::reconciles` is that identity,
        // and BT-131 is this figure rounded to the minor unit once, below.
        //
        // BT-146 is the one amount on the document EN 16931 puts **no** decimal
        // cap on (Table 26), which is what lets a gross tariff's net price stay
        // as exact as `Decimal` can state it: `0.49 / 1.19` does not terminate,
        // and every place kept here is a place the residual does not have to
        // carry. The two amounts beside it are capped at two, and both are
        // rounded.
        let exact_net = line.base_quantity * unit_price / base_quantity;

        into.push(InvoiceLine {
            vat_rate: stated_rate(rate, treatment),
            vat_category: treatment.category,
            id: format!("{ordinal}.{position}"),
            cdr: cdr.key.clone(),
            dimension: line.dimension,
            description: format!(
                "{description} — {}",
                dimension_name(supply.part, line.dimension)
            ),
            // The window this part was priced over. A reservation ran before
            // the session started, so BT-134/BT-135 on its line state the
            // reservation's own dates — not the session's, which would put a
            // supply on a document outside the period it happened in.
            started_at: supply.from,
            ended_at: supply.to,
            // The base quantity — whole seconds for time — rather than the
            // hours a driver reads, because it is the figure the amount was
            // computed from and the only one that reproduces it.
            quantity: line.base_quantity,
            unit_price,
            base_quantity,
            net: round(exact_net),
            exact_net,
            // The compensation is inside a register in kilowatt-hours, so it is
            // a statement about the energy line and about no other. A duration
            // does not contain it, and a session fee rests on no measurement at
            // all.
            compensated_loss: (line.dimension == Dimension::Energy)
                .then(|| cdr.evidence.as_ref().and_then(|e| e.compensated_loss))
                .flatten(),
        });
    }

    record_adjustments(supply, ordinal, position, basis, first, into, adjustments);
}

/// The bound's document-level allowances — one per VAT category it is drawn
/// from.
///
/// A minimum or maximum is a term of the **total** rather than of any one line,
/// and a maximum moves it down — which as a line would be a negative BT-146 and
/// an invoice `BR-27` refuses outright. EN 16931 models it as a document level
/// allowance or charge; see [`DocumentAdjustment`].
///
/// Split out of [`record_lines`] when the bound stopped being one allowance
/// (D284): the lines answer *what was supplied*, and this answers *what the
/// tariff's own limits did to the total*, which is a different question with a
/// different failure mode.
fn record_adjustments(
    supply: &Supply<'_>,
    ordinal: usize,
    position: &mut usize,
    basis: &Basis<'_>,
    first: usize,
    into: &mut Vec<InvoiceLine>,
    adjustments: &mut Vec<DocumentAdjustment>,
) {
    let (cdr, rated) = (supply.cdr, supply.rated);
    let (treatment, currency) = (basis.treatment, basis.currency);
    let description = describe(cdr);
    let round = |amount: Decimal| Money::new(amount, currency).round_to_minor_unit().amount();

    // A minimum or maximum is a term of the **total** rather than of any one
    // line, and a maximum moves it down — which as a line would be a negative
    // BT-146 and an invoice `BR-27` refuses outright. EN 16931 models it as a
    // document level allowance or charge; see `DocumentAdjustment`.
    if let Some(adjustment) = rated.adjustment {
        let rate = effective_rate(adjustment.vat, treatment);
        // **Where the bound lands, category by category.** One part in the
        // ordinary case; several where a cut is deeper than the category
        // `Adjustment::vat` names, because BG-20 is repeatable and BR-S-08
        // would otherwise state a negative taxable amount (D283, D284). The
        // split is `emob-tariff`'s, computed against the same walk the price
        // was solved along, so the document and the total agree by
        // construction rather than by a second reading here.
        let parts = rated.adjustment_parts();
        // What this record's lines state on the document, and what the bound
        // says the record comes to. Both rounded, so the document reaches the
        // tariff's own figure exactly rather than a cent past it.
        //
        // # Every term is stripped at its own rate, including this one
        //
        // The target used to be the record's whole total put through `net_of`
        // once, at the **adjustment's** rate. That is right on the one shape
        // every fixture had — a tariff whose components sit in one VAT
        // category — and wrong on any other: a session priced at 19 % beside a
        // fee at 7 % had its 7 % half divided by 1.19 as well, and the document
        // came out €1.20 under a €100.00 session (D248). The lines a few lines
        // above already strip **per line**; each part of the bound is one
        // further amount in one further category, so each is stripped on its
        // own and added, and the two halves of this function read one rule.
        let stated: Decimal = into[first..].iter().map(|line| line.net).sum();
        let exact_lines: Decimal = into[first..].iter().map(|line| line.exact_net).sum();
        let part_nets: Vec<Decimal> = parts
            .iter()
            .map(|part| {
                net_of(
                    part.amount,
                    effective_rate(part.vat, treatment),
                    rated.tax_included,
                )
            })
            .collect();
        let adjustment_net: Decimal = part_nets.iter().sum();
        let exact_target = exact_lines + adjustment_net;
        let signed = stated - round(exact_target);
        // Which is `-adjustment_net`, written as the difference so that the
        // rounded and the exact figure are visibly the same subtraction.
        let exact_signed: Decimal = exact_lines - exact_target;

        // A bound with nothing to adjust **is** the line. A driver who plugged
        // in, drew nothing and owes the minimum has no priced dimension, and
        // `BR-16` requires an invoice to have at least one line — so a charge
        // cannot stand alone here, and there is no document for it to be a
        // charge *on*. The mirror case cannot arise: a maximum only moves a
        // total the lines already exceeded, so lines exist by construction.
        if into.len() == first {
            *position += 1;
            into.push(InvoiceLine {
                vat_rate: stated_rate(rate, treatment),
                vat_category: treatment.category,
                id: format!("{ordinal}.{position}"),
                cdr: cdr.key.clone(),
                dimension: Dimension::Flat,
                description: format!("{description} — {}", adjustment_name(adjustment.kind)),
                started_at: supply.from,
                ended_at: supply.to,
                quantity: Decimal::ONE,
                unit_price: exact_target,
                base_quantity: Decimal::ONE,
                net: round(exact_target),
                exact_net: exact_target,
                // A minimum charge on a session that delivered nothing rests on
                // no measured value, so there is none to disclose the contents
                // of.
                compensated_loss: None,
            });
            return;
        }

        if !signed.is_zero() || !exact_signed.is_zero() {
            // **The rounded total is the document's, and it is shared out.**
            // `signed` is a difference of two figures the document already
            // prints, which is what makes the invoice reach the tariff's own
            // total exactly rather than a cent past it — so it cannot be
            // re-derived per part. Each part takes its own rounded share and
            // the largest one carries whatever the rounding left over, because
            // it is by construction the part with the most room under it.
            let mut shares: Vec<Decimal> = part_nets.iter().map(|net| round(-net)).collect();
            if let Some(index) = largest_share(&shares) {
                let allocated: Decimal = shares.iter().sum();
                shares[index] += signed - allocated;
            }

            for ((part, share), exact) in parts.iter().zip(&shares).zip(&part_nets) {
                let exact_share = -exact;
                if share.is_zero() && exact_share.is_zero() {
                    continue;
                }
                // Which side of the totals chain this sits on, from the figure
                // that has a side. The rounded difference is the document's, so
                // it decides — but it can be zero while the exact one is not,
                // because both ends round to the same minor unit, and then
                // `is_sign_negative` on a zero says "Allowance" about a bound
                // that moved the total *up*.
                //
                // The document is unharmed either way — the amount is 0.00 —
                // but `exact_amount` is signed by `kind`, so the wrong side
                // subtracts a residual it should add and `rounding_residual`
                // reports twice the difference against a document that is
                // exactly right. Rounding is monotonic, so the two figures can
                // only disagree through zero: where the rounded one has no
                // side, the exact one is the only one left to ask.
                let direction = if share.is_zero() { exact_share } else { *share };
                let kind = if direction.is_sign_negative() {
                    DocumentAdjustmentKind::Charge
                } else {
                    DocumentAdjustmentKind::Allowance
                };
                adjustments.push(DocumentAdjustment {
                    kind,
                    vat_category: treatment.category,
                    cdr: cdr.key.clone(),
                    amount: share.abs(),
                    exact_amount: exact_share.abs(),
                    vat_rate: stated_rate(effective_rate(part.vat, treatment), treatment),
                    reason: format!("{description} — {}", adjustment_name(adjustment.kind)),
                });
            }
        }
    }
}

/// The part that carries the rounding residual: the largest share by magnitude.
///
/// It has the most room under it, so a cent moved onto it cannot take a
/// category below what its lines hold — which is the whole reason the bound was
/// split in the first place.
fn largest_share(shares: &[Decimal]) -> Option<usize> {
    shares
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.abs().cmp(&right.abs()))
        .map(|(index, _)| index)
}

/// Refuse a document whose categories cannot share one, and say which two.
///
/// `BR-O-11` … `BR-O-14` make `O` — outside the scope of VAT — exclusive: no
/// second breakdown group, no line, allowance or charge in another category.
/// [`VatCategory::is_exclusive`] is `en16931`'s own predicate, generated from the
/// CEN artefacts, so this crate states the *consequence* and not the rule.
///
/// Returns whether the document's statement is an exclusive one, which is what a
/// reader needs to know before asking whether it may carry a VAT identifier.
fn refuse_mixed_exclusive(treatments: &[TaxTreatment]) -> Result<bool, BillingError> {
    let mut categories: Vec<VatCategory> = treatments.iter().map(|t| t.category).collect();
    categories.sort_unstable_by_key(|category| category.code());
    categories.dedup();

    let exclusive = categories.iter().copied().find(|c| c.is_exclusive());
    match (exclusive, categories.len()) {
        (Some(_), 1) => Ok(true),
        (Some(exclusive), _) => Err(BillingError::NoTaxTreatment {
            reason: format!(
                "this document states category {} and {}, and {} may not share a document with \
                 any other [BR-O-11..14]: the electricity and the subscription are taxed in two \
                 places and only one of them is inside the scope of EU VAT, so they are two \
                 documents",
                exclusive.code(),
                categories
                    .iter()
                    .filter(|c| !c.is_exclusive())
                    .map(|c| c.code().to_owned())
                    .collect::<Vec<_>>()
                    .join(", "),
                exclusive.code(),
            ),
        }),
        (None, _) => Ok(false),
    }
}

/// One subscription, as the line C-60/23 keeps apart from the electricity.
///
/// A quantity of **one** at the fee's own price, in the unit a session fee
/// already uses — `C62`, a dimensionless count — because a month of access is
/// one thing supplied once and not a measured quantity of anything.
fn subscription_line(
    subscription: &Subscription,
    position: usize,
    service: &TaxTreatment,
) -> InvoiceLine {
    let rate = effective_rate(None, service);
    InvoiceLine {
        vat_rate: stated_rate(rate, service),
        vat_category: service.category,
        id: format!("S.{position}"),
        // A subscription belongs to no record. The key is the document's own,
        // and nothing downstream reads it as a session: `records()` lists what
        // an invoice billed, and a fee that rests on no measurement is not one
        // of them.
        cdr: CdrKey {
            party: PartyId::new("ZZ", "SUB").unwrap_or_else(|_| unreachable!()),
            id: CdrId::new("subscription").unwrap_or_else(|_| unreachable!()),
        },
        dimension: Dimension::Flat,
        description: subscription.description.clone(),
        started_at: subscription.from.midnight().assume_utc(),
        ended_at: subscription.to.midnight().assume_utc(),
        quantity: Decimal::ONE,
        unit_price: subscription.net,
        base_quantity: Decimal::ONE,
        net: subscription.net,
        exact_net: subscription.net,
        // A fee that rests on no measured value has no measured value to
        // disclose the contents of `[REA 6-A §3.2]`.
        compensated_loss: None,
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
    treatments: &[TaxTreatment],
    currency: Currency,
) -> Vec<TaxSubtotal> {
    // One entry per **(category, rate)**, which is what BG-23 groups on. It was
    // per rate, with the category taken from the document — right while a
    // document had one supply, and wrong the moment it has two: a
    // reverse-charged subscription and standard-rated electricity can both state
    // a rate of nothing and are not one group (D269).
    let mut groups: Vec<((VatCategory, Option<Decimal>), Decimal)> = Vec::new();
    let mut add = |key: (VatCategory, Option<Decimal>), amount: Decimal| match groups
        .iter_mut()
        .find(|(group, _)| *group == key)
    {
        Some((_, taxable)) => *taxable += amount,
        None => groups.push((key, amount)),
    };
    for line in lines {
        add((line.vat_category, line.vat_rate), line.net);
    }
    // `BR-S-08` and its nine siblings: a category's taxable amount is the sum
    // of its lines **minus** the allowances and **plus** the charges in it.
    for adjustment in adjustments {
        add(
            (adjustment.vat_category, adjustment.vat_rate),
            adjustment.kind.sign() * adjustment.amount,
        );
    }
    groups.sort_by(|((left, left_rate), _), ((right, right_rate), _)| {
        left.code()
            .cmp(right.code())
            .then(left_rate.cmp(right_rate))
    });

    groups
        .into_iter()
        .map(|((category, rate), taxable)| {
            // BT-120 and the place of supply belong to the *supply*, so they
            // come from the treatment that produced this group rather than from
            // the document — which no longer has one to give.
            //
            // Matched on the **rate** as well as the category, because two
            // supplies can share a category and not a country: electricity taxed
            // in France at 20 % beside a subscription taxed in Germany at 19 %
            // is two `S` groups, and taking the first would put the French
            // rate under a German place of supply.
            //
            // Falling back to the category alone where no rate matches, which is
            // the case a tariff's own stated rate creates — the document then
            // states a rate the place of supply does not levy, and there is a
            // note beside it saying so (D271).
            let stated = treatments
                .iter()
                .find(|t| {
                    t.category == category && (!category.carries_tax() || Some(t.rate) == rate)
                })
                .or_else(|| treatments.iter().find(|t| t.category == category));
            TaxSubtotal {
                category,
                rate,
                // From the category's own taxable amount, which is what
                // `BR-CO-17` checks and `BR-S-09` computes — not the sum of
                // per-line taxes: two lines of 0.005 each round to zero apiece
                // and to one cent together, and the standard states the rule on
                // the subtotal.
                tax: match rate {
                    Some(rate) if category.carries_tax() => {
                        Money::new(taxable * rate / HUNDRED, currency)
                            .round_to_minor_unit()
                            .amount()
                    }
                    _ => Decimal::ZERO,
                },
                taxable,
                exemption_reason: stated.and_then(|t| t.reason.clone()),
                place_of_supply: stated
                    .map(|t| t.place_of_supply.clone())
                    .unwrap_or_default(),
            }
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

/// What a dimension is called on a line, in the part that priced it.
///
/// The same `TIME` dimension is two supplies: minutes the vehicle was charging,
/// and minutes a point was held for a driver who had not arrived. A document
/// that calls both "charging time" is one the driver cannot check against what
/// they were quoted, which is the whole of `[AFIR Art. 5(4)]`'s point.
const fn dimension_name(part: Part, dimension: Dimension) -> &'static str {
    match (part, dimension) {
        (Part::Session, Dimension::Energy) => "energy",
        (Part::Session, Dimension::Time) => "charging time",
        (Part::Session, Dimension::ParkingTime) => "occupancy",
        (Part::Session, Dimension::Flat) => "session fee",
        (Part::Reservation, Dimension::Time) => "reservation",
        (Part::Reservation, Dimension::Flat) => "reservation fee",
        // `[OCPI 2.3.0 §mod_tariffs_tariffdimensiontype_enum]` lets only `FLAT`
        // and `TIME` price a reservation, and `rate_reservation` hands over a
        // window with no energy in it — so neither of these can be reached from
        // a rating this workspace produced. Named rather than folded into the
        // session's spelling, because a line that arrived here anyway is one a
        // reader has to be able to tell apart.
        (Part::Reservation, Dimension::Energy) => "reservation energy",
        (Part::Reservation, Dimension::ParkingTime) => "reservation occupancy",
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
    use emob_core::Activity;
    use emob_tariff::PriceLimit;

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
            reservation: None,
            session_id: "s-1".parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(30),
            auth_path: AuthPath::AdHoc,
            authorization_reference: None,
            clock: emob_core::ClockResolution::conforming(),
            periods: vec![ChargingPeriod {
                quarter_hour: QuarterHour::containing(at(0)),
                start: at(0),
                end: at(30),
                energy: kwh,
                activity: Activity::Charging,
                provenance: Provenance::Measured,
            }],
            total_energy: kwh,
            direction: Direction::Import,
            // Signed, because `[MessEG §33]` lets a measured value be used in
            // German commercial dealings only where it is traceable to the
            // measurement, and requires an invoice resting on one to be
            // checkable by the person it is addressed to. Every fixture here
            // bills German kilowatt-hours, so every one of them needs a record
            // behind it (D232).
            evidence: Some(emob_cdr::EvidenceRef {
                encoding_method: "OCMF".into(),
                payload_digests: vec![[1u8; 32]],
                identification_strength: emob_core::IdentificationStrength::Trusted,
                energy_billable: true,
                duration_billable: true,
                direction: Some(Direction::Import),
                compensated_loss: None,
                tariff_changes: Vec::new(),
            }),
            cost: Some(Cost {
                tariff_id: tariff.id.clone(),
                tariff_fingerprint: tariff.fingerprint(),
                rated: rate(tariff, &chargeable),
                reservation: None,
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

    /// A tariff that prices a reservation beside the session it precedes.
    fn tariff_with_reservation() -> Tariff {
        Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: emob_tariff::TaxIncluded::Yes,
            elements: vec![
                emob_tariff::TariffElement {
                    components: vec![
                        PriceComponent::new(Dimension::Time, dec("6.00")).with_vat(dec("19")),
                    ],
                    restrictions: emob_tariff::Restrictions {
                        reservation: Some(emob_tariff::ReservationRestriction::Reservation),
                        ..emob_tariff::Restrictions::default()
                    },
                },
                emob_tariff::TariffElement::unrestricted(vec![
                    PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
                ]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        }
    }

    #[test]
    fn the_rectification_loss_inside_a_measured_value_reaches_the_document() {
        // `[REA 6-A §3.2]` lets a DC station meter on the AC side, before the
        // rectifier, and then obliges the operator to tell "die von einem
        // Messwert **oder einer Rechnung** Betroffenen" that the losses are
        // inside the number they are billed for. The chain computes it, the CDR
        // carries it, the OCPI crossing tells the roaming partner — and the
        // document addressed to the person the sentence names said nothing,
        // while a boolean on the charge point's profile asserted the disclosure
        // had been made (D253).
        let tariff = gross_tariff();
        let mut cdr = record("c-1", "10.000", &tariff);
        cdr.evidence.as_mut().unwrap().compensated_loss =
            Some(Energy::from_kwh(dec("0.150")).unwrap());

        let invoice = builder(&[&cdr])
            .build()
            .unwrap()
            .into_value_discarding_notes();
        let energy = invoice
            .lines
            .iter()
            .find(|l| l.dimension == Dimension::Energy)
            .expect("the record was priced per kWh");
        assert_eq!(
            energy.compensated_loss,
            Some(Energy::from_kwh(dec("0.150")).unwrap())
        );

        // …and it is on the EN 16931 document, in BT-127, which is the line
        // stating the measured value.
        let crossed = crate::en16931::to_en16931(&invoice, crate::en16931::Specification::Core)
            .unwrap()
            .into_value_discarding_notes();
        let note = crossed
            .invoice
            .lines
            .iter()
            .find_map(|l| l.note.as_ref())
            .expect("BT-127 is set");
        assert!(note.contains("REA 6-A"), "got {note}");
        assert!(
            note.contains("0.150"),
            "the figure itself is stated: {note}"
        );
        assert!(
            crossed.is_valid(),
            "{:?}",
            crossed.reasons().collect::<Vec<_>>()
        );

        // A line that rests on no such value says nothing extra.
        let plain = record("c-2", "10.000", &tariff);
        let plain_invoice = builder(&[&plain])
            .build()
            .unwrap()
            .into_value_discarding_notes();
        assert!(
            plain_invoice
                .lines
                .iter()
                .all(|l| l.compensated_loss.is_none())
        );
    }

    #[test]
    fn a_note_the_payer_is_owed_is_on_the_document_and_the_rest_is_not() {
        // A block size bills up to one block more than was delivered. That is
        // lawful and it is also the payer's business — they are being charged
        // for kilowatt-hours nobody delivered — so it belongs on the document.
        // A power restriction the tariff carries is a fact about the operator's
        // own document and belongs in their queue.
        let mut tariff = gross_tariff();
        tariff.elements[0].components[0].step_size = 3_000; // whole 3 kWh blocks
        tariff.elements.push(emob_tariff::TariffElement {
            components: vec![
                PriceComponent::new(Dimension::ParkingTime, dec("1.00")).with_vat(dec("19")),
            ],
            restrictions: emob_tariff::Restrictions {
                min_power_kw: Some(dec("50")),
                ..emob_tariff::Restrictions::default()
            },
        });
        let cdr = record("c-1", "10.000", &tariff);

        let crossing = builder(&[&cdr]).build().unwrap();
        let operator_saw: Vec<String> = crossing.reasons().collect();
        let invoice = crossing.into_value_discarding_notes();

        assert!(
            invoice.notes.iter().any(|n| n.text.contains("rounded up")),
            "the block rounding is on the document: {:?}",
            invoice.notes
        );
        assert!(
            invoice
                .notes
                .iter()
                .all(|n| !n.text.contains("average power")),
            "a fault in the operator's own tariff is not: {:?}",
            invoice.notes
        );
        assert!(
            operator_saw.iter().any(|r| r.contains("average power")),
            "…and it reached the queue that can act on it: {operator_saw:?}"
        );

        // Every note names its record, because a month carries many.
        assert!(
            invoice
                .notes
                .iter()
                .all(|n| n.cdr.as_ref() == Some(&cdr.key))
        );

        // And the standard accepts the document with BG-1 on it, in both
        // syntaxes CEN/TS 16931-2 makes mandatory.
        let crossed = crate::en16931::to_en16931(&invoice, crate::en16931::Specification::Core)
            .unwrap()
            .into_value_discarding_notes();
        assert!(
            crossed.is_valid(),
            "{:?}",
            crossed.reasons().collect::<Vec<_>>()
        );
        assert!(
            crossed.invoice.notes.iter().any(|n| n
                .note
                .as_deref()
                .is_some_and(|text| text.contains("rounded up"))),
            "BT-22 carries it"
        );
    }

    #[test]
    fn a_reservation_the_driver_paid_for_reaches_the_invoice() {
        let tariff = tariff_with_reservation();
        let mut cdr = record("c-1", "10.000", &tariff);
        // Reserved at 09:30, plugged in at 10:00: half an hour at 6.00/h.
        let held = emob_tariff::Reservation::honoured(at(-30), at(0));
        cdr.reservation = Some(held);
        let cost = cdr.cost.as_mut().unwrap();
        cost.reservation = Some(emob_tariff::rate_reservation(&tariff, &held));

        let reserved = cost.reservation.as_ref().unwrap().gross();
        assert_eq!(
            reserved.to_string(),
            "3.00 EUR",
            "the reservation was priced"
        );

        let record_total = cost.gross();
        let invoice = builder(&[&cdr])
            .build()
            .unwrap()
            .into_value_discarding_notes();

        assert_eq!(
            invoice.gross_total(),
            record_total,
            "the invoice has to bill what the record says the driver owes"
        );

        // The reservation is stated first, over its own window, under its own
        // name — and the two parts share one line sequence.
        let ids: Vec<&str> = invoice.lines.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["1.1", "1.2"], "one sequence across both parts");
        assert!(
            invoice.lines[0].description.ends_with("reservation"),
            "got {}",
            invoice.lines[0].description
        );
        assert_eq!(
            (invoice.lines[0].started_at, invoice.lines[0].ended_at),
            (at(-30), at(0)),
            "a reservation's line states the window the reservation ran in"
        );
        assert_eq!(invoice.lines[1].dimension, Dimension::Energy);

        assert!(invoice.reconciles(), "the document adds up");
        let postings = crate::postings::postings_for(&invoice);
        assert!(postings.balances(), "and the books balance on it");
    }

    #[test]
    fn a_cap_over_two_vat_rates_bills_what_the_tariff_priced() {
        // Energy at 19 %, a session fee at 7 %, gross prices, and a maximum
        // that takes € 10.70 off a € 110.70 session. Two rates is what makes
        // this a different question from the fixture below: a document-level
        // allowance carries **one** VAT rate — BT-95 and BT-96 — so the amount
        // it states has to be that category's own net, and the record's total
        // put through one factor is not it (D248).
        //
        // 100.00 gross = 84.03 (19 %) + 10.00 (7 %) − 8.99 allowance, taxed at
        // 14.26 + 0.70. Any other split bills a sum the driver was not quoted.
        let tariff = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: emob_tariff::TaxIncluded::Yes,
            elements: vec![emob_tariff::TariffElement::unrestricted(vec![
                PriceComponent::new(Dimension::Energy, dec("10.00")).with_vat(dec("19")),
                PriceComponent::new(Dimension::Flat, dec("10.70")).with_vat(dec("7")),
            ])],
            min_price: None,
            max_price: Some(emob_tariff::PriceLimit::gross(dec("100.00"))),
            valid_from: None,
            valid_until: None,
        };
        let cdr = record("c-1", "10.000", &tariff);
        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.gross().to_string(), "100.00 EUR");

        let invoice = builder(&[&cdr]).build().unwrap().value;
        assert_eq!(invoice.adjustments.len(), 1);
        assert_eq!(
            invoice.adjustments[0].kind,
            DocumentAdjustmentKind::Allowance
        );
        assert_eq!(invoice.adjustments[0].amount, dec("8.99"));
        assert_eq!(invoice.adjustments[0].vat_rate, Some(dec("19")));
        assert_eq!(invoice.taxable_total().to_string(), "85.04 EUR");
        assert_eq!(invoice.tax_total().to_string(), "14.96 EUR");
        // The whole point: the document bills the money the tariff priced.
        assert_eq!(invoice.gross_total(), rated.gross());
        assert!(invoice.reconciles());
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
        tariff.min_price = Some(PriceLimit::net(dec("20.00")));
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
    fn a_record_its_own_validator_blocks_is_not_a_record_this_layer_sends() {
        // Rule 5, at the seam where the money leaves. A record built by this
        // workspace passes its own validator by construction — a property test
        // asserts it over a thousand generated sessions — so this is about the
        // other two doors: a record assembled from a partner's document, which
        // never goes through `CdrBuilder`, and one deserialised from wherever a
        // service kept it (D232).
        let tariff = gross_tariff();
        let good = record("c-1", "29.500", &tariff);
        assert!(builder(&[&good]).build().is_ok());

        // The same minute in two periods is the same minute billed twice, and
        // it is the fault this layer must not pass on.
        let mut overlapping = good.clone();
        let extra = overlapping.periods[0].clone();
        overlapping.periods.push(extra);
        overlapping.total_energy =
            Energy::from_kwh(overlapping.total_energy.kwh() * Decimal::TWO).unwrap();

        let err = builder(&[&overlapping]).build().unwrap_err();
        let BillingError::NotSettleable { reasons, .. } = &err else {
            panic!("{err}");
        };
        assert!(
            reasons.iter().any(|r| r.contains("outside the session")
                || r.contains("periods")
                || r.contains("overlap")),
            "{reasons:?}"
        );

        // …and a warning is not a block. Missing evidence is where `validate`
        // deliberately stops and hands the decision on, because it is a regime
        // question rather than an arithmetic one — see the test below, which is
        // where that decision is made.
        let mut unsigned = good;
        unsigned.evidence = None;
        let report = emob_cdr::validate(&unsigned);
        assert!(
            report.is_settleable(),
            "a missing signature does not block here"
        );
        assert!(
            report.warnings().next().is_some(),
            "…and it is still reported"
        );
    }

    #[test]
    fn a_german_kilowatt_hour_with_nothing_behind_it_is_not_an_invoice_line() {
        // The decision `emob_cdr::validate` grades as a warning **on purpose**,
        // and says in its own source belongs "to the billing layer that knows
        // which regime applies". This is that layer, and until now the sentence
        // described a decision nothing made (D232).
        //
        // `[MessEG §33(3) Nr. 1]` names invoices in as many words: those resting
        // on measured values have to be ones the recipient can follow in order
        // to check the values stated. A line per kilowatt-hour rests on one, and
        // with no signed record behind it there is nothing to check it against.
        let tariff = gross_tariff();
        let mut unsigned = record("c-1", "29.500", &tariff);
        unsigned.evidence = None;

        let err = builder(&[&unsigned]).build().unwrap_err();
        assert!(matches!(err, BillingError::NotVerifiable { .. }), "{err}");
        assert!(err.to_string().contains("MessEG"), "{err}");

        // The regime is where the **measurement** happened, not where the supply
        // is taxed. The same record drawn at a Dutch point is an ordinary Dutch
        // invoice, because §33 binds German commercial dealings and asserting it
        // over every member state would refuse lawful documents elsewhere.
        let dutch = InvoiceBuilder::new(
            "R-1",
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            Counterparty::new(
                "CPO",
                "Amsterdam",
                TaxStatus::business("NL", "NL123456789B01"),
            ),
            Counterparty::new("Driver", "Amsterdam", TaxStatus::consumer("NL")),
        )
        .supplied_from("NL", dec("21"))
        .due_on(date!(2026 - 07 - 15))
        .record(&unsigned)
        .build();
        assert!(dutch.is_ok(), "{:?}", dutch.err());

        // …and a session fee rests on no measurement at all, so a document
        // carrying only one needs nothing behind it even here.
        let flat = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Flat, dec("1.19"))],
        );
        let mut fee_only = record("c-2", "0", &flat);
        fee_only.evidence = None;
        assert!(builder(&[&fee_only]).build().is_ok());
    }

    #[test]
    fn energy_that_flowed_out_of_the_vehicle_is_not_a_line_on_the_drivers_invoice() {
        // The one exception the chain carried everywhere except at the end. A
        // V2G discharge is a supply in the **other** direction: the driver
        // supplies and the operator buys, which moves the party, the place of
        // supply and the VAT liability, and in Germany is ordinarily a
        // self-billed Gutschrift with the parties reversed `[UStG §14]`.
        //
        // Priced through the same tariff and put on an invoice unchanged it
        // demands €14.46 from the person who *supplied* the energy — and
        // nothing downstream objects, because the document is valid EN 16931,
        // the postings balance and the direct debit collects. `to_ocpi` already
        // refuses the same record by name; this is the layer that sends the
        // document (D230).
        let tariff = gross_tariff();
        let mut discharge = record("c-1", "29.500", &tariff);
        discharge.direction = Direction::Export;

        let err = builder(&[&discharge]).build().unwrap_err();
        assert!(
            matches!(err, BillingError::ExportNotBillable { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("Gutschrift"), "{err}");

        // …and one export among many imports takes the whole document with it,
        // rather than being the line nobody reads.
        let import = record("c-2", "10.000", &tariff);
        assert!(matches!(
            builder(&[&import, &discharge]).build(),
            Err(BillingError::ExportNotBillable { .. })
        ));
        // The same records without it are an ordinary invoice.
        assert!(builder(&[&import]).build().is_ok());
    }

    #[test]
    fn a_cancellation_is_the_same_document_with_a_direction_and_a_reference_back() {
        // A Stornorechnung reverses an invoice without re-billing it, and every
        // figure on it stays **positive**: EN 16931 carries the direction in
        // BT-3 and the UBL root element. Negating the lines would produce a
        // negative BT-146, which `BR-27` refuses outright — the same argument
        // that makes a tariff's cap a document level allowance (D201).
        let tariff = gross_tariff();
        let cdr = record("c-1", "29.500", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;
        let storno = invoice
            .cancellation("R-1-STORNO", date!(2026 - 08 - 15), "re-rated")
            .unwrap();

        assert_eq!(storno.kind, DocumentKind::CreditNote);
        assert_eq!(storno.number, "R-1-STORNO");
        assert_eq!(storno.issued_on, date!(2026 - 08 - 15));
        assert_eq!(
            storno.cancels,
            Some(Cancelled {
                number: "R-1".to_owned(),
                issued_on: date!(2026 - 07 - 01),
            })
        );

        // Same money, same lines, same records — which is what lets a ledger
        // pair the two and see a reversal rather than a second sale.
        assert_eq!(storno.lines, invoice.lines);
        assert_eq!(storno.gross_total(), invoice.gross_total());
        assert_eq!(storno.records(), invoice.records());
        assert!(storno.lines.iter().all(|line| line.net > Decimal::ZERO));
        assert!(storno.reconciles());

        // …and cancelling a cancellation is a re-issued invoice rather than a
        // second reversal, which this crate cannot tell apart.
        assert!(matches!(
            storno.cancellation("R-1-STORNO-2", date!(2026 - 08 - 16), "re-rated"),
            Err(BillingError::NotCancellable { .. })
        ));
    }

    #[test]
    fn a_cancellation_is_a_credit_note_the_standard_accepts_and_a_direct_debit_refuses() {
        let tariff = gross_tariff();
        let cdr = record("c-1", "29.500", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;
        let storno = invoice
            .cancellation("R-1-STORNO", date!(2026 - 08 - 15), "re-rated")
            .unwrap();

        // BT-3 is 381 and BG-3 names the document being reversed — `BR-55`
        // wants BT-25 to have content, and the reference is the whole point of
        // a Stornorechnung.
        let crossed =
            crate::en16931::to_en16931(&storno, crate::en16931::Specification::Core).unwrap();
        assert_eq!(
            crossed
                .value
                .invoice
                .type_code
                .as_ref()
                .map(en16931::invoice::Code::as_str),
            Some("381")
        );
        assert_eq!(crossed.value.invoice.number.as_deref(), Some("R-1-STORNO"));
        assert_eq!(crossed.value.invoice.preceding_invoices.len(), 1);
        assert_eq!(
            crossed.value.invoice.preceding_invoices[0]
                .reference
                .as_str(),
            "R-1"
        );
        assert!(
            crossed.value.is_valid(),
            "{:?}",
            crossed.value.reasons().collect::<Vec<_>>()
        );

        // …and it serialises as a credit note in both mandatory syntaxes. UBL
        // spells the two documents with different root elements, in different
        // namespaces, with different names for the type code and the line — so
        // a `kind` that did not reach the writer would produce an `<Invoice>`
        // claiming BT-3 = 381, which is a document `BR-CL-01` refuses on the
        // way in and no schema catches on the way out.
        for syntax in [crate::en16931::Syntax::Ubl, crate::en16931::Syntax::Cii] {
            let xml = crate::en16931::write(&storno, crate::en16931::Specification::Core, syntax)
                .unwrap()
                .value;
            assert!(xml.contains("381"), "{syntax}: {xml}");
            assert!(xml.contains("R-1"), "{syntax}: BG-3 is missing");
        }
        let ubl = crate::en16931::write(
            &storno,
            crate::en16931::Specification::Core,
            crate::en16931::Syntax::Ubl,
        )
        .unwrap()
        .value;
        assert!(ubl.contains("<CreditNote "), "{ubl}");

        // …and the one thing a platform must never do with it. Every figure on
        // a credit note is positive, so nothing else in `instruct` would have
        // objected and the driver would have been debited twice.
        let err = crate::payment::instruct(
            &storno,
            &crate::payment::Creditor {
                name: "CPO".to_owned(),
                iban: sepa::validate_iban("DE89370400440532013000").unwrap(),
                bic: None,
                creditor_id: sepa::validate_creditor_id("DE98ZZZ09999999999").unwrap(),
            },
            &crate::payment::Mandate {
                reference: "M-1".to_owned(),
                signed_on: sepa::IsoDate::new(2026, 1, 1).unwrap(),
                debtor_name: "Driver".to_owned(),
                debtor_iban: sepa::validate_iban("DE89370400440532013000").unwrap(),
            },
            sepa::IsoDate::new(2026, 8, 25).unwrap(),
            sepa::IsoDateTime::new(sepa::IsoDate::new(2026, 8, 15).unwrap(), 0, 0, 0).unwrap(),
        )
        .unwrap_err();
        assert!(
            matches!(err, crate::payment::PaymentError::NotAnInvoice { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_cancellation_reverses_the_books_rather_than_repeating_them() {
        // The half a platform gets wrong silently. A credit note booked like the
        // invoice it cancels doubles the revenue and the VAT liability, and the
        // books then disagree with the two documents that were sent — at year
        // end, in a reconciliation nobody runs until then.
        let tariff = gross_tariff();
        let cdr = record("c-1", "29.500", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;
        let storno = invoice
            .cancellation("R-1-STORNO", date!(2026 - 08 - 15), "re-rated")
            .unwrap();

        let billed = crate::postings::postings_for(&invoice);
        let reversed = crate::postings::postings_for(&storno);

        assert!(billed.balances() && reversed.balances());
        assert_eq!(billed.debits(), reversed.credits());
        assert_eq!(billed.credits(), reversed.debits());
        assert_eq!(
            billed.roles(),
            reversed.roles(),
            "the same accounts move, the other way"
        );
        for (one, other) in billed.postings.iter().zip(&reversed.postings) {
            assert_eq!(one.amount, other.amount);
            assert_ne!(one.side, other.side);
        }
        // …and the reversal is booked on its own issue date, not the invoice's.
        assert_eq!(reversed.booked_on, date!(2026 - 08 - 15));
        assert_eq!(reversed.reference, "R-1-STORNO");
    }

    #[test]
    fn a_bound_the_document_already_reaches_is_still_on_the_side_it_moved_the_total() {
        // 24.99 kWh at 0.40 net is 9.996 exactly, and the tariff's minimum is
        // 10.00 — so the line *rounds to* the minimum and the charge carrying
        // the difference is 0.00. It still has a side. `exact_amount` is signed
        // by `kind`, and reading the side off a rounded difference of zero made
        // this an allowance: the 0.004 the minimum **added** was subtracted
        // instead, `exact_taxable` came out at 9.992, and the document reported
        // 0.008 of approximation against a figure it reaches exactly.
        //
        // Rounding is monotonic, so the rounded difference and the exact one can
        // only disagree through zero — which is precisely where the rounded one
        // has no side to read.
        let mut tariff = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("0.40"))],
        );
        tariff.tax_included = TaxIncluded::No;
        tariff.min_price = Some(PriceLimit::net(dec("10.00")));
        let cdr = record("c-1", "24.990", &tariff);
        let invoice = builder(&[&cdr]).build().unwrap().value;

        assert_eq!(invoice.lines.len(), 1, "{:?}", invoice.lines);
        assert_eq!(invoice.lines[0].exact_net, dec("9.996"));
        assert_eq!(invoice.lines[0].net, dec("10.00"));

        assert_eq!(invoice.adjustments.len(), 1);
        let adjustment = &invoice.adjustments[0];
        assert_eq!(
            adjustment.kind,
            DocumentAdjustmentKind::Charge,
            "a minimum moves the total up, whatever the rounded difference came to"
        );
        assert_eq!(
            adjustment.amount,
            Decimal::ZERO,
            "the document's own lines already state the minimum"
        );
        assert_eq!(adjustment.exact_amount, dec("0.004"));

        assert_eq!(invoice.taxable_total().to_string(), "10.00 EUR");
        assert_eq!(
            invoice.exact_taxable_total().amount(),
            dec("10.000"),
            "the lines plus what the bound added is the tariff's own minimum"
        );
        assert!(
            invoice.rounding_residual().is_zero(),
            "the document reaches the minimum exactly, so it approximates nothing: {}",
            invoice.rounding_residual()
        );
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
        // The bound is stated in the basis the prices are quoted in: this
        // tariff's are gross, so this is a gross ceiling.
        tariff.max_price = Some(PriceLimit::gross(dec("10.00")));
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
        let crossed =
            crate::en16931::to_en16931(&invoice, crate::en16931::Specification::Core).unwrap();
        assert!(
            crossed.value.is_valid(),
            "{:?}",
            crossed.value.reasons().collect::<Vec<_>>()
        );
        let german =
            crate::en16931::to_en16931(&invoice, crate::en16931::Specification::XRechnung).unwrap();
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
