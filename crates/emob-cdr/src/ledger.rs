//! Accepting CDRs exactly once.
//!
//! # The failure this prevents
//!
//! Roaming transports retry. A partner that does not get a `200` in time sends
//! the CDR again, and the same charging session arrives twice. The usual
//! handling is an upsert keyed on the CDR id, which is wrong in both
//! directions:
//!
//! - a **retransmission** of an identical record is fine and must not produce a
//!   second invoice line;
//! - a **different** record under the same id is not a retry, it is a partner
//!   silently changing a settled number — and an upsert accepts it without a
//!   sound.
//!
//! [`CdrLedger::accept`] tells the two apart, and says which happened.
//!
//! # Why content equality rather than a version field
//!
//! OCPI has no version on a CDR. What it has is the rule that a CDR is
//! immutable and a correction is a *new* CDR that supersedes the old one. So
//! the only honest test for "is this the same record" is whether it says the
//! same thing, and that is what this does.
//!
//! # A correction chain has to have one end
//!
//! The immutability rule moves the ambiguity rather than removing it. Two
//! records that both supersede one key are two corrections of one session, and
//! the ledger holding both cannot say which is current — an upsert's problem,
//! arriving through the door that was built to avoid it. [`CdrLedger::accept`]
//! refuses the second, and [`CdrLedger::live`] is the set a billing run reads:
//! everything not superseded by something the ledger also holds. Summing
//! [`CdrLedger::iter`] instead bills a corrected session twice.

use std::collections::BTreeMap;

use crate::cdr::{Cdr, CdrKey};

/// What happened when a CDR was offered to the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Acceptance {
    /// The CDR is new; the ledger now holds it.
    Stored,
    /// An identical CDR was already held. Nothing changed, and nothing should
    /// be billed twice.
    Duplicate,
    /// A **different** CDR is already held under this key.
    ///
    /// Not accepted. A partner restating a settled number needs a human, not an
    /// upsert.
    Conflict {
        /// How the two differ, in words.
        difference: String,
    },
    /// Two records would correct the same one, and nothing says which is
    /// current.
    ///
    /// A correction is a new CDR that supersedes the old one, which makes the
    /// chain the only statement of what is owed. A second correction of the
    /// same record forks it: both are stored, neither is superseded, and a
    /// billing run reading the live set bills the session twice — the exact
    /// failure content equality is checked to prevent, arriving one link along.
    ///
    /// Not accepted, for the reason a [`Self::Conflict`] is not: whichever of
    /// the two is right, choosing needs somebody who knows why the first
    /// correction was wrong.
    Forked {
        /// The record both corrections claim to replace.
        supersedes: CdrKey,
        /// The correction already held.
        held: CdrKey,
    },
}

impl Acceptance {
    /// Whether the ledger changed.
    #[must_use]
    pub const fn is_stored(&self) -> bool {
        matches!(self, Self::Stored)
    }

    /// Whether this needs a human.
    #[must_use]
    pub const fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. } | Self::Forked { .. })
    }
}

/// An in-memory ledger of accepted CDRs.
///
/// Pure data — persisting it is a service's job — so a whole month of roaming
/// traffic can be replayed as a unit test.
#[derive(Debug, Clone, Default)]
pub struct CdrLedger {
    entries: BTreeMap<CdrKey, Cdr>,
}

impl CdrLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a CDR to the ledger.
    ///
    /// Idempotent: offering the same CDR any number of times stores it once and
    /// reports [`Acceptance::Duplicate`] thereafter.
    ///
    /// A correction is checked as well as a record: one that supersedes itself,
    /// or that forks a chain another correction already owns, is refused rather
    /// than stored beside it. See [`Acceptance::Forked`].
    pub fn accept(&mut self, cdr: Cdr) -> Acceptance {
        if let Some(previous) = &cdr.supersedes {
            // A record cannot correct itself. Stored, it would be superseded by
            // its own presence and vanish from every live set — a CDR that
            // exists and is billed by nothing.
            if *previous == cdr.key {
                return Acceptance::Forked {
                    supersedes: previous.clone(),
                    held: cdr.key.clone(),
                };
            }
            // …and a chain has one end. The check runs before the identity
            // check below so that re-offering the *same* correction still
            // reports `Duplicate`: a retransmission is not a fork.
            if let Some(held) = self.corrector_of(previous)
                && *held != cdr.key
            {
                return Acceptance::Forked {
                    supersedes: previous.clone(),
                    held: held.clone(),
                };
            }
        }

        match self.entries.get(&cdr.key) {
            None => {
                self.entries.insert(cdr.key.clone(), cdr);
                Acceptance::Stored
            }
            Some(existing) if *existing == cdr => Acceptance::Duplicate,
            Some(existing) => Acceptance::Conflict {
                difference: describe_difference(existing, &cdr),
            },
        }
    }

    /// The CDR held under a key.
    #[must_use]
    pub fn get(&self, key: &CdrKey) -> Option<&Cdr> {
        self.entries.get(key)
    }

    /// How many CDRs are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ledger is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every CDR, in key order.
    pub fn iter(&self) -> impl Iterator<Item = &Cdr> {
        self.entries.values()
    }

    /// The CDRs that supersede another, and the keys they replace.
    ///
    /// A correction chain is how a settled number legitimately changes, and it
    /// is worth being able to walk it.
    pub fn corrections(&self) -> impl Iterator<Item = (&Cdr, &CdrKey)> {
        self.entries
            .values()
            .filter_map(|cdr| cdr.supersedes.as_ref().map(|prev| (cdr, prev)))
    }

    /// Whether a key has been superseded by a later correction.
    #[must_use]
    pub fn is_superseded(&self, key: &CdrKey) -> bool {
        self.corrector_of(key).is_some()
    }

    /// The record that supersedes a key, if the ledger holds one.
    #[must_use]
    pub fn corrector_of(&self, key: &CdrKey) -> Option<&CdrKey> {
        self.entries
            .values()
            .find(|cdr| cdr.supersedes.as_ref() == Some(key))
            .map(|cdr| &cdr.key)
    }

    /// The records a billing run may act on: everything this ledger holds that
    /// nothing else in it supersedes.
    ///
    /// **The set to sum**, and the reason it exists rather than being left to
    /// each caller: a correction is a *new* record, so a ledger holding a
    /// session and its correction holds both, and
    /// `iter().map(total_energy).sum()` bills that session twice. Filtering
    /// with [`Self::is_superseded`] per record is the same answer at quadratic
    /// cost, which is what a caller writes when the ledger does not offer it.
    ///
    /// Order is the ledger's own — by party, then by record id.
    pub fn live(&self) -> impl Iterator<Item = &Cdr> {
        let superseded: std::collections::BTreeSet<&CdrKey> = self
            .entries
            .values()
            .filter_map(|cdr| cdr.supersedes.as_ref())
            .collect();
        self.entries
            .values()
            .filter(move |cdr| !superseded.contains(&cdr.key))
    }
}

/// Say what changed, in the terms a settlement dispute is conducted in.
///
/// The energy and the period count first, because those are what money is
/// computed from and what a partner is most likely to have quietly restated.
fn describe_difference(existing: &Cdr, incoming: &Cdr) -> String {
    let mut differences = Vec::new();

    if existing.total_energy != incoming.total_energy {
        differences.push(format!(
            "total energy {} → {}",
            existing.total_energy, incoming.total_energy
        ));
    }
    if existing.periods.len() != incoming.periods.len() {
        differences.push(format!(
            "{} periods → {}",
            existing.periods.len(),
            incoming.periods.len()
        ));
    }
    if existing.session_id != incoming.session_id {
        differences.push(format!(
            "session {} → {}",
            existing.session_id, incoming.session_id
        ));
    }
    if existing.evse_id != incoming.evse_id {
        differences.push(format!("EVSE {} → {}", existing.evse_id, incoming.evse_id));
    }
    if existing.started_at != incoming.started_at || existing.ended_at != incoming.ended_at {
        differences.push("the session window moved".to_owned());
    }
    if existing.direction != incoming.direction {
        differences.push(format!(
            "direction {} → {}",
            existing.direction, incoming.direction
        ));
    }
    if existing.auth_path != incoming.auth_path {
        differences.push(format!(
            "authorisation {:?} → {:?}",
            existing.auth_path, incoming.auth_path
        ));
    }
    if existing.evidence != incoming.evidence {
        differences.push("the signed evidence differs".to_owned());
    }
    match (existing.total_cost(), incoming.total_cost()) {
        (Some(before), Some(after)) if before != after => {
            differences.push(format!("total cost {before} → {after}"));
        }
        (Some(_), None) => differences.push("the price was dropped".to_owned()),
        (None, Some(after)) => differences.push(format!("a price of {after} was added")),
        _ if existing.cost != incoming.cost => {
            differences.push("the price breakdown differs".to_owned());
        }
        _ => {}
    }

    if differences.is_empty() {
        // Equality already failed, so something differs; saying "identical"
        // here would be worse than admitting the comparison is incomplete.
        "the records differ in a field this summary does not name".to_owned()
    } else {
        differences.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr::{CdrBuilder, ChargingPeriod};
    use emob_core::{Direction, Energy, PartyId};
    use emob_session::{
        Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
    };
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use time::macros::datetime;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn party() -> PartyId {
        PartyId::new("DE", "ABC").unwrap()
    }

    fn cdr(id: &str, end_kwh: &str) -> Cdr {
        let mut s = Session::open(
            "s-1".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(SessionState::Charging, at(0)).unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(30),
                        kwh(end_kwh),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(30), EndReason::Local).unwrap();

        CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), id.parse().unwrap())
            .build()
            .unwrap()
    }

    #[test]
    fn a_new_cdr_is_stored() {
        let mut ledger = CdrLedger::new();
        assert_eq!(ledger.accept(cdr("1", "118.000")), Acceptance::Stored);
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn a_retransmission_is_not_a_second_invoice() {
        let mut ledger = CdrLedger::new();
        ledger.accept(cdr("1", "118.000"));

        // The partner did not get our 200 and sent it again. Three times.
        for _ in 0..3 {
            assert_eq!(ledger.accept(cdr("1", "118.000")), Acceptance::Duplicate);
        }
        assert_eq!(ledger.len(), 1, "one session, one record");
    }

    #[test]
    fn a_restated_number_is_a_conflict_not_an_upsert() {
        let mut ledger = CdrLedger::new();
        ledger.accept(cdr("1", "118.000"));

        let outcome = ledger.accept(cdr("1", "218.000"));
        assert!(outcome.is_conflict());
        let Acceptance::Conflict { difference } = outcome else {
            panic!("expected a conflict");
        };
        assert!(
            difference.contains("total energy 18.000 kWh → 118.000 kWh"),
            "{difference}"
        );

        // And the original is untouched: a partner does not get to overwrite a
        // settled number by resending it.
        assert_eq!(
            ledger
                .get(&CdrKey {
                    party: party(),
                    id: "1".parse().unwrap()
                })
                .unwrap()
                .total_energy,
            kwh("18.000")
        );
    }

    #[test]
    fn the_conflict_message_names_what_moved() {
        let mut ledger = CdrLedger::new();
        let original = cdr("1", "118.000");
        ledger.accept(original.clone());

        let mut changed = original;
        changed.periods.push(ChargingPeriod {
            quarter_hour: emob_session::QuarterHour::containing(at(45)),
            start: at(45),
            end: at(60),
            energy: kwh("0"),
            charging: true,
            provenance: emob_session::Provenance::Measured,
        });

        let Acceptance::Conflict { difference } = ledger.accept(changed) else {
            panic!("expected a conflict");
        };
        assert!(difference.contains("periods"), "{difference}");
    }

    #[test]
    fn two_parties_may_each_have_a_cdr_number_one() {
        let mut ledger = CdrLedger::new();
        let mut theirs = cdr("1", "118.000");
        theirs.key.party = PartyId::new("DE", "XYZ").unwrap();

        assert_eq!(ledger.accept(cdr("1", "118.000")), Acceptance::Stored);
        assert_eq!(
            ledger.accept(theirs),
            Acceptance::Stored,
            "a ledger keyed on the bare id would have dropped one"
        );
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn a_correction_chain_is_walkable() {
        let mut ledger = CdrLedger::new();
        let first = cdr("1", "118.000");
        ledger.accept(first.clone());

        let mut correction = cdr("2", "120.000");
        correction.supersedes = Some(first.key.clone());
        ledger.accept(correction);

        assert!(ledger.is_superseded(&first.key));
        let corrections: Vec<_> = ledger.corrections().collect();
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].1, &first.key);
    }

    #[test]
    fn an_uncorrected_cdr_is_not_superseded() {
        let mut ledger = CdrLedger::new();
        let only = cdr("1", "118.000");
        ledger.accept(only.clone());
        assert!(!ledger.is_superseded(&only.key));
    }

    #[test]
    fn the_live_set_is_what_a_billing_run_sums() {
        // A correction is a *new* record, so a ledger holding a session and its
        // correction holds both. Summing `iter()` bills that session twice —
        // the failure content equality is checked to prevent, one link along.
        let mut ledger = CdrLedger::new();
        let first = cdr("1", "118.000");
        ledger.accept(first.clone());

        let mut correction = cdr("2", "120.000");
        correction.supersedes = Some(first.key.clone());
        ledger.accept(correction.clone());

        assert_eq!(ledger.iter().count(), 2);
        let live: Vec<&CdrKey> = ledger.live().map(|cdr| &cdr.key).collect();
        assert_eq!(live, vec![&correction.key]);

        let billed: Energy = ledger.live().map(|cdr| cdr.total_energy).sum();
        assert_eq!(billed, kwh("20.000"), "the corrected figure, once");
    }

    #[test]
    fn a_second_correction_of_one_record_is_a_fork_and_is_refused() {
        // Both would be live, neither superseded, and the session billed twice.
        // Whichever is right, choosing needs somebody who knows why the first
        // correction was wrong.
        let mut ledger = CdrLedger::new();
        let first = cdr("1", "118.000");
        ledger.accept(first.clone());

        let mut correction = cdr("2", "120.000");
        correction.supersedes = Some(first.key.clone());
        assert_eq!(ledger.accept(correction.clone()), Acceptance::Stored);

        let mut second = cdr("3", "121.000");
        second.supersedes = Some(first.key.clone());
        let answer = ledger.accept(second);
        assert_eq!(
            answer,
            Acceptance::Forked {
                supersedes: first.key.clone(),
                held: correction.key.clone(),
            }
        );
        assert!(
            answer.is_conflict(),
            "a fork needs a human, like a conflict"
        );
        assert_eq!(ledger.len(), 2, "the fork was not stored");

        // …and re-offering the correction the ledger already holds is still a
        // retransmission rather than a fork.
        assert_eq!(ledger.accept(correction), Acceptance::Duplicate);
    }

    #[test]
    fn a_record_that_supersedes_itself_is_refused() {
        // Stored, it would be superseded by its own presence and vanish from
        // every live set: a CDR that exists and is billed by nothing.
        let mut ledger = CdrLedger::new();
        let mut ouroboros = cdr("1", "118.000");
        ouroboros.supersedes = Some(ouroboros.key.clone());

        assert!(ledger.accept(ouroboros).is_conflict());
        assert!(ledger.is_empty());
    }

    #[test]
    fn a_correction_may_arrive_before_the_record_it_corrects() {
        // Roaming transports do not order deliveries, and OCPI lets a partner
        // send a correction standalone. Refusing one whose original has not
        // landed yet would drop the only record of what is owed.
        let mut ledger = CdrLedger::new();
        let first = cdr("1", "118.000");
        let mut correction = cdr("2", "120.000");
        correction.supersedes = Some(first.key.clone());

        assert_eq!(ledger.accept(correction.clone()), Acceptance::Stored);
        assert_eq!(ledger.accept(first.clone()), Acceptance::Stored);

        let live: Vec<&CdrKey> = ledger.live().map(|cdr| &cdr.key).collect();
        assert_eq!(
            live,
            vec![&correction.key],
            "the order does not change what is owed"
        );
    }
}
