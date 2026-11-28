//! Apply a document credit and reduce a debit under one ledger lock.
use emob_service::{Actor, journal::Journal, vault::DataKey};

use super::{BAD, Book, CONFLICT, Ledger, OFFLINE, Result};
use crate::model::{Fact, Offset, OffsetReceipt};

impl Ledger {
    fn offset_quote(
        &self,
        actor: &Actor,
        request: &Offset,
        key: &DataKey,
    ) -> Result<OffsetReceipt> {
        if request.amount_minor <= 0 {
            return Err(BAD);
        }
        let source = self.documents.get(&request.source_number).ok_or(CONFLICT)?;
        let target = self.documents.get(&request.target_number).ok_or(CONFLICT)?;
        if !matches!(actor, Actor::Operator(_))
            || source.root == target.root
            || [source, target].iter().any(|item| {
                !item.current()
                    || item.doc.account != request.account
                    || item.doc.currency != request.currency
            })
        {
            return Err(CONFLICT);
        }
        let source_before_minor = source.due();
        let target_before_minor = target.due();
        if source_before_minor > -request.amount_minor || target_before_minor < request.amount_minor
        {
            return Err(CONFLICT);
        }
        let source_after_minor = source_before_minor + request.amount_minor;
        let target_after_minor = target_before_minor - request.amount_minor;
        // Check all counters before persisting a fact whose application must succeed.
        source
            .outgoing
            .checked_add(request.amount_minor)
            .ok_or(CONFLICT)?;
        target
            .incoming
            .checked_add(request.amount_minor)
            .ok_or(CONFLICT)?;
        source.revision.checked_add(1).ok_or(CONFLICT)?;
        target.revision.checked_add(1).ok_or(CONFLICT)?;
        let bytes = serde_json::to_vec(&(
            actor,
            &self.issuer,
            request,
            source.revision,
            target.revision,
            source_before_minor,
            target_before_minor,
        ))
        .map_err(|_| OFFLINE)?;
        let review = key
            .digest("accountingd/offset-review/v1", &bytes)
            .hex()
            .to_owned();
        Ok(OffsetReceipt {
            selection: request.clone(),
            issuer: self.issuer.clone(),
            actor: actor.clone(),
            source_revision: source.revision,
            target_revision: target.revision,
            source_before_minor,
            target_before_minor,
            source_after_minor,
            target_after_minor,
            review,
        })
    }

    pub(super) fn check_offset(
        &self,
        actor: &Actor,
        receipt: &OffsetReceipt,
        key: &DataKey,
    ) -> Result<()> {
        if self.offsets.contains_key(&receipt.review)
            || self.offset_quote(actor, &receipt.selection, key)? != *receipt
        {
            return Err(CONFLICT);
        }
        Ok(())
    }

    pub(super) fn post_offset(&mut self, receipt: &OffsetReceipt) {
        let r = &receipt.selection;
        let source = self
            .documents
            .get_mut(&r.source_number)
            .expect("validated source");
        source.outgoing += r.amount_minor;
        source.revision += 1;
        let target = self
            .documents
            .get_mut(&r.target_number)
            .expect("validated target");
        target.incoming += r.amount_minor;
        target.revision += 1;
        self.totals
            .get_mut(&(r.account.clone(), r.currency))
            .expect("documents have an account")
            .open -= r.amount_minor;
        self.offsets.insert(receipt.review.clone(), receipt.clone());
    }
}

impl<J: Journal> Book<J> {
    pub fn offset_review(&self, actor: &Actor, request: &Offset) -> Result<OffsetReceipt> {
        self.live()?;
        self.state.offset_quote(actor, request, &self.key)
    }

    pub fn offset_receipt(
        &self,
        actor: &Actor,
        request: &Offset,
        review: &str,
    ) -> Result<Option<OffsetReceipt>> {
        self.live()?;
        Ok(self
            .state
            .offsets
            .get(review)
            .filter(|r| &r.actor == actor && &r.selection == request)
            .cloned())
    }

    pub fn confirm_offset(
        &mut self,
        actor: &Actor,
        request: &Offset,
        review: &str,
    ) -> Result<OffsetReceipt> {
        if let Some(receipt) = self.offset_receipt(actor, request, review)? {
            return Ok(receipt);
        }
        let receipt = self.offset_review(actor, request)?;
        if receipt.review != review {
            return Err(CONFLICT);
        }
        self.commit(actor, &Fact::Offset(receipt.clone()))?;
        Ok(receipt)
    }
}
