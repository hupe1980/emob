//! Collecting what the invoice asks for.
//!
//! # One instruction per invoice, and the clock is an argument
//!
//! `sepa` writes the pain.008 and validates the IBAN, the BIC and the creditor
//! identifier against the registries that define them, so nothing here re-reads
//! ISO 20022. What lives here is the mapping from an invoice to a collection —
//! and one rule the wrapped crate cannot enforce for us.
//!
//! **Its builders default several fields off the system clock**: a collection
//! date five business days out, a message timestamp of *now*. Both are
//! reasonable defaults for an application and neither is usable here. A domain
//! crate that reads a clock cannot be replayed (`just purity`), and a collection
//! file that differs between two runs of one billing job is a file no bank
//! reconciles and no auditor can check. So every one of them is an argument at
//! this seam, and a test asserts that the same
//! inputs produce the same bytes.
//!
//! # Amounts are minor units, and that is not a conversion
//!
//! SEPA counts in cents — `i64`, never a float — and so does everything on an
//! invoice here, because the line amounts were rounded to the currency's minor
//! unit before the document was built. The conversion is therefore exact by the
//! time it happens, and [`PaymentError::NotAWholeMinorUnit`] is the assertion
//! that it was, rather than a rounding step wearing a conversion's clothes.

use rust_decimal::Decimal;
use sepa::{Bic, CreditorId, DirectDebitEntry, DirectDebitGroup, Iban, IsoDate, Pain008Builder};

use crate::invoice::Invoice;

/// The creditor's side of a mandate — everything a collection needs that is not
/// on the invoice.
#[derive(Debug, Clone)]
pub struct Creditor {
    /// The name the debtor will see on their statement.
    pub name: String,
    /// The account the money lands in.
    pub iban: Iban,
    /// The bank, when it is known. Optional since SEPA went IBAN-only.
    pub bic: Option<Bic>,
    /// The EPC creditor identifier — AT-02 — which every direct debit carries.
    pub creditor_id: CreditorId,
}

/// The debtor's side: the mandate the customer signed.
#[derive(Debug, Clone)]
pub struct Mandate {
    /// The mandate reference — AT-01.
    pub reference: String,
    /// The day it was signed — AT-25.
    pub signed_on: IsoDate,
    /// The account holder's name, as it appears on the mandate.
    pub debtor_name: String,
    /// The account to draw from.
    pub debtor_iban: Iban,
}

/// A collection built from one invoice.
#[derive(Debug)]
pub struct Collection {
    /// The pain.008 document.
    pub xml: String,
    /// What it draws, in the currency's minor unit.
    pub amount_minor: i64,
}

/// Build the direct-debit instruction that collects an invoice.
///
/// Every date and identifier is an argument — see the module documentation for
/// why none of them may come from a clock.
///
/// # Errors
///
/// [`PaymentError`] when the invoice's total is not a whole number of minor
/// units, when it is not positive, or when `sepa`'s own validation refuses the
/// instruction.
pub fn instruct(
    invoice: &Invoice,
    creditor: &Creditor,
    mandate: &Mandate,
    collect_on: IsoDate,
    created_at: sepa::IsoDateTime,
) -> Result<Collection, PaymentError> {
    let total = invoice.gross_total().amount();
    let amount_minor = minor_units(total, invoice.currency.minor_unit_digits())?;
    if amount_minor <= 0 {
        return Err(PaymentError::NothingToCollect { amount: total });
    }

    let mut entry = DirectDebitEntry::new(
        mandate.reference.clone(),
        mandate.signed_on,
        mandate.debtor_name.clone(),
        mandate.debtor_iban.clone(),
        amount_minor,
        // The end-to-end reference is the invoice number, so a return lands
        // back on the document rather than on a batch.
        invoice.number.clone(),
    );
    entry.remittance = Some(sepa::RemittanceInfo::Unstructured(format!(
        "Invoice {}",
        invoice.number
    )));

    let mut group =
        DirectDebitGroup::new(creditor.name.clone(), &creditor.iban, &creditor.creditor_id)
            .collection_date(collect_on)
            .add_entry(entry);
    if let Some(bic) = &creditor.bic {
        group = group.creditor_bic(bic.clone());
    }

    let builder = Pain008Builder::new(creditor.name.clone())
        .msg_id(format!("EMOB-{}", invoice.number))
        .created_at(created_at)
        .add_group(group);

    let xml = builder
        .build()
        .map_err(|error| PaymentError::Refused(error.to_string()))?;
    Ok(Collection { xml, amount_minor })
}

/// An exact amount as whole minor units.
///
/// Exact by the time it is called: every figure on an invoice this crate builds
/// was rounded to the currency's minor unit at the line. A value with a further
/// decimal came from somewhere else, and rounding it here would be this crate
/// deciding a cent on somebody's behalf at the last possible moment.
fn minor_units(amount: Decimal, digits: u32) -> Result<i64, PaymentError> {
    use rust_decimal::prelude::ToPrimitive as _;

    let scaled = amount * Decimal::from(10_u64.pow(digits));
    if scaled.fract() != Decimal::ZERO {
        return Err(PaymentError::NotAWholeMinorUnit { amount });
    }
    scaled
        .to_i64()
        .ok_or(PaymentError::NotAWholeMinorUnit { amount })
}

/// Why an invoice could not be collected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PaymentError {
    /// The total is not a whole number of minor units.
    #[error(
        "{amount} is not a whole number of minor units: SEPA counts in cents and every figure on \
         an invoice this crate builds is already rounded to one, so this came from elsewhere"
    )]
    NotAWholeMinorUnit {
        /// The figure.
        amount: Decimal,
    },

    /// There is nothing to collect.
    #[error(
        "this invoice comes to {amount}: a direct debit collects a positive amount, and a credit is refunded rather than drawn"
    )]
    NothingToCollect {
        /// The total.
        amount: Decimal,
    },

    /// `sepa` refused the instruction.
    #[error("the collection is not one a bank would accept: {0}")]
    Refused(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn an_exact_amount_becomes_whole_minor_units() {
        assert_eq!(minor_units(dec("31.04"), 2).unwrap(), 3104);
        assert_eq!(
            minor_units(dec("31.0400"), 2).unwrap(),
            3104,
            "scale is not precision"
        );
        // The yen has no minor unit and the dinar has three, and a hard-coded
        // two would invent a hundredth of one and throw away a fils.
        assert_eq!(minor_units(dec("1235"), 0).unwrap(), 1235);
        assert_eq!(minor_units(dec("1.235"), 3).unwrap(), 1235);
    }

    #[test]
    fn a_figure_that_is_not_a_whole_minor_unit_is_refused_rather_than_rounded() {
        // Every amount on an invoice this crate builds was rounded at the line.
        // One that arrives with a further decimal came from somewhere else, and
        // rounding it here would be this crate deciding a cent at the last
        // possible moment on somebody's behalf.
        let err = minor_units(dec("31.045"), 2).unwrap_err();
        assert!(
            matches!(err, PaymentError::NotAWholeMinorUnit { .. }),
            "{err}"
        );
    }
}
