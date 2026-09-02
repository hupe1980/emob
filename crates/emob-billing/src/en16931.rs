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

use emob_core::{Crossing, Money};
use en16931::invoice::{
    Code, CreditTransfer, DirectDebit, Item, LineVat, Party, PaymentInstructions, PaymentMeans,
    PostalAddress, PriceDetails, VatBreakdown,
};
use en16931::validation::ValidationReport;
use en16931::{Date, Identifier, InvoiceAmount, Percentage, Quantity};
use rust_decimal::Decimal;

use crate::error::BillingError;
use crate::invoice::{Counterparty, Invoice, InvoiceLine, PaymentDetails};

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
        date_of(invoice.issued_on),
        COMMERCIAL_INVOICE,
        invoice.currency.as_str(),
    )
    .business_process(BILLING_PROCESS)
    .seller(party(&invoice.seller))
    .buyer(party(&invoice.buyer));

    if let Some(terms) = &invoice.payment_terms {
        builder = builder.payment_terms(terms.clone());
    }
    if let Some(reference) = &invoice.buyer_reference {
        builder = builder.buyer_reference(reference.clone());
    }

    for line in &invoice.lines {
        builder = builder.line(invoice_line(invoice, line)?);
    }

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
            rate: Some(Percentage::new(subtotal.rate)),
            exemption_reason: None,
            exemption_reason_code: None,
        };
        if subtotal.category.needs_exemption_reason() {
            // BT-120. `[UStG §14a]` asks for the same sentence in German law,
            // and `TaxTreatment` is where it was decided rather than invented
            // here.
            entry.exemption_reason.clone_from(&invoice.treatment.reason);
        }
        builder = builder.vat_breakdown(entry);
    }

    let totals = en16931::invoice::DocumentTotals {
        line_total: amount(invoice.line_total().amount(), invoice, "line total")?,
        allowance_total: None,
        charge_total: None,
        taxable_total: amount(invoice.taxable_total().amount(), invoice, "taxable total")?,
        vat_total: Some(amount(invoice.tax_total().amount(), invoice, "VAT total")?),
        vat_total_accounting: None,
        gross_total: amount(invoice.gross_total().amount(), invoice, "gross total")?,
        paid: None,
        rounding: None,
        due: amount(invoice.gross_total().amount(), invoice, "amount due")?,
    };

    let mut built = builder.totals(totals).build();
    built.invoicing_period = Some(en16931::invoice::Period {
        start: Some(date_of(invoice.period_from)),
        end: Some(date_of(invoice.period_to)),
    });
    if let Some(due) = invoice.due_on {
        built.due_date = Some(date_of(due));
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
    let vat_rate = Percentage::new(line.vat_rate);
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
            start: Some(date_of(line.started_at.date())),
            end: Some(date_of(line.ended_at.date())),
        }),
        allowances: Vec::new(),
        charges: Vec::new(),
        price: PriceDetails {
            net_price: en16931::UnitPriceAmount::new(line.unit_price),
            price_discount: None,
            gross_price: None,
            base_quantity: None,
            base_quantity_code: None,
        },
        vat: LineVat {
            category: Code::new(invoice.treatment.category.code()),
            rate: Some(vat_rate),
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

fn party(counterparty: &Counterparty) -> Party {
    Party {
        name: Some(counterparty.name.clone()),
        trading_name: None,
        identifiers: Vec::new(),
        legal_registration: None,
        vat_identifier: counterparty.tax.vat_identifier.clone(),
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
/// # Why this cannot fail in practice, and does not pretend it cannot
///
/// `en16931::Date` bounds the year to four digits, which every date a charging
/// session carries satisfies. The conversion is still fallible in the type
/// system, and the fallback is the epoch rather than a panic: a domain crate
/// that aborts on a corrupt timestamp takes a whole billing run down over one
/// record, and a date of `1970-01-01` on an invoice is a fault every validator
/// and every human notices immediately.
fn date_of(date: time::Date) -> Date {
    Date::try_from(date).unwrap_or_else(|_| Date::new(1970, 1, 1).expect("a literal date"))
}

fn amount(value: Decimal, invoice: &Invoice, what: &str) -> Result<InvoiceAmount, BillingError> {
    InvoiceAmount::try_from(value).map_err(|_| BillingError::UnrepresentableAmount {
        what: what.to_owned(),
        amount: value,
        currency: invoice.currency,
    })
}

/// The gross total as this workspace's own money type, for a caller that has
/// the EN 16931 document and wants the figure back in the vocabulary the rest of
/// the stack speaks.
#[must_use]
pub fn total_of(invoice: &Invoice) -> Money {
    invoice.gross_total()
}
