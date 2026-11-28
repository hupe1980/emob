//! Book each external statement reference once, irrespective of the payer selected for import.
use std::collections::BTreeMap;

use emob_service::{Actor, estate::valid_account_id, journal::Journal};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::ledger::{BAD, Book, Result};
use crate::model::{Booking, Entry, Fact, Held, Import, Statement, valid_reference};

#[derive(Serialize)]
pub(crate) struct Report {
    booked: usize,
    duplicates: usize,
    held: Vec<HeldRow>,
}

#[derive(Serialize)]
struct HeldRow {
    row: usize,
    reason: &'static str,
}

fn issue(entry: &Entry) -> Option<&'static str> {
    match entry.reference.as_deref() {
        None | Some("") => return Some("missing_reference"),
        Some(reference) if !valid_reference(reference) => return Some("invalid_reference"),
        _ => {}
    }
    if entry.reversal {
        Some("reversal")
    } else if entry.status != "booked" {
        Some("not_booked")
    } else if entry.direction != "credit" {
        Some("not_credit")
    } else if entry.amount_minor <= 0 {
        Some("invalid_amount")
    } else {
        None
    }
}

impl<J: Journal> Book<J> {
    pub fn import(&mut self, actor: &Actor, statement: Import) -> Result<Report> {
        if statement.version != 1
            || !valid_account_id(&statement.account)
            || !valid_account_id(&statement.source)
            || statement.entries.is_empty()
            || statement.entries.len() > 100
        {
            return Err(BAD);
        }
        self.live()?;
        let mut batch = Statement {
            issuer: self.state.issuer.clone(),
            account: statement.account,
            currency: statement.currency,
            source: statement.source,
            payments: Vec::new(),
            held: Vec::new(),
        };
        let mut report = Report {
            booked: 0,
            duplicates: 0,
            held: Vec::new(),
        };
        let mut booked = BTreeMap::new();
        let mut new_holds = std::collections::BTreeSet::new();
        let mut first_amount = BTreeMap::new();
        let mut ambiguous = std::collections::BTreeSet::new();
        for entry in &statement.entries {
            if issue(entry).is_none() {
                let reference = entry.reference.as_deref().unwrap_or_default();
                if first_amount
                    .insert(reference.to_owned(), entry.amount_minor)
                    .is_some_and(|old| old != entry.amount_minor)
                {
                    ambiguous.insert(reference.to_owned());
                }
            }
        }
        for (i, entry) in statement.entries.into_iter().enumerate() {
            let reference = entry.reference.as_deref().unwrap_or_default();
            let mut problem = issue(&entry);
            if problem.is_none() && ambiguous.contains(reference) {
                problem = Some("reference_conflict");
            }
            if problem.is_none() {
                let matching = self
                    .state
                    .payments
                    .get(&(batch.source.clone(), reference.to_owned()))
                    .map(|p| {
                        p.account == batch.account
                            && p.currency == batch.currency
                            && p.amount == entry.amount_minor
                    })
                    .or_else(|| booked.get(reference).map(|v| *v == entry.amount_minor));
                match matching {
                    Some(true) => {
                        report.duplicates += 1;
                        continue;
                    }
                    Some(false) => problem = Some("reference_conflict"),
                    None => {
                        booked.insert(reference.to_owned(), entry.amount_minor);
                        batch.payments.push(Booking {
                            reference: reference.to_owned(),
                            amount_minor: entry.amount_minor,
                        });
                        report.booked += 1;
                    }
                }
            }
            if let Some(reason) = problem {
                report.held.push(HeldRow { row: i + 1, reason });
                let identity = serde_json::to_vec(&(
                    &batch.issuer,
                    &batch.account,
                    batch.currency,
                    &batch.source,
                    &entry,
                    reason,
                ))
                .map_err(|_| BAD)?;
                let id = hex::encode(Sha256::digest(identity));
                if !self.state.held.contains_key(&id) && new_holds.insert(id.clone()) {
                    batch.held.push(Held {
                        id,
                        reference: entry.reference.clone().filter(|r| valid_reference(r)),
                        reason: reason.to_owned(),
                        entry,
                    });
                }
            }
        }
        if !(batch.payments.is_empty() && batch.held.is_empty()) {
            self.commit(actor, &Fact::Statement(batch))?;
        }
        Ok(report)
    }
}
