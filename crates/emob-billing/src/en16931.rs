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
//! [`ValidationReport`] together, and [`xrechnung`] refuses to produce a
//! document that its profile rejects: `Validated<XRechnung>` is a type that
//! cannot be constructed from an invalid invoice, which is the same discipline
//! `Evidence::billable_energy` applies to a kilowatt-hour one layer down.
//!
//! # What EN 16931 cannot carry, and is told
//!
//! | Fact | Why it does not cross |
//! |---|---|
//! | the signed meter records | there is no business term for evidence. `BT-18` carries *an* object identifier and the digests are many |
//! | the quarter-hour periods | a line is a quantity and a price; the settlement grid is not an invoice concept |
//! | which tariff version priced it | `BT-127`, the free-text line note, is the only place it fits, and it goes there |
//! | a rating note | the same — and a note that stayed behind is a note nobody can invoke |

use emob_core::Crossing;
use en16931::invoice::{
    Code, CreditTransfer, DirectDebit, DocumentAllowanceCharge, Item, LineVat, Party,
    PaymentInstructions, PaymentMeans, PostalAddress, PriceDetails, VatBreakdown,
};
use en16931::validation::ValidationReport;
use en16931::{Date, Identifier, InvoiceAmount, Percentage, Quantity};
use rust_decimal::Decimal;

use crate::error::BillingError;
use crate::invoice::{Counterparty, DocumentAdjustmentKind, Invoice, InvoiceLine, PaymentDetails};
use crate::tax::VatCategory;

/// The CEN core specification identifier — BT-24.
pub const CEN_CORE: &str = "urn:cen.eu:en16931:2017";
/// `XRechnung` 3.0's, which a German public buyer requires.
pub const XRECHNUNG_3: &str =
    "urn:cen.eu:en16931:2017#compliant#urn:xoev-de:kosit:standard:xrechnung_3.0";
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
/// judged by — [`CEN_CORE`] for a partner settlement, [`XRECHNUNG_3`] for a
/// German public buyer. It is an argument because it is a fact about the
/// *recipient*, and this crate has no way to know one.
///
/// # Errors
///
/// [`BillingError::UnrepresentableAmount`] when a figure will not fit an
/// EN 16931 amount — which cannot happen for an invoice this crate built, and
/// can for one that arrived over a wire.
pub fn to_en16931(
    invoice: &Invoice,
    specification: &str,
) -> Result<Crossing<Crossed>, BillingError> {
    let mut crossing = Crossing::lossless(());

    let mut builder = en16931::Invoice::builder(
        specification,
        invoice.number.clone(),
        date_of(invoice.issued_on, "issue date (BT-2)")?,
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

/// The invoice as an `XRechnung` 3.0 UBL document.
///
/// # Errors
///
/// [`BillingError`] from the crossing, or [`BillingError::NotCollectable`]
/// carrying the profile's own findings when the document would be rejected —
/// which is a refusal rather than a warning for the reason the whole workspace
/// draws that line: a document that will come back is not a document, and the
/// findings name the term to fix.
pub fn xrechnung(invoice: &Invoice) -> Result<Crossing<String>, BillingError> {
    let crossed = to_en16931(invoice, XRECHNUNG_3)?;
    let (mut crossing, crossed) = split(crossed);

    let validated = en16931::validation::profile::Validated::<en16931::profiles::XRechnung>::new(
        crossed.invoice,
    )
    .map_err(|rejected| BillingError::NotCollectable {
        reason: format!(
            "this invoice does not satisfy XRechnung 3.0: {}",
            rejected
                .1
                .fatal()
                .map(|finding| format!("{} {}", finding.path, finding.rule))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })?;

    let written = en16931_formats::ubl::write_validated(&validated);
    crossing.note(
        "/Invoice",
        "this document is UBL. CII is the other syntax CEN/TS 16931-2 makes mandatory and a \
         recipient may require it instead",
    );
    Ok(crossing.map(|()| written.xml))
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
    // BT-127. The one place the record this line came from fits, and it is
    // where a dispute starts: a partner holding the invoice can ask for that
    // CDR by name.
    built.note = Some(format!("CDR {}", line.cdr));
    Ok(built)
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
