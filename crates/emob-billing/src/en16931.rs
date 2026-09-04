//! The invoice, as the European e-invoice.
//!
//! # What this crossing is for
//!
//! [`crate::Invoice`] is this workspace's model: it knows about sessions, EVSE
//! ids and quarter hours, and it adds up. EN 16931 is the *legal* model: 164
//! business terms, 223 syntax-independent rules, and a national usage
//! specification on top that a German public buyer will actually validate
//! against. The two are not the same document and the second is the one that
//! gets sent.
//!
//! `en16931` owns that model and its rules, so nothing here re-reads the
//! standard. What lives here is the mapping and — the part that is worth
//! writing down — **which of this workspace's facts the standard has no field
//! for**, reported the way every other seam in this workspace reports it: a
//! [`Crossing`], with a JSON Pointer into the document the recipient will have
//! open.
//!
//! # The verdict is the deliverable, not the XML
//!
//! An invoice that serialises and does not validate is an invoice that will come
//! back. So [`to_en16931`] returns the semantic invoice and its
//! [`ValidationReport`] together, and [`write()`] refuses to produce a
//! document that its profile rejects: `Validated<P>` is a type that cannot be
//! constructed from an invalid invoice, which is the same discipline
//! `Evidence::billable_energy` applies to a kilowatt-hour one layer down.
//!
//! # Two questions, and neither has a default
//!
//! [`Specification`] is BT-24 **and** the rules the document is judged by, in
//! one argument, so nothing can claim one profile having been checked against
//! another. [`Syntax`] is UBL or CII, the two CEN/TS 16931-2 makes mandatory:
//! one semantic invoice, two spellings, and which one an access point takes is a
//! fact about the recipient.
//!
//! # What EN 16931 cannot carry, and is told
//!
//! | Fact | Where it goes |
//! |---|---|
//! | the signed meter records | **nowhere**: there is no business term for evidence. `BT-18` carries *an* object identifier and the digests are many. The record is named instead, so a holder of the invoice can ask for it |
//! | the quarter-hour periods | **nowhere**: a line is a quantity and a price, and the settlement grid is not an invoice concept |
//! | which record a line came from | `BT-127`, the line's own free text — where a dispute starts |
//! | compensated cable or rectification loss | the same note, because `[REA 6-A §3.2]` names *"einem Messwert **oder einer Rechnung**"* and this is the line stating the measured value |
//! | a rating note the **payer** is owed | `BG-1` (BT-22), coded `AAI`, one per note. A quantity billed differently from how it was measured is a sentence the person paying finds on the document rather than discovers |
//! | a rating note only the **operator** can act on | the [`Crossing`] this returns. A rate no split can be computed from is a fault in a document the payer did not write |
//!
//! The first two rows are losses and say so. The tariff version is not among
//! them and is deliberately absent: a line names its record, and the record
//! names the tariff by content, so a second identifier here would be a name the
//! recipient cannot resolve without the record anyway (D253).

use emob_core::Crossing;
use en16931::invoice::{
    Code, CreditTransfer, DirectDebit, DocumentAllowanceCharge, Item, LineVat, Party,
    PaymentInstructions, PaymentMeans, PostalAddress, PriceDetails, VatBreakdown,
};
use en16931::validation::ValidationReport;
use en16931::{Date, Identifier, InvoiceAmount, Percentage, Quantity};
use rust_decimal::Decimal;

/// UNCL 4451 — *general information*, the subject code BT-21 takes for a note
/// that is neither a payment term nor a tax statement.
const GENERAL_INFORMATION: &str = "AAI";

use crate::error::BillingError;
use crate::invoice::{Counterparty, DocumentAdjustmentKind, Invoice, InvoiceLine, PaymentDetails};
use crate::tax::VatCategory;

/// The specification a document is written against — BT-24, **and** the rule
/// set it is judged by.
///
/// # Why the two are one argument
///
/// They are the same decision, and separating them is how a document comes to
/// claim one thing and have been checked against another. That is the single
/// most common way an invoice passes local validation and is rejected on
/// receipt, and it is the reason `en16931` carries a typed proof at all: a
/// `Validated<XRechnung>` cannot be constructed from an invoice the `XRechnung`
/// rules reject, and the writer stamps BT-24 from the profile that proved it.
/// Taking BT-24 as a string here would put the mismatch back one layer up.
///
/// # Which one an invoice needs
///
/// `[UStG §14]` requires a B2B invoice to conform to Directive 2014/55/EU —
/// which is EN 16931 itself, [`Self::Core`]. `XRechnung`'s `BR-DE-*` rules are a
/// German **public-sector** usage specification: they demand a Leitweg-ID, a
/// seller contact and payment instructions that a private company neither has
/// nor needs. Writing every invoice as `XRechnung` would refuse lawful B2B
/// documents for want of a routing identifier the recipient does not issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Specification {
    /// EN 16931 itself — the core the Directive names, and what a partner
    /// settlement and an ordinary B2B invoice are judged against.
    Core,
    /// `XRechnung` 3.0, the German public-sector CIUS.
    XRechnung,
    /// Peppol BIS Billing 3.0, for a document that crosses the Peppol network.
    PeppolBis3,
}

impl Specification {
    /// The identifier BT-24 states.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Core => "urn:cen.eu:en16931:2017",
            Self::XRechnung => {
                "urn:cen.eu:en16931:2017#compliant#urn:xoev-de:kosit:standard:xrechnung_3.0"
            }
            Self::PeppolBis3 => {
                "urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0"
            }
        }
    }
}

impl core::fmt::Display for Specification {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Core => "EN 16931",
            Self::XRechnung => "XRechnung 3.0",
            Self::PeppolBis3 => "Peppol BIS Billing 3.0",
        })
    }
}

/// Which of the two syntaxes CEN/TS 16931-2 makes mandatory.
///
/// Both are the same semantic invoice; a recipient's access point accepts one,
/// the other or either, and it is a fact about the recipient rather than about
/// the document. Neither is a default here for the reason a time zone is not:
/// a syntax nobody chose is a document somebody returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Syntax {
    /// OASIS UBL 2.1, which the German platforms take by default.
    Ubl,
    /// UN/CEFACT CII D16B — the other mandatory one, and the payload every
    /// `ZUGFeRD` hybrid PDF carries.
    Cii,
}

impl core::fmt::Display for Syntax {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Ubl => "UBL",
            Self::Cii => "CII",
        })
    }
}

/// UNCL 1001 code 380 — a commercial invoice.
const COMMERCIAL_INVOICE: &str = "380";
/// The business process BT-23 states.
///
/// `PEPPOL-EN16931-R001` makes it mandatory and `XRechnung` inherits the rule.
/// `01:1.0` is billing, which is what an invoice for delivered energy is.
const BILLING_PROCESS: &str = "urn:fdc:peppol.eu:2017:poacc:billing:01:1.0";

/// The semantic invoice and the verdict on it.
#[derive(Debug)]
pub struct Crossed {
    /// The invoice, in EN 16931's own model.
    pub invoice: en16931::Invoice,
    /// What the rules say about it.
    pub report: ValidationReport,
}

impl Crossed {
    /// Whether the document would be accepted.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.report.is_valid()
    }

    /// One line per fatal finding, naming the business term it points at.
    ///
    /// EN 16931 findings point at a *term* — `BT-1`, `lines[3]/BT-151` — rather
    /// than at an `XPath`, which is what makes them actionable before a document
    /// exists.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.report
            .fatal()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
    }
}

/// Carry an invoice onto EN 16931's semantic model, and validate it.
///
/// `specification` is BT-24 and it selects the rule set the document will be
/// judged by — [`Specification::Core`] for a partner settlement or an ordinary
/// B2B invoice under `[UStG §14]`, [`Specification::XRechnung`] for a German
/// public buyer. It is an argument because it is a fact about the *recipient*,
/// and this crate has no way to know one.
///
/// # Errors
///
/// [`BillingError::UnrepresentableAmount`] when a figure will not fit an
/// EN 16931 amount — which cannot happen for an invoice this crate built, and
/// can for one that arrived over a wire.
pub fn to_en16931(
    invoice: &Invoice,
    specification: Specification,
) -> Result<Crossing<Crossed>, BillingError> {
    let mut crossing = Crossing::lossless(());

    // A credit note is built as the document it reverses and then turned into
    // one, because `en16931::Invoice::to_credit_note` is what knows the four
    // changes — BT-3, the UBL root element, the new identity, and the BG-3
    // reference back — and a second spelling of them here would be a second
    // answer. So BT-1 and BT-2 start as the *cancelled* document's, and the
    // conversion below moves them into BG-3 and puts this document's own in
    // their place (D229).
    let (number, issued_on) = invoice.cancels.as_ref().map_or_else(
        || (invoice.number.clone(), invoice.issued_on),
        |cancelled| (cancelled.number.clone(), cancelled.issued_on),
    );

    let mut builder = en16931::Invoice::builder(
        specification.identifier(),
        number,
        date_of(issued_on, "issue date (BT-2)")?,
        COMMERCIAL_INVOICE,
        invoice.currency.as_str(),
    )
    .business_process(BILLING_PROCESS)
    .seller(party(&invoice.seller, invoice.treatment.category))
    .buyer(party(&invoice.buyer, invoice.treatment.category));

    if let Some(terms) = &invoice.payment_terms {
        builder = builder.payment_terms(terms.clone());
    }
    if let Some(reference) = &invoice.buyer_reference {
        builder = builder.buyer_reference(reference.clone());
    }

    builder = payer_notes(invoice, builder);

    for line in &invoice.lines {
        builder = builder.line(invoice_line(invoice, line)?);
    }

    builder = adjustments(invoice, builder)?;

    // BG-23 is stated rather than reconciled from the lines, because the
    // category and its reason are a property of the whole document — a supply is
    // a reverse charge or it is not — and `build_reconciled` would recompute the
    // numbers and then have nowhere to put the reason. The numbers are the ones
    // `crate::Invoice` already computed and asserts it can reproduce.
    for subtotal in &invoice.tax {
        let mut entry = VatBreakdown {
            taxable_amount: amount(subtotal.taxable, invoice, "taxable amount")?,
            tax_amount: amount(subtotal.tax, invoice, "tax amount")?,
            category: Code::new(subtotal.category.code()),
            // `None` under `O`, the only category that states no rate:
            // `BR-O-05`'s breakdown sibling refuses the field, and zero is the
            // field. `TaxSubtotal::rate` already carries the distinction.
            rate: subtotal.rate.map(Percentage::new),
            exemption_reason: None,
            exemption_reason_code: None,
        };
        if subtotal.category.requires_exemption_reason() {
            // BT-120. `[UStG §14a]` asks for the same sentence in German law,
            // and `TaxTreatment` is where it was decided rather than invented
            // here.
            entry.exemption_reason.clone_from(&invoice.treatment.reason);
        }
        builder = builder.vat_breakdown(entry);
    }

    let totals = totals_of(invoice)?;
    let mut built = builder.totals(totals).build();
    built.invoicing_period = Some(en16931::invoice::Period {
        start: Some(date_of(
            invoice.period_from,
            "invoicing period start (BT-73)",
        )?),
        end: Some(date_of(invoice.period_to, "invoicing period end (BT-74)")?),
    });
    if let Some(due) = invoice.due_on {
        built.due_date = Some(date_of(due, "due date (BT-9)")?);
    }
    built.payment = invoice.payment.as_ref().map(payment_instructions);

    // …and now it becomes the credit note, through the upstream function whose
    // documentation is the specification for what a `Stornorechnung` changes.
    // A `kind` of `CreditNote` with no `cancels` cannot be built by this crate,
    // and one that arrived over a wire gets a document with no BG-3 rather than
    // an invented reference: `BR-55` wants BT-25 to have content, and a blank
    // one turns a missing number into a second, more confusing finding.
    if invoice.kind.is_credit_note() {
        built = built.to_credit_note(
            invoice.number.clone(),
            date_of(invoice.issued_on, "credit note issue date (BT-2)")?,
        );
    }

    // The three facts this workspace holds and the standard has no term for.
    // Said once per document rather than once per line: ninety-six identical
    // notes are the same fact reported ninety-six times.
    crossing.note(
        "/lines",
        format!(
            "{} record(s) back these lines and EN 16931 has no business term for the signed meter \
             data behind them. The digests stay on the CDR, which is what a customer's own \
             verifier is handed under [MessEG §33]",
            invoice.records().len()
        ),
    );
    if invoice
        .lines
        .iter()
        .any(|line| line.quantity.scale() > 3 || line.unit_price.scale() > 4)
    {
        crossing.note(
            "/lines",
            "a quantity or a unit price on this invoice carries more decimals than an invoice is \
             usually read at. EN 16931 caps neither — BT-146 is an unbounded decimal and BT-129 a \
             quantity — so nothing was narrowed, and a renderer that shows two will show a price \
             that does not reproduce its own line",
        );
    }
    if invoice
        .lines
        .iter()
        .any(|line| line.base_quantity != Decimal::ONE)
    {
        crossing.note(
            "/lines",
            "a time line is stated in whole seconds against a price per 3600 of them (BT-149), \
             because a duration in hours is usually not a decimal and a rounded quantity no \
             longer reproduces its own amount. A renderer that ignores BT-149 shows a price per \
             second that is 3600 times too high",
        );
    }

    let report = en16931::validate(&built);
    Ok(crossing.map(|()| Crossed {
        invoice: built,
        report,
    }))
}

/// The invoice as an EN 16931 document, in a named specification and one of the
/// two mandatory syntaxes.
///
/// # The verdict comes first, and it is the profile's own
///
/// A document that serialises and does not validate is a document that will come
/// back, so this refuses rather than warns: the writers here take a
/// `Validated<P>`, a type that cannot be constructed from an invoice the profile
/// rejects, and they stamp BT-24 from the profile that proved it. A document
/// claiming `XRechnung` because somebody typed the string, having been checked
/// against the bare core, is unrepresentable rather than discouraged.
///
/// ```no_run
/// # use emob_billing::en16931::{Specification, Syntax, write};
/// # fn demo(invoice: &emob_billing::Invoice) -> Result<(), emob_billing::BillingError> {
/// // A German public buyer.
/// let xml = write(invoice, Specification::XRechnung, Syntax::Ubl)?.value;
/// // A private company under `[UStG §14]`, whose access point wants CII.
/// let cii = write(invoice, Specification::Core, Syntax::Cii)?.value;
/// # let _ = (xml, cii);
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// [`BillingError`] from the crossing, or [`BillingError::NotCollectable`]
/// carrying the profile's own findings when the document would be rejected —
/// which is a refusal rather than a warning for the reason the whole workspace
/// draws that line: a document that will come back is not a document, and the
/// findings name the term to fix.
pub fn write(
    invoice: &Invoice,
    specification: Specification,
    syntax: Syntax,
) -> Result<Crossing<String>, BillingError> {
    let crossed = to_en16931(invoice, specification)?;
    let (mut crossing, crossed) = split(crossed);

    let written = match specification {
        Specification::Core => {
            serialise::<en16931::profiles::En16931>(crossed.invoice, specification, syntax)
        }
        Specification::XRechnung => {
            serialise::<en16931::profiles::XRechnung>(crossed.invoice, specification, syntax)
        }
        Specification::PeppolBis3 => {
            serialise::<en16931::profiles::PeppolBis3>(crossed.invoice, specification, syntax)
        }
    }?;

    crossing.note(
        "/Invoice",
        format!(
            "this document is {specification} in {syntax}. The other syntax CEN/TS 16931-2 makes \
             mandatory carries the same semantic invoice, and which one a recipient's access \
             point takes is a fact about the recipient"
        ),
    );
    Ok(crossing.map(|()| written))
}

/// Prove the invoice against one profile and write it in one syntax.
///
/// Generic over the profile marker so the proof and the stamped BT-24 are the
/// same decision — `write_validated` takes `P::PROFILE.specification_id` — and
/// so a specification added to [`Specification`] is a `match` arm rather than a
/// second validation path.
fn serialise<P>(
    invoice: en16931::Invoice,
    specification: Specification,
    syntax: Syntax,
) -> Result<String, BillingError>
where
    P: en16931::validation::profile::ProfileMarker,
{
    let validated =
        en16931::validation::profile::Validated::<P>::new(invoice).map_err(|rejected| {
            BillingError::NotCollectable {
                reason: format!(
                    "this invoice does not satisfy {specification}: {}",
                    rejected
                        .1
                        .fatal()
                        .map(|finding| format!("{} {}", finding.path, finding.rule))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        })?;
    Ok(match syntax {
        Syntax::Ubl => en16931_formats::ubl::write_validated(&validated).xml,
        Syntax::Cii => en16931_formats::cii::write_validated(&validated).xml,
    })
}

/// BG-22 — the totals chain, in the order `BR-CO-10` … `BR-CO-16` states it.
fn totals_of(invoice: &Invoice) -> Result<en16931::invoice::DocumentTotals, BillingError> {
    Ok(en16931::invoice::DocumentTotals {
        line_total: amount(invoice.line_total().amount(), invoice, "line total")?,
        // BT-107 / BT-108, stated only when there is one: `BR-CO-11` and
        // `BR-CO-12` make each the sum of its group, and an explicit zero on a
        // document with no allowances is a figure with nothing behind it.
        allowance_total: non_zero(invoice.allowance_total().amount())
            .map(|total| amount(total, invoice, "allowance total"))
            .transpose()?,
        charge_total: non_zero(invoice.charge_total().amount())
            .map(|total| amount(total, invoice, "charge total"))
            .transpose()?,
        taxable_total: amount(invoice.taxable_total().amount(), invoice, "taxable total")?,
        vat_total: Some(amount(invoice.tax_total().amount(), invoice, "VAT total")?),
        vat_total_accounting: None,
        gross_total: amount(invoice.gross_total().amount(), invoice, "gross total")?,
        paid: None,
        rounding: None,
        due: amount(invoice.gross_total().amount(), invoice, "amount due")?,
    })
}

/// BG-20 / BG-21 — a tariff's minimum or maximum, on the side the totals chain
/// subtracts or adds.
///
/// Stated as a positive magnitude, because a cap put on as a line is a negative
/// BT-146 and `BR-27` refuses the document outright.
fn adjustments(
    invoice: &Invoice,
    mut builder: en16931::invoice::InvoiceBuilder,
) -> Result<en16931::invoice::InvoiceBuilder, BillingError> {
    for adjustment in &invoice.adjustments {
        let entry = DocumentAllowanceCharge {
            amount: amount(adjustment.amount, invoice, "allowance or charge")?,
            base_amount: None,
            percentage: None,
            vat: LineVat {
                category: Code::new(invoice.treatment.category.code()),
                rate: adjustment.vat_rate.map(Percentage::new),
            },
            reason: Some(adjustment.reason.clone()),
            reason_code: None,
        };
        builder = match adjustment.kind {
            DocumentAdjustmentKind::Allowance => builder.allowance(entry),
            DocumentAdjustmentKind::Charge => builder.charge(entry),
        };
    }
    Ok(builder)
}

/// A total worth stating: `None` for zero, which is a figure with nothing
/// behind it on a document that has no allowances or charges.
const fn non_zero(total: Decimal) -> Option<Decimal> {
    if total.is_zero() { None } else { Some(total) }
}

/// Take a crossing apart so a second stage can add to its account.
fn split<T>(crossing: Crossing<T>) -> (Crossing<()>, T) {
    let mut carrier = Crossing::lossless(());
    carrier.absorb_notes("", crossing.notes().to_vec());
    (carrier, crossing.into_value_discarding_notes())
}

fn invoice_line(
    invoice: &Invoice,
    line: &InvoiceLine,
) -> Result<en16931::InvoiceLine, BillingError> {
    // BT-152, absent under `O` — see `InvoiceLine::vat_rate`.
    let vat_rate = line.vat_rate.map(Percentage::new);
    let mut built = en16931::InvoiceLine {
        id: line.id.clone(),
        note: None,
        order_line_reference: None,
        accounting_reference: None,
        object_identifier: None,
        quantity: Quantity::new(line.quantity),
        unit_code: Code::new(line.unit_code()),
        net_amount: amount(line.net, invoice, &format!("line {}", line.id))?,
        period: Some(en16931::invoice::Period {
            start: Some(date_of(
                line.started_at.date(),
                "line period start (BT-134)",
            )?),
            end: Some(date_of(line.ended_at.date(), "line period end (BT-135)")?),
        }),
        allowances: Vec::new(),
        charges: Vec::new(),
        price: PriceDetails {
            net_price: en16931::UnitPriceAmount::new(line.unit_price),
            price_discount: None,
            gross_price: None,
            // BT-149/BT-150: "6.00 EUR per 3600 SEC". Stated only where it is
            // not one, and in the line's own unit code, which `R130` requires.
            base_quantity: (line.base_quantity != Decimal::ONE)
                .then(|| Quantity::new(line.base_quantity)),
            base_quantity_code: (line.base_quantity != Decimal::ONE)
                .then(|| Code::new(line.unit_code())),
        },
        vat: LineVat {
            category: Code::new(invoice.treatment.category.code()),
            rate: vat_rate,
        },
        item: Item {
            name: Some(line.description.clone()),
            description: None,
            seller_identifier: None,
            buyer_identifier: None,
            standard_identifier: None,
            classification_identifiers: Vec::new(),
            origin_country: None,
            attributes: Vec::new(),
        },
    };
    // BT-127, the line's own free text — 0..1, so everything this line has to
    // say has to fit in one sentence.
    //
    // The record it came from is where a dispute starts: a partner holding the
    // invoice can ask for that CDR by name. Beside it, when the register this
    // line was billed from contains compensated loss, the sentence
    // `[REA 6-A §3.2]` requires — and it is required **here**, because the
    // paragraph names *"einem Messwert oder einer Rechnung"* and this is the
    // line stating the measured value (D253).
    built.note = Some(line.compensated_loss.map_or_else(
        || format!("CDR {}", line.cdr),
        |loss| {
            format!(
                "CDR {} — {loss} of this measured value is compensated cable or rectification \
                 loss and is part of the stated value [REA 6-A §3.2]",
                line.cdr
            )
        },
    ));
    Ok(built)
}

/// BG-1, one entry per note the rating owed the **payer**.
///
/// A quantity billed differently from how it was measured — a block rounding
/// that charges for kilowatt-hours nobody delivered, a dimension nothing priced,
/// a line the station's clock could not resolve. Coded `AAI`, UNCL 4451's
/// general information, because BT-21 is what a receiving system routes on and
/// an uncoded note is one it can only display (D253).
///
/// The other half of what the rating had to say never reaches a document: a
/// tariff whose two bounds contradict is a fault the payer did not cause and
/// cannot act on, and it goes to the operator queue instead. The split is
/// `emob_tariff::RatingNote::concerns_the_payer`, asked once where the lines are
/// assembled.
fn payer_notes(
    invoice: &Invoice,
    mut builder: en16931::invoice::InvoiceBuilder,
) -> en16931::invoice::InvoiceBuilder {
    for note in &invoice.notes {
        builder = builder.coded_note(
            en16931::invoice::InvoiceNote::new(format!("{}: {}", note.cdr, note.text))
                .with_subject(GENERAL_INFORMATION),
        );
    }
    builder
}

/// BG-16, from the invoice's own statement of how it will be paid.
fn payment_instructions(details: &PaymentDetails) -> PaymentInstructions {
    let means = match details {
        PaymentDetails::CreditTransfer { iban, holder } => {
            PaymentMeans::CreditTransfer(vec![CreditTransfer {
                account_identifier: Some(iban.clone()),
                account_name: holder.clone(),
                provider_identifier: None,
            }])
        }
        PaymentDetails::DirectDebit {
            mandate_reference,
            creditor_identifier,
            debited_iban,
        } => PaymentMeans::DirectDebit(DirectDebit {
            mandate_reference: Some(mandate_reference.clone()),
            creditor_identifier: Some(creditor_identifier.clone()),
            debited_account: Some(debited_iban.clone()),
        }),
    };
    PaymentInstructions {
        means_code: Some(Code::new(details.means_code())),
        means_text: None,
        remittance_information: None,
        means: Some(means),
    }
}

/// A party, as the document's own VAT category permits it to be stated.
///
/// # The identifier that has to be left off
///
/// Almost every category *wants* a VAT identifier — `BR-AE-2` and `BR-AE-3`
/// refuse a reverse charge without one on both sides, which is why
/// [`crate::TaxTreatment::decide`] refuses to produce that category without
/// them. `O` is the exception and it runs the other way: `BR-O-02` allows
/// **none** — not the seller's BT-31, not a tax representative's BT-63, not the
/// buyer's BT-48 — because a supply outside the scope of the tax is not one any
/// VAT registration is being exercised under.
///
/// A German operator invoicing a reseller established outside the Union
/// therefore omits its own identifier from that document, which is exactly the
/// field a platform would fill in from a customer master without asking.
fn party(counterparty: &Counterparty, category: VatCategory) -> Party {
    let identifiers_allowed = category != VatCategory::OutOfScope;
    Party {
        name: Some(counterparty.name.clone()),
        trading_name: None,
        identifiers: Vec::new(),
        legal_registration: counterparty
            .legal_registration
            .as_ref()
            .map(|(identifier, scheme)| {
                scheme.as_ref().map_or_else(
                    || Identifier::new(identifier.clone()),
                    |scheme| Identifier::schemed(identifier.clone(), scheme.clone()),
                )
            }),
        vat_identifier: identifiers_allowed
            .then(|| counterparty.tax.vat_identifier.clone())
            .flatten(),
        tax_registration: None,
        additional_legal_information: None,
        electronic_address: counterparty
            .electronic_address
            .as_ref()
            .map(|(address, scheme)| Identifier::schemed(address.clone(), scheme.clone())),
        address: PostalAddress {
            line1: counterparty.street.clone(),
            city: Some(counterparty.city.clone()),
            post_code: counterparty.post_code.clone(),
            country: Some(Code::new(counterparty.country.to_ascii_uppercase())),
            ..PostalAddress::default()
        },
        contact: counterparty.contact.as_ref().map_or_else(
            en16931::invoice::Contact::default,
            |contact| en16931::invoice::Contact {
                name: Some(contact.name.clone()),
                phone: Some(contact.phone.clone()),
                email: Some(contact.email.clone()),
            },
        ),
    }
}

/// A calendar day, through `en16931`'s own `time` conversion.
///
/// # A date that will not fit is refused, not replaced
///
/// `en16931::Date` bounds the year to four digits, which every date a charging
/// session carries satisfies — so this cannot fail in practice. The tempting
/// answer when it does is the **epoch**: a domain crate must not abort a
/// billing run over one record, and `1970-01-01` is a fault anybody notices.
///
/// Neither half holds. `1970-01-01` is a perfectly valid `BT-2`, so no validator
/// objects and the document is sendable: an invoice issued fifty-six years ago,
/// or — where the field is `BT-9` — one that fell due before the customer
/// existed. And refusing does not abort anything: every caller here already
/// returns a `Result`, exactly as [`amount`] does for a figure that will not
/// fit. Substituting a date is inventing one on behalf of somebody who will be
/// invoiced for it, which is the repair this workspace refuses at every other
/// seam.
fn date_of(date: time::Date, what: &str) -> Result<Date, BillingError> {
    Date::try_from(date).map_err(|_| BillingError::UnrepresentableDate {
        what: what.to_owned(),
        date: date.to_string(),
    })
}

fn amount(value: Decimal, invoice: &Invoice, what: &str) -> Result<InvoiceAmount, BillingError> {
    InvoiceAmount::try_from(value).map_err(|_| BillingError::UnrepresentableAmount {
        what: what.to_owned(),
        amount: value,
        currency: invoice.currency,
    })
}

#[cfg(test)]
mod tests {
    use super::{Date, date_of};
    use crate::error::BillingError;

    #[test]
    fn a_date_the_standard_cannot_state_is_refused_rather_than_replaced() {
        // It used to fall back to the epoch, on the argument that `1970-01-01`
        // is a fault anybody notices. It is not: it is a perfectly valid BT-2,
        // so no validator objects and the document is sendable — an invoice
        // issued fifty-six years ago, or one that fell due before the customer
        // existed. Substituting a date is inventing one on behalf of somebody
        // who will be invoiced for it, which is the repair this workspace
        // refuses at every other seam.
        // `time` reaches back before the common era; EN 16931 bounds the year
        // to `0..=9999`.
        let before_the_era = time::Date::from_calendar_date(-44, time::Month::March, 15)
            .expect("a `time` date outside EN 16931's four-digit year");
        let err = date_of(before_the_era, "issue date (BT-2)").unwrap_err();
        assert!(
            matches!(err, BillingError::UnrepresentableDate { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("issue date (BT-2)"), "{err}");

        // …and an ordinary date crosses untouched.
        let ordinary = time::Date::from_calendar_date(2026, time::Month::July, 1).unwrap();
        assert_eq!(
            date_of(ordinary, "issue date (BT-2)").unwrap(),
            Date::new(2026, 7, 1).unwrap()
        );
    }
}
