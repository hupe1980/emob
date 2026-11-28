//! A balance is the fold of immutable documents, accepted bank entries and named allocations.
mod offset;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::http::StatusCode;
use emob_core::{Currency, PartyId};
use emob_service::estate::valid_account_id;
use emob_service::journal::Journal;
use emob_service::vault::DataKey;
use emob_service::{Actor, Readiness};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::model::{
    Accepted, Allocation, Document, Fact, Held, Kind, OffsetReceipt, Preview, Statement,
};

pub(crate) type Result<T> = std::result::Result<T, StatusCode>;
pub(crate) const CONFLICT: StatusCode = StatusCode::CONFLICT;
pub(crate) const OFFLINE: StatusCode = StatusCode::SERVICE_UNAVAILABLE;
pub(crate) const BAD: StatusCode = StatusCode::BAD_REQUEST;

#[derive(serde::Serialize)]
pub(crate) struct Item {
    pub doc: Document,
    #[serde(skip)]
    root: String,
    /// The cumulative cash assigned to this document and its predecessors.
    paid: i64,
    incoming: i64,
    outgoing: i64,
    revision: u64,
    credited: i64,
    cancelled: bool,
    replacement: bool,
}

impl Item {
    fn current(&self) -> bool {
        !self.replacement && matches!(self.doc.kind, Kind::Invoice | Kind::Replacement)
    }

    fn due(&self) -> i64 {
        if !self.current() {
            return 0;
        }
        let face = if self.cancelled {
            0
        } else {
            self.doc.amount_minor - self.credited
        };
        signed_due(face, self.paid, self.incoming, self.outgoing)
            .expect("the document transition checked the resulting balance")
    }

    fn open(&self) -> i64 {
        self.due().max(0)
    }
}

pub(crate) fn signed_due(face: i64, paid: i64, incoming: i64, outgoing: i64) -> Result<i64> {
    i64::try_from(i128::from(face) - i128::from(paid) - i128::from(incoming) + i128::from(outgoing))
        .map_err(|_| CONFLICT)
}

#[derive(serde::Serialize)]
pub(crate) struct Payment {
    pub account: String,
    pub currency: Currency,
    pub amount: i64,
    pub available: i64,
}

type SourceRef = (String, String);

#[derive(Clone, Copy, Default)]
struct Totals {
    net: i64,
    cash: i64,
    open: i64,
}

impl Totals {
    fn add(&mut self, net: i64, cash: i64, open: i64) -> Result<()> {
        let updated = Self {
            net: self.net.checked_add(net).ok_or(CONFLICT)?,
            cash: self.cash.checked_add(cash).ok_or(CONFLICT)?,
            open: self.open.checked_add(open).ok_or(CONFLICT)?,
        };
        updated.open.checked_sub(updated.net).ok_or(CONFLICT)?;
        *self = updated;
        Ok(())
    }
}

pub(crate) struct Ledger {
    totals: BTreeMap<(String, Currency), Totals>,
    pub issuer: PartyId,
    pub last: Option<Document>,
    pub documents: BTreeMap<String, Item>,
    pub payments: BTreeMap<SourceRef, Payment>,
    pub held: BTreeMap<String, (String, Currency, String, Held)>,
    pub allocations: BTreeMap<String, Accepted>,
    offsets: BTreeMap<String, OffsetReceipt>,
}

impl Ledger {
    pub fn new(issuer: PartyId) -> Self {
        Self {
            issuer,
            last: None,
            totals: BTreeMap::new(),
            documents: BTreeMap::new(),
            payments: BTreeMap::new(),
            held: BTreeMap::new(),
            allocations: BTreeMap::new(),
            offsets: BTreeMap::new(),
        }
    }

    pub fn cursor(&self) -> u64 {
        self.last.as_ref().map_or(0, |d| d.position)
    }

    fn receive(&mut self, doc: &Document) -> Result<()> {
        if doc.issuer != self.issuer
            || Some(doc.position) != self.cursor().checked_add(1)
            || !valid_account_id(&doc.account)
            || doc.amount_minor < 0
            || doc.number.is_empty()
            || doc.number.len() > 96
            || !doc
                .number
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"*._/-".contains(&b))
            || self.documents.contains_key(&doc.number)
        {
            return Err(CONFLICT);
        }
        let scope = (doc.account.clone(), doc.currency);
        let mut totals = self.totals.get(&scope).copied().unwrap_or_default();
        let mut item = Item {
            doc: doc.clone(),
            root: doc.number.clone(),
            paid: 0,
            incoming: 0,
            outgoing: 0,
            revision: 1,
            credited: 0,
            cancelled: false,
            replacement: false,
        };
        match (doc.kind, doc.predecessor.as_ref()) {
            (Kind::Invoice, None) => {
                totals.add(doc.amount_minor, 0, doc.amount_minor)?;
            }
            (kind @ (Kind::Credit | Kind::LineCredit | Kind::Replacement), Some(number)) => {
                let prior = self.documents.get_mut(number).ok_or(CONFLICT)?;
                if matches!(prior.doc.kind, Kind::Credit | Kind::LineCredit)
                    || prior.doc.account != doc.account
                    || prior.doc.currency != doc.currency
                {
                    return Err(CONFLICT);
                }
                item.root.clone_from(&prior.root);
                let next_revision = prior.revision.checked_add(1).ok_or(CONFLICT)?;
                if kind == Kind::LineCredit {
                    if prior.cancelled
                        || doc.amount_minor == 0
                        || doc.amount_minor > prior.doc.amount_minor - prior.credited
                    {
                        return Err(CONFLICT);
                    }
                    signed_due(
                        prior.doc.amount_minor - prior.credited - doc.amount_minor,
                        prior.paid,
                        prior.incoming,
                        prior.outgoing,
                    )?;
                    totals.add(-doc.amount_minor, 0, -prior.open().min(doc.amount_minor))?;
                    prior.credited += doc.amount_minor;
                    prior.revision = next_revision;
                } else if kind == Kind::Credit {
                    if prior.cancelled
                        || prior.credited != 0
                        || prior.doc.amount_minor != doc.amount_minor
                    {
                        return Err(CONFLICT);
                    }
                    signed_due(0, prior.paid, prior.incoming, prior.outgoing)?;
                    totals.add(-doc.amount_minor, 0, -prior.open())?;
                    prior.cancelled = true;
                    prior.revision = next_revision;
                } else {
                    if !prior.cancelled || prior.replacement {
                        return Err(CONFLICT);
                    }
                    let due =
                        signed_due(doc.amount_minor, prior.paid, prior.incoming, prior.outgoing)?;
                    totals.add(doc.amount_minor, 0, due.max(0) - prior.open())?;
                    prior.replacement = true;
                    item.paid = prior.paid;
                    item.incoming = prior.incoming;
                    item.outgoing = prior.outgoing;
                    item.revision = next_revision;
                }
            }
            _ => return Err(CONFLICT),
        }
        self.totals.insert(scope, totals);
        self.documents.insert(doc.number.clone(), item);
        self.last = Some(doc.clone());
        Ok(())
    }

    fn statement(&mut self, batch: &Statement) -> Result<()> {
        if batch.issuer != self.issuer
            || !valid_account_id(&batch.account)
            || !valid_account_id(&batch.source)
        {
            return Err(CONFLICT);
        }
        let mut seen = BTreeSet::new();
        let mut cash = 0_i64;
        for p in &batch.payments {
            let id = (batch.source.clone(), p.reference.clone());
            cash = cash.checked_add(p.amount_minor).ok_or(CONFLICT)?;
            if p.amount_minor <= 0
                || !crate::model::valid_reference(&p.reference)
                || self.payments.contains_key(&id)
                || !seen.insert(&p.reference)
            {
                return Err(CONFLICT);
            }
        }
        self.totals
            .entry((batch.account.clone(), batch.currency))
            .or_default()
            .add(0, cash, 0)?;
        for p in &batch.payments {
            self.payments.insert(
                (batch.source.clone(), p.reference.clone()),
                Payment {
                    account: batch.account.clone(),
                    currency: batch.currency,
                    amount: p.amount_minor,
                    available: p.amount_minor,
                },
            );
        }
        for h in &batch.held {
            self.held.insert(
                h.id.clone(),
                (
                    batch.account.clone(),
                    batch.currency,
                    batch.source.clone(),
                    h.clone(),
                ),
            );
        }
        Ok(())
    }

    pub fn amounts(&self, request: &Allocation) -> Result<(i64, i64)> {
        if request.amount_minor <= 0 {
            return Err(BAD);
        }
        let doc = self.documents.get(&request.number).ok_or(CONFLICT)?;
        let payment = self
            .payments
            .get(&(request.source.clone(), request.reference.clone()))
            .ok_or(CONFLICT)?;
        if doc.doc.account != request.account
            || payment.account != request.account
            || doc.doc.currency != request.currency
            || payment.currency != request.currency
            || request.amount_minor > doc.open()
            || request.amount_minor > payment.available
        {
            return Err(CONFLICT);
        }
        Ok((
            doc.open() - request.amount_minor,
            payment.available - request.amount_minor,
        ))
    }

    fn allocate(&mut self, a: &Accepted, actor: &Actor) -> Result<()> {
        let (open, available) = self.amounts(&a.allocation)?;
        if actor != &a.actor
            || self.allocations.contains_key(&a.result.review)
            || open != a.result.open_after_minor
            || available != a.result.unapplied_after_minor
        {
            return Err(CONFLICT);
        }
        let request = &a.allocation;
        let item = &self.documents[&request.number];
        let revision = item.revision.checked_add(1).ok_or(CONFLICT)?;
        let paid = item
            .paid
            .checked_add(request.amount_minor)
            .ok_or(CONFLICT)?;
        self.totals
            .get_mut(&(request.account.clone(), request.currency))
            .ok_or(CONFLICT)?
            .add(
                -request.amount_minor,
                -request.amount_minor,
                -request.amount_minor,
            )?;
        let doc = self.documents.get_mut(&request.number).ok_or(CONFLICT)?;
        doc.paid = paid;
        doc.revision = revision;
        self.payments
            .get_mut(&(request.source.clone(), request.reference.clone()))
            .ok_or(CONFLICT)?
            .available = available;
        self.allocations.insert(a.result.review.clone(), a.clone());
        Ok(())
    }

    fn apply(&mut self, fact: &Fact, actor: &Actor, key: &DataKey) -> Result<()> {
        match (fact, actor) {
            (Fact::Document { document }, Actor::Service(name)) if name == "billd" => {
                self.receive(document)
            }
            (Fact::Statement(statement), Actor::Operator(_)) => self.statement(statement),
            (Fact::Allocation(allocation), Actor::Operator(_)) => self.allocate(allocation, actor),
            (Fact::Offset(receipt), Actor::Operator(_)) => {
                self.check_offset(actor, receipt, key)?;
                self.post_offset(receipt);
                Ok(())
            }
            _ => Err(CONFLICT),
        }
    }

    pub fn proof(&self) -> Value {
        // JSON arrays preserve the sorted compound keys without using them as object keys.
        let values = (
            &self.documents,
            self.payments.iter().collect::<Vec<_>>(),
            &self.held,
            &self.allocations,
            &self.offsets,
        );
        let canonical =
            serde_json::to_vec(&values).expect("ledger contains only serializable values");
        json!({"cursor":self.cursor(),"documents":self.documents.len(),"payments":self.payments.len(),
            "held": self.held.len(), "allocations":self.allocations.len(),"offsets":self.offsets.len(),
            "digest":hex::encode(Sha256::digest(&canonical))})
    }
}

pub(crate) struct Book<J> {
    pub state: Ledger,
    journal: J,
    key: Arc<DataKey>,
    pub ready: Readiness,
}

impl<J: Journal> Book<J> {
    pub fn open(
        issuer: PartyId,
        journal: J,
        key: Arc<DataKey>,
        ready: Readiness,
    ) -> anyhow::Result<Self> {
        let mut state = Ledger::new(issuer);
        for row in journal.records()? {
            let fact: Fact = serde_json::from_str(&row.body)?;
            anyhow::ensure!(fact.kind() == row.kind, "accounting journal kind mismatch");
            state
                .apply(&fact, &row.actor, &key)
                .map_err(|_| anyhow::anyhow!("inconsistent accounting journal at {}", row.seq))?;
        }
        ready.up("ledger");
        Ok(Self {
            state,
            journal,
            key,
            ready,
        })
    }

    pub fn commit(&mut self, actor: &Actor, fact: &Fact) -> Result<()> {
        self.live()?;
        let body = serde_json::to_string(fact).map_err(|_| OFFLINE)?;
        if let Fact::Offset(receipt) = fact {
            self.state.check_offset(actor, receipt, &self.key)?;
        } else {
            self.state.apply(fact, actor, &self.key)?;
        }
        if self.journal.append(fact.kind(), actor, &body).is_err() {
            self.ready.set(
                "ledger",
                emob_service::health::Probe::down("storage acknowledgement unavailable"),
            );
            return Err(OFFLINE);
        }
        if let Fact::Offset(receipt) = fact {
            self.state.post_offset(receipt);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn into_journal(self) -> J {
        self.journal
    }

    pub fn live(&self) -> Result<()> {
        if self.ready.is_ready() {
            Ok(())
        } else {
            Err(OFFLINE)
        }
    }

    pub fn review(&self, actor: &Actor, request: &Allocation) -> Result<Preview> {
        let (open_after_minor, unapplied_after_minor) = self.state.amounts(request)?;
        let bytes = serde_json::to_vec(&(
            actor,
            request,
            &self.state.issuer,
            open_after_minor,
            unapplied_after_minor,
        ))
        .map_err(|_| OFFLINE)?;
        Ok(Preview {
            review: self
                .key
                .digest("accountingd/allocation-review/v1", &bytes)
                .hex()
                .to_owned(),
            open_after_minor,
            unapplied_after_minor,
        })
    }

    pub fn receipt(
        &self,
        actor: &Actor,
        allocation: &Allocation,
        review: &str,
    ) -> Result<Option<Preview>> {
        self.live()?;
        Ok(self
            .state
            .allocations
            .get(review)
            .filter(|entry| &entry.actor == actor && &entry.allocation == allocation)
            .map(|entry| entry.result.clone()))
    }

    pub fn confirm(
        &mut self,
        actor: &Actor,
        allocation: Allocation,
        review: &str,
    ) -> Result<Preview> {
        if let Some(previous) = self.receipt(actor, &allocation, review)? {
            return Ok(previous);
        }
        let result = self.review(actor, &allocation)?;
        if result.review != review {
            return Err(CONFLICT);
        }
        let fact = Fact::Allocation(Accepted {
            allocation,
            actor: actor.clone(),
            result: result.clone(),
        });
        self.commit(actor, &fact)?;
        Ok(result)
    }
}

fn page<I: Iterator>(rows: I, offset: usize, more: &mut bool) -> Vec<I::Item> {
    let mut rows = rows.skip(offset);
    let selected = rows.by_ref().take(50).collect();
    *more |= rows.next().is_some();
    selected
}

impl Ledger {
    pub(crate) fn booked_effects(&self, basis: &crate::customer::Basis) -> Result<(i64, i64, i64)> {
        let Some(item) = self.documents.get(&basis.number) else {
            // Only a replacement can inherit an allocation before its own booking arrives.
            return if basis.replacement {
                Err(CONFLICT)
            } else {
                Ok((0, 0, 0))
            };
        };
        if item.doc.account != basis.account
            || item.doc.currency != basis.currency
            || item.doc.amount_minor != basis.amount_minor
            || item.credited > basis.credited_minor
            || item.cancelled
            || matches!(item.doc.kind, Kind::Credit | Kind::LineCredit)
        {
            return Err(OFFLINE);
        }
        Ok((item.paid, item.incoming, item.outgoing))
    }

    pub(crate) fn has_account(&self, account: &str) -> bool {
        self.totals.keys().any(|(id, _)| id == account)
    }

    pub(crate) fn allocation_evidence(
        &self,
        account: &str,
        currency: Currency,
        number: &str,
        page_number: u32,
    ) -> Result<Value> {
        let item = self
            .documents
            .get(number)
            .filter(|i| i.doc.account == account && i.doc.currency == currency)
            .ok_or(StatusCode::NOT_FOUND)?;
        let selected = self
            .allocations
            .values()
            .filter(|a| {
                self.documents
                    .get(&a.allocation.number)
                    .is_some_and(|i| i.root == item.root)
            })
            .map(allocation_row);
        let mut more = false;
        let allocations = page(
            selected,
            (page_number as usize).saturating_mul(50),
            &mut more,
        );
        let offsets = self.offsets.values().filter(|r| {
            [&r.selection.source_number, &r.selection.target_number]
                .iter()
                .any(|n| self.documents[*n].root == item.root)
        });
        let offsets = page(
            offsets,
            (page_number as usize).saturating_mul(50),
            &mut more,
        );
        Ok(json!({
            "number": number, "account": account, "currency": currency,
            "open_minor": item.open(), "due_minor": item.due(),
            "incoming_minor": item.incoming, "outgoing_minor": item.outgoing,
            "allocations": allocations, "offsets": offsets,
            "next_page": if more {page_number.checked_add(1)} else {None},
        }))
    }

    pub(crate) fn balance(&self, account: &str, currency: Currency, page_number: u32) -> Value {
        let totals = self
            .totals
            .get(&(account.to_owned(), currency))
            .copied()
            .unwrap_or_default();
        let offset = (page_number as usize).saturating_mul(50);
        let mut more = false;
        let docs = self
            .documents
            .values()
            .filter(|item| item.doc.account == account && item.doc.currency == currency)
            .map(|item| {
                json!({
                    "number": item.doc.number, "kind": item.doc.kind,
                    "amount_minor": item.doc.amount_minor, "predecessor": item.doc.predecessor,
                    "open_minor": item.open(), "due_minor": item.due(),
                    "incoming_minor": item.incoming, "outgoing_minor": item.outgoing,
                })
            });
        let payments = self.payments.iter().filter(|(_,p)| p.account == account && p.currency == currency).map(|((source,reference),p)| {
            json!({"source":source,"reference":reference,"amount_minor":p.amount,"unapplied_minor":p.available})
        });
        let held = self.held.values().filter(|(a,c,_,_)| a==account && *c==currency).map(|(_,_,source,h)| {
            json!({"source":source,"reference":h.reference,"reason":h.reason,"amount_minor":h.entry.amount_minor})
        });
        let allocations = self
            .allocations
            .values()
            .filter(|a| a.allocation.account == account && a.allocation.currency == currency)
            .map(allocation_row);
        let offsets = self
            .offsets
            .values()
            .filter(|o| o.selection.account == account && o.selection.currency == currency);
        json!({
            "issuer": self.issuer, "account": account, "currency": currency,
            "net_due_minor": totals.net, "credit_minor": totals.open - totals.net,
            "unapplied_minor": totals.cash,
            "can_offset": totals.open > 0 && totals.open > totals.net,
            "can_allocate": totals.open > 0 && totals.cash > 0,
            "documents": page(docs, offset, &mut more),
            "payments": page(payments, offset, &mut more),
            "held": page(held, offset, &mut more),
            "allocations": page(allocations, offset, &mut more),
            "offsets": page(offsets, offset, &mut more),
            "next_page": if more {page_number.checked_add(1)} else {None},
        })
    }
}

fn allocation_row(a: &Accepted) -> Value {
    let r = &a.allocation;
    json!({"source":r.source,"reference":r.reference,"number":r.number,"amount_minor":r.amount_minor,"actor":a.actor})
}
