//! Build a candidate semantic invoice from the two owners' evidence.
use std::collections::BTreeMap;

use emob_billing::{Counterparty, InvoiceBuilder, Specification, Subscription, Syntax, TaxStatus};
use emob_cdr::Cdr;
use emob_core::{Currency, Money, PartyId};
use emob_service::estate::{Billing, RetailAccountKind};
use rust_decimal::Decimal;
use sha2::{Digest as _, Sha256};

use super::{Agreement, Invoice, Line, Period, Profile, Summary, money};

pub(super) struct Candidate {
    pub summary: Summary,
    pub doc: Option<Invoice>,
    pub content: String,
}

fn digest(value: &impl serde::Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("invoice facts are serializable");
    hex::encode(Sha256::digest(bytes))
}

impl Candidate {
    pub(super) fn confirmation(
        &self,
        actor: &emob_service::Actor,
        previous: Option<&str>,
    ) -> String {
        digest(&("retail-confirm/1", actor, &self.content, previous))
    }
}

struct Item<'a> {
    document_lines: Vec<String>,
    agreement: &'a Agreement,
    source: String,
    monthly: bool,
    gross: Decimal,
    net: Decimal,
}

fn rounded(value: Decimal) -> Decimal {
    Money::new(value, Currency::EUR)
        .round_to_minor_unit()
        .amount()
}

fn rated(
    agreement: &Agreement,
    received: &crate::Received,
    provider: &PartyId,
    rate: Decimal,
) -> Result<Cdr, &'static str> {
    let original = &received.cdr;
    let terms = &agreement.offer.terms;
    let bound = received.retail.as_ref().ok_or("missing_authorization")?;
    if bound.agreement != agreement.id
        || bound.emaid != agreement.emaid
        || bound.reference != agreement.offer.reference
        || &received.partner != provider
        || original.key.party != terms.operator
        || agreement.at > original.started_at
        || agreement
            .ended_at
            .is_some_and(|end| original.started_at >= end)
        || !agreement.offer.matches(
            &original.evse_id,
            provider,
            &terms.code,
            original.started_at,
        )
        || !original.was_priced_with(&terms.base)
    {
        return Err("authorization_mismatch");
    }
    if terms.fees.vat != rate || terms.point.country_code() != "DE" {
        return Err("unsupported_tax_evidence");
    }
    if original.supersedes.is_some() {
        return Err("cdr_correction_requires_review");
    }
    if original
        .evidence
        .as_ref()
        .is_none_or(|e| !e.energy_billable || e.payload_digests.is_empty())
        || !emob_cdr::validate(original).is_settleable()
    {
        return Err("unverified_evidence");
    }
    original
        .rerated_with(&terms.retail_tariff().map_err(|_| "unsupported_offer")?)
        .map_err(|_| "unrateable_evidence")
}

fn unsupported_account(seller: &Billing, profile: &Profile, expected_account: &str) -> bool {
    profile.account.id != expected_account
        || seller.country != "DE"
        || profile.account.country != "DE"
        || (profile.account.kind == RetailAccountKind::Fleet && profile.account.vat_id.is_none())
        || seller.vat_id.trim().is_empty()
        || seller.street.as_ref().is_none_or(|s| s.trim().is_empty())
        || seller
            .post_code
            .as_ref()
            .is_none_or(|s| s.trim().is_empty())
        || emob_service::estate::validate_retail_accounts(std::slice::from_ref(&profile.account))
            .is_err()
}

pub(super) fn prepare(
    state: &crate::Billd,
    provider: &PartyId,
    seller: &Billing,
    input: &super::http::Request,
    period: Period,
    profile: &Profile,
) -> Candidate {
    let rate = Decimal::from(19); // Domestic standard-rated retail supply (UStG §12(1)).
    let mut summary = Summary::empty(&input.account, &input.period);
    let mut records = Vec::new();
    let mut subs = Vec::new();
    let mut items = Vec::new();
    if unsupported_account(seller, profile, &input.account) {
        summary.hold(&input.account, "unsupported_tax_evidence");
    }
    let mut by_id = BTreeMap::new();
    let mut by_emaid = BTreeMap::new();
    let mut ordered: Vec<_> = profile.agreements.iter().collect();
    ordered.sort_unstable_by_key(|ag| &ag.id);
    for ag in ordered {
        if ag.account != input.account
            || ag.offer.terms.provider != *provider
            || by_id.insert(&ag.id, ag).is_some()
            || by_emaid.insert(&ag.emaid, ag).is_some()
        {
            summary.hold(&input.account, "conflicting_agreement");
            continue;
        }
        match monthly(ag, rate, period) {
            Ok(Some((sub, item))) => {
                subs.push(sub);
                items.push(item);
            }
            Ok(None) => {}
            Err(reason) => summary.hold(&ag.id, reason),
        }
    }
    for received in state.received().filter(|r| period.contains(r.cdr.ended_at)) {
        let Some(binding) = received.retail.as_ref() else {
            continue;
        };
        let Some(&ag) = by_emaid
            .get(&binding.emaid)
            .or_else(|| by_id.get(&binding.agreement))
        else {
            continue;
        };
        let key = received.cdr.key.to_string();
        match rated(ag, received, provider, rate) {
            Ok(record) => {
                let gross = record
                    .cost
                    .as_ref()
                    .expect("rerating returns a rated CDR")
                    .gross()
                    .amount();
                items.push(Item {
                    document_lines: Vec::new(),
                    agreement: ag,
                    source: key,
                    monthly: false,
                    gross: rounded(gross),
                    net: Decimal::ZERO,
                });
                records.push(record);
            }
            Err(reason) => summary.hold(&key, reason),
        }
    }
    let mut doc = None;
    if items.is_empty() && summary.withheld.is_empty() {
        summary.hold(&input.account, "no_billable_items");
    }
    if !items.is_empty() {
        match build(seller, profile, period, rate, &records, subs, &mut items) {
            Some(invoice) => {
                totals(&mut summary, &items, rate, &invoice);
                if super::feed::minor_units(invoice.currency, invoice.payable_total().amount())
                    .is_none()
                {
                    summary.hold(&input.account, "payable_out_of_range");
                }
                doc = Some(invoice);
            }
            None => summary.hold(&input.account, "invalid_invoice"),
        }
    }
    if summary.withheld.is_empty() {
        summary.state = "ready".into();
    }
    let content = digest(&("retail-month/1", provider, &doc, &summary));
    Candidate {
        summary,
        doc,
        content,
    }
}

fn totals(summary: &mut Summary, items: &[Item<'_>], rate: Decimal, invoice: &Invoice) {
    summary.net = money(invoice.taxable_total().amount());
    summary.tax = money(invoice.tax_total().amount());
    summary.gross = money(invoice.gross_total().amount());
    summary.rounding = money(invoice.payable_rounding().amount());
    summary.payable = money(invoice.payable_total().amount());
    let mut net_running = Decimal::ZERO;
    for item in items {
        let next_net = net_running + item.net;
        let tax = rounded(next_net * rate / Decimal::ONE_HUNDRED)
            - rounded(net_running * rate / Decimal::ONE_HUNDRED);
        summary.lines.push(Line {
            document_lines: item.document_lines.clone(),
            kind: if item.monthly { "monthly" } else { "session" }.into(),
            source: item.source.clone(),
            agreement: item.agreement.id.clone(),
            offer: item.agreement.offer.reference.clone(),
            net: money(item.net),
            tax: money(tax),
            gross: money(item.net + tax),
        });
        net_running = next_net;
    }
}

fn monthly(
    ag: &Agreement,
    rate: Decimal,
    period: Period,
) -> Result<Option<(Subscription, Item<'_>)>, &'static str> {
    let terms = &ag.offer.terms;
    let first = period.start.midnight().assume_utc();
    let following = period.next.midnight().assume_utc();
    let ends = terms.until().into_iter().chain(ag.ended_at).min();
    if ag.at.max(terms.from()) >= following || ends.is_some_and(|at| at <= first) {
        return Ok(None);
    }
    if terms.fees.vat != rate
        || terms.point.country_code() != "DE"
        || terms.retail_tariff().is_err()
        || ag.offer.reference != terms.reference()
    {
        return Err("unsupported_offer");
    }
    let fee = terms.fees.monthly;
    if fee.is_zero() {
        return Ok(None);
    }
    if ag.at.max(terms.from()) > first || ends.is_some_and(|end| end < following) {
        return Err("partial_month");
    }
    if rounded(fee) != fee {
        return Err("fractional_monthly_fee");
    }
    let net = rounded(fee / (Decimal::ONE + rate / Decimal::ONE_HUNDRED));
    let subscription = Subscription::new(
        format!("{} — {}", terms.name, ag.emaid),
        net,
        period.start,
        period.end(),
    );
    Ok(Some((
        subscription,
        Item {
            document_lines: Vec::new(),
            agreement: ag,
            source: ag.id.clone(),
            monthly: true,
            gross: fee,
            net,
        },
    )))
}

fn build(
    seller: &Billing,
    profile: &Profile,
    period: Period,
    rate: Decimal,
    records: &[Cdr],
    subscriptions: Vec<Subscription>,
    items: &mut [Item<'_>],
) -> Option<Invoice> {
    let acct = &profile.account;
    let tax = match acct.kind {
        RetailAccountKind::Private => TaxStatus::consumer(&acct.country),
        RetailAccountKind::Fleet => TaxStatus::business(&acct.country, acct.vat_id.as_ref()?),
    };
    let mut buyer =
        Counterparty::new(&acct.name, &acct.city, tax).at(&acct.street, &acct.post_code);
    if let Some(address) = &acct.electronic_address {
        buyer = buyer.reachable_at(address, "EM");
    }
    let mut builder = InvoiceBuilder::new(
        "PREVIEW",
        period.next,
        (period.start, period.end()),
        crate::daemon::counterparty(seller),
        buyer,
    )
    .subscription_currency(Currency::EUR)
    .supplied_from("DE", rate)
    .buyer_reference(&acct.id)
    .payment_terms("Payment by separate arrangement.");
    for cdr in records {
        builder = builder.record(cdr);
    }
    for sub in subscriptions {
        builder = builder.subscription(sub);
    }
    let mut invoice = builder.build().ok()?.value;
    let mut by_record = BTreeMap::new();
    let mut monthly = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if item.monthly {
            monthly.push(index);
        } else if by_record.insert(item.source.clone(), index).is_some() {
            return None;
        }
    }
    // The builder emits subscription lines in the order the subscriptions were added.
    let mut subscriptions = monthly.into_iter();
    for line in &invoice.lines {
        let item = match &line.cdr {
            Some(key) => {
                let item = &mut items[*by_record.get(&key.to_string())?];
                item.net += line.net;
                item
            }
            None => &mut items[subscriptions.next()?],
        };
        item.document_lines.push(line.id.clone());
    }
    if subscriptions.next().is_some() || items.iter().any(|item| item.document_lines.is_empty()) {
        return None;
    }
    let agreed: Decimal = items.iter().map(|i| i.gross).sum();
    invoice.payable_rounding = agreed - invoice.gross_total().amount();
    emob_billing::en16931::write(&invoice, Specification::Core, Syntax::Ubl).ok()?;
    Some(invoice)
}
