//! The invoice a partner is entitled to check, and the check itself.
//!
//! # The one module OCPI added for the half this workspace is about
//!
//! `[OCPI 2.3.0 §mod_invoice_reconciliation_module]` is a small module with a
//! large purpose: *"Invoice Reconciliation enables Parties that receive invoices
//! for Charging Sessions to check the amounts of these invoices against the CDR
//! data that they transferred via OCPI."* The record is an invoice identifier
//! and the list of CDR ids that invoice covers, and nothing else — when to
//! invoice, how the document is delivered and how it is paid are the parties'.
//!
//! Everywhere else in this workspace a record and its money travel together, so
//! that a CDR and its invoice line can never disagree about one session. Between
//! two *companies* they cannot: the CPO sends records over OCPI and an invoice
//! over an access point, and nothing in either document says which records the
//! other is about. This module is that sentence, and it is the last seam in the
//! chain from a signed meter value to a document somebody pays.
//!
//! # The check is this side's, not the wire's
//!
//! `ocpi-kit` adds up the OCPI CDRs a record names, which is the right answer
//! for a party that holds only OCPI documents. This side does not: it holds the
//! canonical records, each carrying the [`Rated`](emob_tariff::Rated) that
//! priced it, so [`check`] sums **those** — exactly, in
//! [`Money`], with the currency mismatch that makes a total
//! meaningless refused rather than summed.
//!
//! And it reads the ledger through [`CdrLedger::live`], which is where the money
//! is. A correction is a new record that supersedes the old one, and a partner
//! naming the **superseded** id has named a session this side has since
//! restated: adding it in would reconcile an invoice against a number neither
//! party will settle. That id is reported by name, beside the record that
//! replaced it, because *"your invoice covers a record we have corrected"* is a
//! conversation and not an error.

use std::collections::BTreeSet;

use emob_cdr::{CdrKey, CdrLedger};
use emob_core::{Money, PartyId};
use ocpi_kit::v2_3_0::invoice_reconciliation::InvoiceReconciliationRecord;

use crate::error::RoamError;
use crate::ocpi::location::bounded;

/// State the invoice this side has issued, and the records it covers.
///
/// `record_id` identifies the reconciliation record itself and `invoice_id` the
/// document it is about — the number `[UStG §14(4) Nr. 4]` spends *einmalig*,
/// which `billd` holds because a counter is not a fact any document carries.
///
/// # Errors
///
/// [`RoamError::NoInvoicedRecords`] for an empty list: the module gives `cdrs`
/// cardinality `+`, and an invoice covering no CDR is one nobody can check,
/// which is the whole point of sending the record.
///
/// [`RoamError::TooLong`] or [`RoamError::InvalidString`] for an identifier
/// OCPI's own bounds refuse — a refusal rather than a truncation, because a
/// truncated id names a different object.
///
/// A record naming the **same** CDR twice is not refused and not sent twice: the
/// specification's own validator reports a repeated id, and a caller assembling
/// the list from a month's invoice lines has an ordinary reason to hand over
/// duplicates. They are collapsed, in the order first seen, so the document that
/// leaves is one the partner's validator accepts.
///
/// ```
/// # use emob_roam::ocpi::reconciliation::to_ocpi;
/// # use emob_core::PartyId;
/// # use emob_cdr::{CdrKey};
/// # use time::macros::datetime;
/// # let party = PartyId::new("DE", "ABC")?;
/// # let key = |id: &str| CdrKey { party: party.clone(), id: id.parse().unwrap() };
/// let record = to_ocpi(
///     &party,
///     "rec-2026-03",
///     "R-2026-0001",
///     &[key("cdr-1"), key("cdr-2")],
///     datetime!(2026-04-01 09:00 UTC),
/// )?;
/// assert_eq!(record.cdrs.len(), 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn to_ocpi(
    party: &PartyId,
    record_id: &str,
    invoice_id: &str,
    covers: &[CdrKey],
    last_updated: time::OffsetDateTime,
) -> Result<InvoiceReconciliationRecord, RoamError> {
    if covers.is_empty() {
        return Err(RoamError::NoInvoicedRecords {
            invoice_id: invoice_id.to_owned(),
        });
    }

    let mut seen = BTreeSet::new();
    let mut cdrs = Vec::with_capacity(covers.len());
    for key in covers {
        let id = key.id.as_str();
        if !seen.insert(id.to_ascii_lowercase()) {
            continue;
        }
        cdrs.push(bounded::<36>("cdrs", id)?);
    }

    Ok(InvoiceReconciliationRecord {
        country_code: bounded::<2>("country_code", party.country_code())?,
        party_id: bounded::<3>("party_id", party.party_id())?,
        id: bounded::<36>("id", record_id)?,
        invoice_id: bounded::<255>("invoice_id", invoice_id)?,
        cdrs,
        last_updated: last_updated.into(),
        extensions: ocpi_kit::types::Extensions::default(),
    })
}

/// What checking a partner's record against this side's ledger produced.
///
/// Three lists rather than one verdict, because the three are three different
/// conversations. See [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// The invoice the record is about.
    pub invoice_id: String,
    /// What the records this side holds come to, gross — the figure to compare
    /// the invoice's own total against.
    ///
    /// `None` for **three** different absences, and [`Self::currencies`] is what
    /// tells them apart: no record was found and there was nothing to sum
    /// (empty); the records are in more than one currency, which is a total
    /// that means nothing beside a single invoice amount (more than one); or
    /// the sum itself could not be taken (exactly one).
    ///
    /// The third is not hypothetical. Every figure summed here comes off a
    /// record a **partner** sent, `Decimal` panics on overflow rather than
    /// saturating, and `Money`'s addition therefore refuses rather than
    /// panicking (D305) — so a ledger holding two absurd records produces no
    /// total at all. [`Self::is_conclusive`] is what stops that reading as
    /// agreement.
    pub total: Option<Money>,
    /// Ids the record names that this ledger does not hold at all.
    ///
    /// The check is **incomplete** rather than failed: a record this side never
    /// received is a transport question before it is a settlement one.
    pub unknown: Vec<String>,
    /// Ids the record names that this side has since **corrected**, each beside
    /// the record that superseded it.
    ///
    /// A correction is a new record that supersedes the old one, so an invoice
    /// covering the superseded id is an invoice for a session this side has
    /// restated. Not summed, and named rather than dropped.
    pub superseded: Vec<(String, String)>,
    /// Ids the record names that this side holds **unrated**.
    ///
    /// A partner's record read back through [`super::inbound::from_ocpi`] comes
    /// back unpriced on purpose, so a ledger that mixes the two has records with
    /// no money on them. They cannot be in the total and saying so is the
    /// difference between an incomplete check and a wrong one.
    pub unrated: Vec<String>,
    /// The currencies the records that were found are in, in the order first
    /// seen. More than one is why [`Self::total`] is `None`.
    pub currencies: Vec<String>,
}

impl Reconciled {
    /// Whether every id the record names was found, live and rated — the
    /// precondition for [`Self::total`] meaning anything.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unknown.is_empty() && self.superseded.is_empty() && self.unrated.is_empty()
    }

    /// Whether the check answers the question the module exists to ask:
    /// complete, in one currency, **and holding a figure**.
    ///
    /// The third clause is not redundant. A sum that could not be taken leaves
    /// one currency and an empty total, and without it this returned `true`
    /// while [`Self::difference_from`] returned `None` — a check calling itself
    /// conclusive with nothing to conclude from, which a caller reading the two
    /// in order takes for agreement.
    #[must_use]
    pub fn is_conclusive(&self) -> bool {
        self.is_complete() && self.currencies.len() == 1 && self.total.is_some()
    }

    /// What the invoice asks for, less what the records come to.
    ///
    /// Positive means the invoice asks for more than the sessions behind it.
    /// `None` where there is no total to compare against, or where the invoice
    /// is in another currency — which is a disagreement worth its own sentence
    /// rather than a number.
    #[must_use]
    pub fn difference_from(&self, invoiced: Money) -> Option<Money> {
        (invoiced - self.total?).ok()
    }
}

/// Check a partner's invoice reconciliation record against this side's ledger.
///
/// `owner` is the party whose records the ids belong to. OCPI makes a CDR id
/// unique **per party** `[OCPI 2.3.0 §mod_cdrs_cdr_object]`, so an id on its own
/// is not a key and two CPOs may each have a CDR `1` — the ledger is keyed on
/// the pair, and a check that looked up the bare id would reconcile an invoice
/// against somebody else's session.
///
/// The record itself carries the party that **owns the record**, which for an
/// invoice a CPO sends is the CPO and for one an eMSP sends is the eMSP. Which
/// party the *CDR ids* belong to is a fact about the settlement rather than
/// about the document, so it is an argument.
///
/// ```
/// # use emob_roam::ocpi::reconciliation::check;
/// # use emob_cdr::CdrLedger;
/// # use emob_core::{Currency, Money, PartyId};
/// # use ocpi_kit::v2_3_0::invoice_reconciliation::InvoiceReconciliationRecord;
/// # fn demo(record: &InvoiceReconciliationRecord, ledger: &CdrLedger, cpo: &PartyId,
/// #         invoiced: Money) {
/// let checked = check(record, cpo, ledger);
/// if checked.is_conclusive() {
///     match checked.difference_from(invoiced) {
///         Some(difference) if difference.is_zero() => println!("the invoice matches"),
///         Some(difference) => println!("the invoice asks {difference} more than the sessions"),
///         None => println!("the invoice is in another currency"),
///     }
/// }
/// # }
/// ```
#[must_use]
pub fn check(
    record: &InvoiceReconciliationRecord,
    owner: &PartyId,
    ledger: &CdrLedger,
) -> Reconciled {
    let mut total: Option<Money> = None;
    let mut unknown = Vec::new();
    let mut superseded = Vec::new();
    let mut unrated = Vec::new();
    let mut currencies: Vec<String> = Vec::new();

    for named in &record.cdrs {
        let Ok(id) = named.as_str().parse() else {
            // An id this side's own type refuses names no record here, which is
            // the same answer as one nobody holds and is reported the same way.
            unknown.push(named.as_str().to_owned());
            continue;
        };
        let key = CdrKey {
            party: owner.clone(),
            id,
        };
        let Some(cdr) = ledger.get(&key) else {
            unknown.push(named.as_str().to_owned());
            continue;
        };
        if let Some(corrector) = ledger.corrector_of(&key) {
            superseded.push((named.as_str().to_owned(), corrector.id.to_string()));
            continue;
        }
        let Some(cost) = cdr.total_cost() else {
            unrated.push(named.as_str().to_owned());
            continue;
        };
        let code = cost.currency().to_string();
        if !currencies.contains(&code) {
            currencies.push(code);
        }
        // Summed only while one currency holds. A second one makes the figure
        // meaningless rather than approximate, and `currencies` is what says so.
        total = match total {
            None => Some(cost),
            Some(running) => (running + cost).ok(),
        };
    }

    Reconciled {
        invoice_id: record.invoice_id.as_str().to_owned(),
        total: (currencies.len() == 1).then_some(total).flatten(),
        unknown,
        superseded,
        unrated,
        currencies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_cdr::Cdr;
    use emob_core::CdrId;
    use time::macros::datetime;

    fn party() -> PartyId {
        PartyId::new("DE", "ABC").expect("a valid party")
    }

    fn key(id: &str) -> CdrKey {
        CdrKey {
            party: party(),
            id: id.parse::<CdrId>().expect("a valid CDR id"),
        }
    }

    /// A rated record, priced at `amount` — the smallest shape the ledger and
    /// this check read.
    fn rated(id: &str, amount: &str, supersedes: Option<&str>) -> Cdr {
        let amount: rust_decimal::Decimal = amount.parse().expect("a decimal");
        let tariff = emob_tariff::Tariff::simple(
            "flat".parse().expect("a tariff id"),
            emob_core::Currency::EUR,
            emob_tariff::TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").expect("a zone"),
            vec![emob_tariff::PriceComponent::new(
                emob_tariff::Dimension::Flat,
                amount,
            )],
        );
        Cdr {
            key: key(id),
            reservation: None,
            session_id: "s-1".parse().expect("a session id"),
            evse_id: "DE*ABC*E00001".parse().expect("an EVSE id"),
            started_at: datetime!(2026-03-01 10:00 UTC),
            ended_at: datetime!(2026-03-01 10:30 UTC),
            auth_path: emob_session::AuthPath::AdHoc,
            authorization_reference: None,
            periods: Vec::new(),
            total_energy: emob_core::Energy::ZERO,
            direction: emob_core::Direction::Import,
            clock: emob_core::ClockResolution::conforming(),
            evidence: None,
            cost: Some(emob_cdr::Cost {
                tariff_id: tariff.id.clone(),
                tariff_fingerprint: tariff.fingerprint(),
                rated: emob_tariff::Rated {
                    lines: vec![emob_tariff::Line {
                        dimension: emob_tariff::Dimension::Flat,
                        quantity: rust_decimal::Decimal::ONE,
                        base_quantity: rust_decimal::Decimal::ONE,
                        unit_price: amount,
                        amount,
                        vat: None,
                    }],
                    currency: emob_core::Currency::EUR,
                    tax_included: emob_tariff::TaxIncluded::Yes,
                    adjustment: None,
                    notes: Vec::new(),
                },
                reservation: None,
            }),
            supersedes: supersedes.map(key),
        }
    }

    fn ledger(records: impl IntoIterator<Item = Cdr>) -> CdrLedger {
        let mut ledger = CdrLedger::new();
        for record in records {
            ledger.accept(record);
        }
        ledger
    }

    #[test]
    fn an_invoice_reconciles_against_the_records_it_names() {
        let ledger = ledger([
            rated("cdr-1", "10.00", None),
            rated("cdr-2", "4.46", None),
            rated("cdr-3", "99.00", None),
        ]);
        let record = to_ocpi(
            &party(),
            "rec-1",
            "R-2026-0001",
            &[key("cdr-1"), key("cdr-2")],
            datetime!(2026-04-01 09:00 UTC),
        )
        .expect("a well-formed record");

        let checked = check(&record, &party(), &ledger);
        assert!(checked.is_conclusive(), "{checked:?}");
        assert_eq!(
            checked.total.map(|t| t.to_string()),
            Some("14.46 EUR".to_owned())
        );
        // The third record is not the invoice's, and an invoice covers the CDRs
        // it lists rather than the ones that happen to be in the same month.
        assert_eq!(checked.unknown, Vec::<String>::new());
        assert_eq!(
            checked
                .difference_from(Money::new(
                    "14.46".parse().expect("a decimal"),
                    emob_core::Currency::EUR
                ))
                .map(Money::amount),
            Some(rust_decimal::Decimal::ZERO)
        );
    }

    /// **A corrected session is not the session the invoice covers.**
    ///
    /// A correction is a new record that supersedes the old one, so an invoice
    /// naming the superseded id is one for a session this side has since
    /// restated. Summing it in reconciles the document against a number neither
    /// party will settle — and every layer above reads `CdrLedger::live` for
    /// exactly this reason.
    #[test]
    fn a_superseded_record_is_named_rather_than_summed() {
        let ledger = ledger([
            rated("cdr-1", "10.00", None),
            rated("cdr-2", "4.46", Some("cdr-1")),
        ]);
        let record = to_ocpi(
            &party(),
            "rec-1",
            "R-2026-0001",
            &[key("cdr-1")],
            datetime!(2026-04-01 09:00 UTC),
        )
        .expect("a well-formed record");

        let checked = check(&record, &party(), &ledger);
        assert!(!checked.is_complete());
        assert_eq!(
            checked.superseded,
            vec![("cdr-1".to_owned(), "cdr-2".to_owned())]
        );
        assert_eq!(checked.total, None, "nothing was summed");
    }

    /// **A check with no figure is not a conclusive check.**
    ///
    /// `total` is `None` for three different absences and `currencies` is what
    /// tells them apart. The third — one currency, every id found and rated, and
    /// still no sum — is the one that has no list of its own: every figure here
    /// comes off a record a partner sent, and `Money`'s addition refuses rather
    /// than panicking on a figure `Decimal` cannot carry (D305). Before this
    /// clause the check called itself conclusive while `difference_from`
    /// returned `None`, which a caller reading the two in order takes for
    /// agreement (D311).
    #[test]
    fn a_check_that_could_not_take_its_own_sum_is_not_conclusive() {
        let unsummable = Reconciled {
            invoice_id: "R-2026-0001".to_owned(),
            total: None,
            unknown: Vec::new(),
            superseded: Vec::new(),
            unrated: Vec::new(),
            currencies: vec!["EUR".to_owned()],
        };
        assert!(unsummable.is_complete(), "every id was found and rated");
        assert!(
            !unsummable.is_conclusive(),
            "one currency and no figure is not an answer"
        );
        assert_eq!(
            unsummable.difference_from(Money::new(
                rust_decimal::Decimal::ONE,
                emob_core::Currency::EUR
            )),
            None
        );

        // …and the ordinary case still is one.
        let summed = Reconciled {
            total: Some(Money::new(
                rust_decimal::Decimal::TEN,
                emob_core::Currency::EUR,
            )),
            ..unsummable
        };
        assert!(summed.is_conclusive());
    }

    /// An id nobody here holds is an incomplete check, not a wrong invoice.
    #[test]
    fn an_unknown_id_leaves_the_check_incomplete() {
        let ledger = ledger([rated("cdr-1", "10.00", None)]);
        let record = to_ocpi(
            &party(),
            "rec-1",
            "R-2026-0001",
            &[key("cdr-1"), key("cdr-9")],
            datetime!(2026-04-01 09:00 UTC),
        )
        .expect("a well-formed record");

        let checked = check(&record, &party(), &ledger);
        assert!(!checked.is_complete());
        assert_eq!(checked.unknown, vec!["cdr-9".to_owned()]);
        // …and what *was* found is still summed, because an operator chasing a
        // missing record wants the figure it has.
        assert_eq!(
            checked.total.map(|t| t.to_string()),
            Some("10.00 EUR".to_owned())
        );
    }

    /// The ledger is keyed on the **pair**, because OCPI makes an id unique per
    /// party and two CPOs may each have a CDR `1`.
    #[test]
    fn an_id_is_not_a_key() {
        let ledger = ledger([rated("cdr-1", "10.00", None)]);
        let other = PartyId::new("DE", "XYZ").expect("a valid party");
        let record = to_ocpi(
            &party(),
            "rec-1",
            "R-2026-0001",
            &[key("cdr-1")],
            datetime!(2026-04-01 09:00 UTC),
        )
        .expect("a well-formed record");

        let checked = check(&record, &other, &ledger);
        assert_eq!(checked.unknown, vec!["cdr-1".to_owned()]);
    }

    /// `cdrs` has cardinality `+`, and an invoice covering nothing is one
    /// nobody can check.
    #[test]
    fn an_invoice_covering_no_record_is_refused() {
        let error = to_ocpi(
            &party(),
            "rec-1",
            "R-2026-0001",
            &[],
            datetime!(2026-04-01 09:00 UTC),
        )
        .expect_err("cardinality + is not zero");
        assert!(error.to_string().contains("R-2026-0001"), "{error}");
    }

    /// The same record named twice is one line, in the order first seen.
    #[test]
    fn a_repeated_id_is_collapsed_rather_than_sent_twice() {
        let record = to_ocpi(
            &party(),
            "rec-1",
            "R-2026-0001",
            &[key("cdr-1"), key("cdr-2"), key("cdr-1")],
            datetime!(2026-04-01 09:00 UTC),
        )
        .expect("a well-formed record");
        assert_eq!(record.cdrs.len(), 2);
        assert_eq!(record.cdrs[0].as_str(), "cdr-1");
        // …and the document a partner's own validator reads is clean.
        let checked = ocpi_kit::types::Validate::validate(&record);
        assert!(checked.is_ok(), "{checked:?}");
    }
}
