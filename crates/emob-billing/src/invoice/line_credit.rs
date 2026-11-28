//! Credits a monetary amount against one line of a preceding invoice.
use super::{Cancelled, DocumentKind, Invoice, InvoiceLine, TaxSubtotal};
use crate::BillingError;
use emob_core::Money;
use rust_decimal::Decimal;

const fn invalid(reason: &'static str) -> BillingError {
    BillingError::InvalidLineCredit { reason }
}

impl Invoice {
    /// Issues a one-line monetary credit referencing this invoice and its line.
    /// Supply the net and VAT already credited against that line in earlier documents.
    /// Each credit calculates its own VAT and draws on the source line's VAT share.
    ///
    /// # Errors
    ///
    /// Refuses ambiguous lines, document-level adjustments in their tax category,
    /// unsupported precision, and amounts that cannot reconcile to the remaining net and VAT.
    pub fn line_credit(
        &self,
        number: impl Into<String>,
        issued_on: time::Date,
        line_id: &str,
        net: Decimal,
        credited_net: Decimal,
        credited_tax: Decimal,
    ) -> Result<Self, BillingError> {
        if self.kind.is_credit_note() {
            return Err(BillingError::NotCancellable {
                number: self.number.clone(),
            });
        }
        if self.currency.minor_unit_digits() != 2
            || net <= Decimal::ZERO
            || [net, credited_net, credited_tax]
                .iter()
                .any(|v| *v < Decimal::ZERO || v.round_dp(2) != *v)
        {
            return Err(invalid(
                "a positive net and complete minor units are required",
            ));
        }
        let mut matches = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.id == line_id);
        let (index, line) = matches.next().ok_or(invalid("source line is missing"))?;
        if matches.next().is_some() || line.adjusts.is_some() {
            return Err(invalid("source line is ambiguous"));
        }
        let number = number.into();
        if number.trim().is_empty() || number == self.number || issued_on < self.issued_on {
            return Err(invalid("credit requires a new number and valid issue date"));
        }
        let remaining_net = line
            .net
            .checked_sub(credited_net)
            .ok_or(invalid("net exceeds the source line"))?;
        if net > remaining_net {
            return Err(invalid("net exceeds the source line"));
        }

        let (subtotal, tax, available_tax) = self.credit_tax(index, line, net)?;
        let remaining_tax = available_tax
            .checked_sub(credited_tax)
            .ok_or(invalid("VAT exceeds the source line"))?;
        if tax > remaining_tax || (net == remaining_net && tax != remaining_tax) {
            return Err(invalid("credit VAT cannot reconcile to the source line"));
        }
        Ok(Self {
            kind: DocumentKind::CreditNote,
            cancels: Some(Cancelled {
                number: self.number.clone(),
                issued_on: self.issued_on,
            }),
            number,
            issued_on,
            due_on: None,
            payment: None,
            payment_terms: Some("Credit against the preceding invoice.".into()),
            buyer_reference: self.buyer_reference.clone(),
            notes: Vec::new(),
            period_from: self.period_from,
            period_to: self.period_to,
            seller: self.seller.clone(),
            buyer: self.buyer.clone(),
            currency: self.currency,
            exclusive_category: line.vat_category.is_exclusive(),
            lines: vec![InvoiceLine {
                adjusts: Some(line.id.clone()),
                description: format!(
                    "Price adjustment for line {} — {}",
                    line.id, line.description
                ),
                quantity: Decimal::ONE,
                base_quantity: Decimal::ONE,
                unit_price: net,
                exact_net: net,
                net,
                compensated_loss: None,
                ..line.clone()
            }],
            adjustments: Vec::new(),
            tax: vec![TaxSubtotal {
                taxable: net,
                tax,
                ..subtotal.clone()
            }],
            exact_taxable: net,
            payable_rounding: Decimal::ZERO,
        })
    }

    fn credit_tax(
        &self,
        index: usize,
        line: &InvoiceLine,
        net: Decimal,
    ) -> Result<(&TaxSubtotal, Decimal, Decimal), BillingError> {
        let same_category =
            |l: &InvoiceLine| l.vat_category == line.vat_category && l.vat_rate == line.vat_rate;
        if self
            .adjustments
            .iter()
            .any(|a| a.vat_category == line.vat_category && a.vat_rate == line.vat_rate)
        {
            return Err(invalid(
                "a document allowance or charge affects this tax category",
            ));
        }
        let mut groups = self
            .tax
            .iter()
            .filter(|t| t.category == line.vat_category && t.rate == line.vat_rate);
        let group = groups
            .next()
            .ok_or(invalid("source VAT breakdown is missing"))?;
        if groups.next().is_some() {
            return Err(invalid("source VAT breakdown is ambiguous"));
        }
        let rate = line.vat_rate.unwrap_or_default();
        let vat = |amount: Decimal| {
            amount
                .checked_mul(rate)
                .and_then(|v| v.checked_div(Decimal::ONE_HUNDRED))
                .map(|v| Money::new(v, self.currency).round_to_minor_unit().amount())
                .ok_or(invalid("taxable amount is out of range"))
        };
        // The source calculates VAT over the entire category (BR-CO-17).
        // Differences of running rounded totals allocate its cents exactly once.
        let before = self.lines[..index]
            .iter()
            .filter(|l| same_category(l))
            .try_fold(Decimal::ZERO, |total, l| total.checked_add(l.net))
            .ok_or(invalid("source taxable amount is out of range"))?;
        let through = before
            .checked_add(line.net)
            .ok_or(invalid("source taxable amount is out of range"))?;
        if through > group.taxable || vat(group.taxable)? != group.tax || rate < Decimal::ZERO {
            return Err(invalid("source line cannot reconcile to its VAT breakdown"));
        }
        Ok((group, vat(net)?, vat(through)? - vat(before)?))
    }
}
