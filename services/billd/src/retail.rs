//! Private retail invoices. The books retain the customer invoice separately from partner settlement.

mod adjustment;
mod calculation;
mod customer;
mod feed;
mod http;
mod withdrawal;
pub use adjustment::LineCredit;
pub(crate) use http::router;
pub use withdrawal::WithdrawalCredit;

use std::collections::{BTreeMap, BTreeSet};

use emob_billing::invoice::Invoice;
use emob_core::{Currency, Money, PartyId};
use emob_service::estate::{RetailAccount, RetailOffer};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, Month, OffsetDateTime};

/// Evidence attached to a CDR received by the private billing receiver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Captured agreement, empty on an ordinary card.
    pub agreement: String,
    /// Full retail offer digest at authorization.
    pub reference: String,
    /// Contract carried by the same signed delivery.
    pub emaid: String,
}

#[derive(Deserialize)]
struct Profile {
    account: RetailAccount,
    agreements: Vec<Agreement>,
}

#[derive(Deserialize)]
struct Agreement {
    #[serde(rename = "agreement")]
    id: String,
    emaid: String,
    account: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
    offer: RetailOffer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Line {
    #[serde(default)]
    document_lines: Vec<String>,
    kind: String,
    agreement: String,
    source: String,
    offer: String,
    net: String,
    tax: String,
    gross: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hold {
    source: String,
    reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Summary {
    account: String,
    period: String,
    state: String,
    review: Option<String>,
    number: Option<String>,
    supersedes: Option<String>,
    credit: Option<String>,
    currency: Currency,
    net: String,
    tax: String,
    gross: String,
    rounding: String,
    payable: String,
    lines: Vec<Line>,
    withheld: Vec<Hold>,
}

impl Summary {
    fn empty(account: &str, period: &str) -> Self {
        Self {
            account: account.to_owned(),
            period: period.to_owned(),
            state: "held".into(),
            review: None,
            number: None,
            supersedes: None,
            credit: None,
            currency: Currency::EUR,
            net: money(Decimal::ZERO),
            tax: money(Decimal::ZERO),
            gross: money(Decimal::ZERO),
            rounding: money(Decimal::ZERO),
            payable: money(Decimal::ZERO),
            lines: Vec::new(),
            withheld: Vec::new(),
        }
    }
    fn hold(&mut self, source: &str, reason: &str) {
        self.withheld.push(Hold {
            source: source.to_owned(),
            reason: reason.to_owned(),
        });
    }
}

fn money(amount: Decimal) -> String {
    format!(
        "{:.2}",
        Money::new(amount, Currency::EUR)
            .round_to_minor_unit()
            .amount()
    )
}

/// An issued retail invoice and any credit reversing its predecessor, committed as one fact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    provider: PartyId,
    /// The immutable original semantic document.
    pub(crate) document: Invoice,
    xml: String,
    /// Original bytes and number of the credit note issued with a replacement.
    reversal: Option<ArchivedCredit>,
    summary: Summary,
    content: String,
    confirmation: String,
    confirmed_by: emob_service::Actor,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ArchivedCredit {
    number: String,
    xml: String,
}

#[derive(Default, Debug)]
pub(crate) struct Ledger {
    issues: BTreeMap<String, Issue>,
    latest: BTreeMap<(PartyId, String, String), String>,
    account_documents: BTreeMap<(PartyId, String), Vec<String>>,
    adjustments: adjustment::Adjustments,
    withdrawals: BTreeMap<String, WithdrawalCredit>,
    credited_invoices: BTreeMap<String, String>,
    credit_issues: BTreeMap<String, String>,
    agreement_receipts: BTreeMap<String, BTreeSet<(OffsetDateTime, String)>>,
    // In journal order, including a reversal immediately before its replacement.
    document_numbers: Vec<String>,
}

impl Ledger {
    pub(crate) fn apply(&mut self, issue: Issue) {
        let documents = self
            .account_documents
            .entry((issue.provider.clone(), issue.summary.account.clone()))
            .or_default();
        if let Some(credit) = &issue.reversal {
            self.document_numbers.push(credit.number.clone());
            documents.push(credit.number.clone());
            self.credit_issues
                .insert(credit.number.clone(), issue.document.number.clone());
        }
        self.document_numbers.push(issue.document.number.clone());
        documents.push(issue.document.number.clone());
        self.latest.insert(
            (
                issue.provider.clone(),
                issue.summary.account.clone(),
                issue.summary.period.clone(),
            ),
            issue.document.number.clone(),
        );
        self.issues.insert(issue.document.number.clone(), issue);
    }

    pub(crate) fn index_receipt(&mut self, binding: &Binding, end: OffsetDateTime, key: String) {
        self.agreement_receipts
            .entry(binding.agreement.clone())
            .or_default()
            .insert((end, key));
    }

    fn archived(&self, number: &str) -> Option<(&Issue, &str)> {
        if let Some(credit) = self.adjustments.get(number) {
            return Some((self.issues.get(credit.original())?, &credit.xml));
        }
        if let Some(credit) = self.withdrawals.get(number) {
            return Some((self.issues.get(&credit.original)?, &credit.xml));
        }
        if let Some(issue) = self.issues.get(number) {
            return Some((issue, &issue.xml));
        }
        let issue = self.issues.get(self.credit_issues.get(number)?)?;
        Some((issue, &issue.reversal.as_ref()?.xml))
    }

    fn latest(&self, provider: &PartyId, account: &str, period: &str) -> Option<&Issue> {
        self.latest
            .get(&(provider.clone(), account.to_owned(), period.to_owned()))
            .and_then(|number| self.issues.get(number))
    }
    fn number(&self, provider: &PartyId, offset: u64) -> String {
        format!(
            "MSP-{}-{:08}",
            provider,
            self.document_numbers.len() as u64 + offset
        )
    }
}

#[derive(Serialize)]
struct Document {
    #[serde(skip)]
    issuer: PartyId,
    account: String,
    period: String,
    number: String,
    currency: Currency,
    payable: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
}

impl Ledger {
    fn document_row(&self, number: &str) -> Option<Document> {
        let (issue, _) = self.archived(number)?;
        let summary = &issue.summary;
        if let Some(credit) = self.adjustments.get(number) {
            return Some(Document {
                issuer: issue.provider.clone(),
                account: summary.account.clone(),
                period: summary.period.clone(),
                currency: summary.currency,
                number: number.to_owned(),
                payable: money(Decimal::new(credit.receipt.gross_minor, 2)),
                kind: "line_credit",
                supersedes: Some(issue.document.number.clone()),
            });
        }
        if let Some(credit) = self.withdrawals.get(number) {
            return Some(Document {
                issuer: issue.provider.clone(),
                account: summary.account.clone(),
                period: summary.period.clone(),
                number: credit.number.clone(),
                currency: summary.currency,
                payable: summary.payable.clone(),
                kind: "credit",
                supersedes: Some(credit.original.clone()),
            });
        }
        let credit = issue.document.number != number;
        let kind = if credit {
            "credit"
        } else if summary.supersedes.is_some() {
            "replacement"
        } else {
            "invoice"
        };
        let payable = if credit {
            &self
                .issues
                .get(summary.supersedes.as_ref()?)?
                .summary
                .payable
        } else {
            &summary.payable
        };
        Some(Document {
            issuer: issue.provider.clone(),
            account: summary.account.clone(),
            period: summary.period.clone(),
            number: number.to_owned(),
            currency: summary.currency,
            payable: payable.clone(),
            kind,
            supersedes: summary.supersedes.clone(),
        })
    }
}

#[derive(Clone, Copy)]
struct Period {
    start: Date,
    next: Date,
}

impl Period {
    fn parse(value: &str) -> Option<Self> {
        if value.len() != 7
            || value.as_bytes()[4] != b'-'
            || !value
                .bytes()
                .take(4)
                .chain(value.bytes().skip(5))
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let year: i32 = value[..4].parse().ok()?;
        if !(1..=9998).contains(&year) {
            return None;
        }
        let month = Month::try_from(value[5..].parse::<u8>().ok()?).ok()?;
        let start = Date::from_calendar_date(year, month, 1).ok()?;
        let next =
            Date::from_calendar_date(year + i32::from(month == Month::December), month.next(), 1)
                .ok()?;
        Some(Self { start, next })
    }
    fn end(self) -> Date {
        self.next
            .previous_day()
            .expect("a calendar month follows its first day")
    }
    fn contains(self, at: OffsetDateTime) -> bool {
        self.start.midnight().assume_utc() <= at && at < self.next.midnight().assume_utc()
    }
}
