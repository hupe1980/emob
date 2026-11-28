//! The fifth thing `billd` decides: that what it decided outlives the process.
//!
//! Every rule in [`crate::Billd`] is about a **pair** — what was true before a
//! call and what may be true after — which is why they are here rather than in
//! `emob-billing`. A number is spent *once*; a period closes *once*; a
//! correction is a reversal *then* a replacement; a document is booked *after*
//! the recipient took it. Each is a fact about the past, and a counter that
//! starts again at one hands a second document the number the first carries,
//! which `[UStG §14(4) Nr. 4]` forbids by name.
//!
//! [`Books`] is [`Billd`] with an [`emob_service::journal::Journal`] under it:
//! every decision is written down as a fact — `issued`, `submitted`, `booked` —
//! and the service on the way up is the fold of those facts. Facts and not
//! commands, because a command log re-decides at replay, and a platform whose
//! invoice numbers move when a tariff is corrected is not one anybody can keep
//! books with.
//!
//! # The two windows
//!
//! The record is written **after** the decision and before the caller is told.
//! Killed between the two, both are lost: the state that decided died with it,
//! so the next start replays a journal that does not mention a document nobody
//! received. That is the safe direction.
//!
//! A journal that **refuses** a write is the other one. The state has advanced
//! and the record has not, so the service **halts** — [`BooksError::Halted`] on
//! every later call — because a platform that keeps issuing invoices it cannot
//! account for is worse than one that stops and says so.

use emob_billing::invoice::Invoice;
use emob_service::Actor;
use emob_service::journal::{Folded, Journal, JournalError, Kinded};

use crate::{Billd, ChartOfAccounts, ClosingError, Issued, Numbering, Recorded, Submission};

/// One fact `billd` decided.
///
/// Serialised into the journal, and folded back on the way up. Every variant
/// carries everything the fold needs: an [`Event::Issued`] holds the document
/// itself, because an invoice rebuilt at replay from records that have since
/// been re-rated is a different invoice under the same number.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// A document was numbered and built.
    Issued {
        /// The period it closes, where it closes one. A *Stornorechnung*
        /// closes none.
        period: Option<String>,
        /// The number `[UStG §14(4) Nr. 4]` spends once.
        number: String,
        /// The document.
        invoice: Box<Invoice>,
        /// What it reverses, for a credit note.
        cancels: Option<String>,
        /// What it replaces, for the second half of a re-billing.
        replaces: Option<String>,
        /// The specification it was issued in (spec 010); a journal written
        /// before 0.3 holds none and reads as the EN 16931 core.
        #[serde(default = "core")]
        specification: emob_billing::Specification,
        /// The XML syntax chosen at issue, including after configuration changes.
        delivery_syntax: emob_billing::Syntax,
        /// The subscribers its `de.emob.invoice.issued` event is owed to.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        owed_to: Vec<String>,
    },
    /// The recipient's platform answered.
    Submitted {
        /// Which document.
        number: String,
        /// What the platform said.
        submission: Submission,
    },
    /// A subscriber answered a document's `de.emob.invoice.issued` event.
    Announced {
        /// Which document.
        number: String,
        /// Which subscriber.
        to: String,
        /// Why it refused; `None` when it took it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },
    /// A document was posted into the books.
    Booked {
        /// Which document.
        number: String,
    },
    /// Immutable retail invoice, with the credit document when a late record corrected an issue.
    RetailIssued(Box<crate::retail::Issue>),
    /// A separately authorized credit against one fully attributable retail invoice.
    WithdrawalCredited(Box<crate::retail::WithdrawalCredit>),
    /// A numbered, reviewed credit against one attributable original UBL line.
    LineCredited(Box<crate::retail::LineCredit>),
    /// A rated record arrived from `csmsd`, owed by `partner` for `period`.
    Received {
        /// Authorization binding supplied only to the private billing receiver.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retail: Option<crate::retail::Binding>,
        /// The record, whole: the invoice line is built from this value.
        cdr: Box<emob_cdr::Cdr>,
        /// Who pays it — the issuer of the contract it was charged under.
        partner: emob_core::PartyId,
        /// The closing it belongs to, `<CC*PID>/<YYYY-MM>`.
        period: String,
        /// The driver's receipt link, which the invoice line carries (spec 003).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<String>,
    },
}

impl Kinded for Event {
    fn kind(&self) -> &'static str {
        match self {
            Self::RetailIssued(_) => "retail_issued",
            Self::LineCredited(_) => "line_credited",
            Self::WithdrawalCredited(_) => "withdrawal_credited",
            Self::Issued { .. } => "issued",
            Self::Submitted { .. } => "submitted",
            Self::Announced { .. } => "announced",
            Self::Booked { .. } => "booked",
            Self::Received { .. } => "received",
        }
    }
}

const fn core() -> emob_billing::Specification {
    emob_billing::Specification::Core
}

/// What went wrong that was not about a document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BooksError {
    /// The same CDR key arrived with different commercial or measured facts.
    #[error("conflicting delivery for a retained record")]
    ConflictingDelivery,
    /// The closing itself was refused.
    ///
    /// Boxed because it is the large half of a small enum, and every call here
    /// returns this type.
    #[error(transparent)]
    Closing(Box<ClosingError>),

    /// The journal could not take the record.
    #[error("the journal refused the record for {number}: {source}")]
    Journal {
        /// Which document the service had just decided on.
        number: String,
        /// Why.
        source: JournalError,
    },

    /// The service has stopped, because its state is ahead of its journal.
    ///
    /// Not a transient failure and deliberately not retryable here: the process
    /// holds decisions it cannot account for, and the answer is to fix the disk
    /// and restart, which replays the journal and rebuilds a state that matches
    /// it exactly.
    #[error(
        "billd has halted: {why} — the service decided something its journal could not record, \
         so it holds state no replay can reproduce. Restarting rebuilds from the journal, which \
         is the only state this platform can account for"
    )]
    Halted {
        /// What the journal said when it refused.
        why: String,
    },
}

impl From<ClosingError> for BooksError {
    fn from(source: ClosingError) -> Self {
        Self::Closing(Box::new(source))
    }
}

/// [`Billd`], folded from a journal and extended by appending to it.
///
/// Every method that writes a fact takes `by` first: the record names who
/// caused it, so a document in the books answers "on whose word" as well as
/// "what" — and the caller says so, because only the caller knows whether a
/// month closed by the calendar or by an operator's hand.
pub struct Books<J: Journal> {
    folded: Folded<Billd, Event, J>,
    halted: Option<String>,
    subscribers: Vec<String>,
}

impl<J: Journal> core::fmt::Debug for Books<J> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Books")
            .field("documents", &self.folded.state().documents().count())
            .field("halted", &self.halted)
            .finish_non_exhaustive()
    }
}

impl<J: Journal> Books<J> {
    /// Rebuild the service from its journal.
    ///
    /// The chart of accounts is configuration rather than a fact, and is
    /// supplied here rather than replayed: it decides which account a role
    /// lands in and an operator that changes it means the change to apply.
    /// Every posting the journal already holds keeps the account it was written
    /// to, because [`doubleentry`] is append-only and a replay of `booked` is
    /// idempotent on the invoice number.
    ///
    /// # Errors
    ///
    /// [`JournalError::Broken`] when the journal does not chain, and
    /// [`JournalError::Encoding`] when it holds a fact this build cannot read.
    /// Both refuse to start, which is the only answer available: a service that
    /// began from a journal it could not read would be inventing the past.
    pub fn open(
        ledger: &str,
        numbering: Numbering,
        chart: ChartOfAccounts,
        journal: J,
    ) -> Result<Self, JournalError> {
        let empty = Billd::new(ledger, numbering).posting_to(chart);
        Ok(Self {
            folded: Folded::open(empty, journal, Billd::apply)?,
            halted: None,
            subscribers: Vec::new(),
        })
    }

    /// The folded service, for every question that does not change it.
    #[must_use]
    pub const fn state(&self) -> &Billd {
        self.folded.state()
    }

    /// The journal under it.
    #[must_use]
    pub const fn journal(&self) -> &J {
        self.folded.journal()
    }

    /// Whether the service has stopped deciding.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.halted.is_some()
    }

    fn guard(&self) -> Result<(), BooksError> {
        match &self.halted {
            None => Ok(()),
            Some(why) => Err(BooksError::Halted { why: why.clone() }),
        }
    }

    /// Record a fact as `by`'s, halting the service if the journal will not
    /// take it.
    fn commit(&mut self, by: &Actor, event: &Event, number: &str) -> Result<(), BooksError> {
        match self.folded.note(by, event) {
            Ok(_) => Ok(()),
            Err(source) => {
                self.halted = Some(source.to_string());
                Err(BooksError::Journal {
                    number: number.to_owned(),
                    source,
                })
            }
        }
    }

    /// Issue the invoice that closes a period, and write down that it was.
    ///
    /// # Errors
    ///
    /// [`BooksError::Closing`] for a period already closed, and
    /// [`BooksError::Journal`] where the decision could not be recorded — after
    /// which the service is halted.
    pub fn issue(
        &mut self,
        by: &Actor,
        period: &str,
        invoice: impl FnOnce(String) -> Result<Invoice, emob_billing::BillingError>,
    ) -> Result<String, BooksError> {
        self.issue_as(by, period, emob_billing::Specification::Core, invoice)
    }

    /// [`Self::issue`], in a stated EN 16931 specification.
    ///
    /// # Errors
    ///
    /// As [`Self::issue`].
    pub fn issue_as(
        &mut self,
        by: &Actor,
        period: &str,
        specification: emob_billing::Specification,
        invoice: impl FnOnce(String) -> Result<Invoice, emob_billing::BillingError>,
    ) -> Result<String, BooksError> {
        self.guard()?;
        let issued = self
            .folded
            .state_mut()
            .issue_as(period, specification, invoice)?;
        let delivery_syntax = issued.delivery_syntax;
        let number = issued.invoice.number.clone();
        let invoice = Box::new(issued.invoice.clone());
        self.folded.state_mut().owe(&number, &self.subscribers);
        let event = Event::Issued {
            period: Some(period.to_owned()),
            number: number.clone(),
            invoice,
            cancels: None,
            replaces: None,
            specification,
            delivery_syntax,
            owed_to: self.subscribers.clone(),
        };
        self.commit(by, &event, &number)?;
        Ok(number)
    }

    /// Cancel a document and re-bill the period it closed.
    ///
    /// Two facts, in the order the correction has: the reversal, then the
    /// replacement. A journal that held the second without the first would
    /// replay a month with a replacement standing beside an original nothing
    /// reversed, which is the failure D307 names.
    ///
    /// # Errors
    ///
    /// [`BooksError::Closing`] for a document that was never issued, already
    /// cancelled, or is itself a credit note; [`BooksError::Journal`] otherwise.
    pub fn rebill(
        &mut self,
        by: &Actor,
        number: &str,
        cancelled_on: time::Date,
        reason: impl Into<String>,
        replacement: impl FnOnce(String) -> Result<Invoice, emob_billing::BillingError>,
    ) -> Result<(String, String), BooksError> {
        self.guard()?;
        let (credit, replaced) =
            self.folded
                .state_mut()
                .rebill(number, cancelled_on, reason, replacement)?;
        let period = self.folded.state().period_closed_by(&replaced);
        let (credit_doc, replacement_doc) = {
            let state = self.folded.state();
            (
                state
                    .issued(&credit)
                    .map(|d| (d.invoice.clone(), d.specification, d.delivery_syntax)),
                state
                    .issued(&replaced)
                    .map(|d| (d.invoice.clone(), d.specification, d.delivery_syntax)),
            )
        };
        self.folded.state_mut().owe(&credit, &self.subscribers);
        self.folded.state_mut().owe(&replaced, &self.subscribers);
        if let Some((invoice, specification, delivery_syntax)) = credit_doc {
            self.commit(
                by,
                &Event::Issued {
                    period: None,
                    number: credit.clone(),
                    invoice: Box::new(invoice),
                    cancels: Some(number.to_owned()),
                    replaces: None,
                    specification,
                    delivery_syntax,
                    owed_to: self.subscribers.clone(),
                },
                &credit,
            )?;
        }
        if let Some((invoice, specification, delivery_syntax)) = replacement_doc {
            self.commit(
                by,
                &Event::Issued {
                    period,
                    number: replaced.clone(),
                    invoice: Box::new(invoice),
                    cancels: None,
                    replaces: Some(number.to_owned()),
                    specification,
                    delivery_syntax,
                    owed_to: self.subscribers.clone(),
                },
                &replaced,
            )?;
        }
        Ok((credit, replaced))
    }

    /// Owe every document issued from now on to these subscribers as well.
    #[must_use]
    pub fn delivering_to(mut self, subscribers: Vec<String>, syntax: emob_billing::Syntax) -> Self {
        self.folded.state_mut().delivery_syntax = syntax;
        self.subscribers = subscribers;
        self
    }

    /// Record a subscriber's answer to a document's event.
    ///
    /// # Errors
    ///
    /// [`BooksError::Journal`] where the fact could not be recorded.
    pub fn announced(
        &mut self,
        by: &Actor,
        number: &str,
        to: &str,
        refused: Option<String>,
    ) -> Result<(), BooksError> {
        self.guard()?;
        self.folded.state_mut().answer(number, to, refused.clone());
        self.commit(
            by,
            &Event::Announced {
                number: number.to_owned(),
                to: to.to_owned(),
                refused,
            },
            number,
        )
    }

    /// Record that the recipient's platform took a document.
    ///
    /// # Errors
    ///
    /// [`BooksError::Closing`] for a number never issued.
    pub fn accepted(
        &mut self,
        by: &Actor,
        number: &str,
        at: time::Date,
        reference: Option<String>,
    ) -> Result<(), BooksError> {
        self.guard()?;
        self.folded
            .state_mut()
            .accepted(number, at, reference.clone())?;
        self.commit(
            by,
            &Event::Submitted {
                number: number.to_owned(),
                submission: Submission::Accepted { at, reference },
            },
            number,
        )
    }

    /// Record that the recipient's platform refused one.
    ///
    /// # Errors
    ///
    /// [`BooksError::Closing`] for a number never issued or already booked.
    pub fn rejected(
        &mut self,
        by: &Actor,
        number: &str,
        at: time::Date,
        reason: impl Into<String>,
    ) -> Result<(), BooksError> {
        self.guard()?;
        let reason = reason.into();
        self.folded
            .state_mut()
            .rejected(number, at, reason.clone())?;
        self.commit(
            by,
            &Event::Submitted {
                number: number.to_owned(),
                submission: Submission::Rejected { at, reason },
            },
            number,
        )
    }

    /// Post an accepted document into the books.
    ///
    /// # Errors
    ///
    /// [`BooksError::Closing`] for every reason [`Billd::book`] gives, and
    /// [`BooksError::Journal`] where the fact could not be recorded.
    pub fn book(&mut self, by: &Actor, number: &str) -> Result<Recorded, BooksError> {
        self.guard()?;
        let recorded = self.folded.state_mut().book(number)?;
        self.commit(
            by,
            &Event::Booked {
                number: number.to_owned(),
            },
            number,
        )?;
        Ok(recorded)
    }
}

impl<J: Journal> Books<J> {
    /// Take a record `csmsd` delivered, once.
    ///
    /// Returns `false` for a record already held — the delivery id is the
    /// record's key, so a retry changes nothing and is answered as a
    /// duplicate (SR-030).
    ///
    /// # Errors
    ///
    /// [`BooksError::Journal`] where the receipt could not be recorded, after
    /// which the service is halted.
    pub fn receive(
        &mut self,
        by: &Actor,
        cdr: emob_cdr::Cdr,
        partner: emob_core::PartyId,
        receipt: Option<String>,
    ) -> Result<bool, BooksError> {
        self.receive_bound(by, cdr, partner, receipt, None)
    }

    pub(crate) fn receive_bound(
        &mut self,
        by: &Actor,
        cdr: emob_cdr::Cdr,
        partner: emob_core::PartyId,
        receipt: Option<String>,
        retail: Option<crate::retail::Binding>,
    ) -> Result<bool, BooksError> {
        self.guard()?;
        let key = cdr.key.to_string();
        if let Some(prior) = self.state().received.get(&key) {
            if prior.cdr != cdr || prior.partner != partner || prior.retail != retail {
                return Err(BooksError::ConflictingDelivery);
            }
            return Ok(false);
        }
        let period = crate::period_of(&partner, cdr.ended_at);
        let event = Event::Received {
            cdr: Box::new(cdr),
            retail,
            partner,
            period,
            receipt,
        };
        self.commit(by, &event, &key)?;
        self.folded.state_mut().apply(event);
        Ok(true)
    }

    pub(crate) fn issue_retail(
        &mut self,
        by: &Actor,
        issue: crate::retail::Issue,
    ) -> Result<(), BooksError> {
        self.guard()?;
        let number = issue.document.number.clone();
        let event = Event::RetailIssued(Box::new(issue));
        self.commit(by, &event, &number)?;
        self.folded.state_mut().apply(event);
        Ok(())
    }

    pub(crate) fn line_credit(
        &mut self,
        actor: &Actor,
        credit: crate::retail::LineCredit,
        number: &str,
    ) -> Result<(), BooksError> {
        self.guard()?;
        let event = Event::LineCredited(Box::new(credit));
        self.commit(actor, &event, number)?;
        self.folded.state_mut().apply(event);
        Ok(())
    }

    pub(crate) fn withdrawal_credit(
        &mut self,
        actor: &Actor,
        credit: crate::retail::WithdrawalCredit,
    ) -> Result<(), BooksError> {
        self.guard()?;
        let number = credit.number.clone();
        let event = Event::WithdrawalCredited(Box::new(credit));
        self.commit(actor, &event, &number)?;
        self.folded.state_mut().apply(event);
        Ok(())
    }

    /// Close one partner's month from the records received for it.
    ///
    /// `None` when nothing was received for the period — a month with no
    /// sessions is not an invoice of zero.
    ///
    /// # Errors
    ///
    /// [`BooksError::Closing`] for a period already closed or a document
    /// `emob-billing` refuses, [`BooksError::Journal`] otherwise.
    pub fn close(
        &mut self,
        by: &Actor,
        closing: &crate::Closing,
    ) -> Result<Option<String>, BooksError> {
        let period = crate::period_key(&closing.partner, closing.year, closing.month);
        let mut ledger = emob_cdr::CdrLedger::new();
        let mut receipts = std::collections::BTreeMap::new();
        for received in self
            .state()
            .received
            .values()
            .filter(|r| r.period == period)
        {
            let _ = ledger.accept(received.cdr.clone());
            if let Some(receipt) = &received.receipt {
                receipts.insert(received.cdr.key.clone(), receipt.clone());
            }
        }
        if ledger.is_empty() {
            return Ok(None);
        }
        let first =
            time::Date::from_calendar_date(closing.year, closing.month, 1).map_err(|error| {
                BooksError::Halted {
                    why: error.to_string(),
                }
            })?;
        let last = first
            .replace_day(closing.month.length(closing.year))
            .map_err(|error| BooksError::Halted {
                why: error.to_string(),
            })?;
        let specification = closing.specification;
        let syntax = self.state().delivery_syntax;
        let number = self.issue_as(by, &period, specification, |number| {
            let mut builder = emob_billing::InvoiceBuilder::new(
                number,
                closing.issued_on,
                (first, last),
                closing.seller.clone(),
                closing.buyer.clone(),
            )
            .supplied_from(closing.seller.country.clone(), closing.vat_rate)
            .ledger(&ledger)
            .receipts(receipts)
            .due_on(closing.issued_on + time::Duration::days(14));
            if let Some(payment) = &closing.payment {
                builder = builder.paid_by(payment.clone());
            }
            if let Some(reference) = &closing.buyer_reference {
                builder = builder.buyer_reference(reference.clone());
            }
            let invoice = builder.build()?.value;
            // SR-122: a document its recipient's specification rejects is
            // refused here, while it is only a spent number — not served on a
            // page and posted to an access point that hands it back. The
            // profile's own proof, the one the files are written through.
            emob_billing::en16931::write(&invoice, specification, syntax)?;
            Ok(invoice)
        })?;
        Ok(Some(number))
    }
}

impl Billd {
    /// Fold one recorded fact back in.
    ///
    /// Total by construction: a fact that was decided once is not re-decided,
    /// so there is nothing here that can refuse. The two variants that go
    /// through the deciding methods do so because those are deterministic given
    /// the state the previous facts rebuilt — and a refusal there would mean
    /// the journal disagrees with itself, which [`Folded::open`] has already
    /// ruled out by checking the chain.
    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::RetailIssued(issue) => self.retail.apply(*issue),
            Event::LineCredited(credit) => self.retail.apply_line_credit(*credit),
            Event::WithdrawalCredited(credit) => self.retail.apply_credit(*credit),
            Event::Issued {
                period,
                number,
                invoice,
                cancels,
                replaces,
                specification,
                delivery_syntax,
                owed_to,
            } => {
                self.numbering.resume_past(&number);
                if let Some(period) = period {
                    self.closed.insert(period, number.clone());
                }
                if let Some(cancelled) = &cancels {
                    self.cancelled_by.insert(cancelled.clone(), number.clone());
                }
                self.issued.insert(
                    number,
                    Issued {
                        invoice: *invoice,
                        submission: Submission::Issued,
                        cancels,
                        replaces,
                        booked: false,
                        specification,
                        delivery_syntax,
                        owed_to,
                        answered: std::collections::BTreeMap::new(),
                    },
                );
            }
            Event::Announced {
                number,
                to,
                refused,
            } => self.answer(&number, &to, refused),
            Event::Submitted { number, submission } => {
                if let Some(document) = self.issued.get_mut(&number) {
                    document.submission = submission;
                }
            }
            // Deterministic: the postings are a function of the document, the
            // account of the role and the chart, and `doubleentry` keys the
            // entry on the invoice number — so a replay records once and
            // returns what it recorded.
            Event::Booked { number } => {
                let _ = self.book(&number);
            }
            Event::Received {
                cdr,
                retail,
                partner,
                period,
                receipt,
            } => {
                let key = cdr.key.to_string();
                if let Some(binding) = retail.as_ref().filter(|b| !b.agreement.is_empty())
                    && receipt.is_some()
                    && cdr
                        .evidence
                        .as_ref()
                        .is_some_and(|e| e.energy_billable && !e.payload_digests.is_empty())
                    && emob_cdr::validate(&cdr).is_settleable()
                {
                    self.retail
                        .index_receipt(binding, cdr.ended_at, key.clone());
                }
                self.received.insert(
                    key,
                    crate::Received {
                        cdr: *cdr,
                        retail,
                        partner,
                        period,
                        receipt,
                    },
                );
            }
        }
    }

    /// Which period this document closes, if any.
    #[must_use]
    pub fn period_closed_by(&self, number: &str) -> Option<String> {
        self.closed
            .iter()
            .find(|(_, by)| by.as_str() == number)
            .map(|(period, _)| period.clone())
    }
}
