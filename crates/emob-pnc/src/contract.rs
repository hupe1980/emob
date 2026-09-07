//! The contract itself: what this platform holds between one verification and
//! the next, and the two installation seams either side of it.
//!
//! # What the pools do, and what is left
//!
//! `[OPCP]` splits the ecosystem into four services, and three of them are
//! somebody else's server:
//!
//! | Service | Holds |
//! |---|---|
//! | Root Certificate Pool | the roots — *"the mutual trust store"* |
//! | Provisioning Certificate Pool | one certificate per **vehicle**, by PCID |
//! | Certificate Provisioning Service | signs contract data for a Mobility Operator |
//! | Contract Certificate Pool | the signed contract data, *"assigned to their respective PCID"* |
//!
//! None of that is here. What is here is the half a platform cannot fetch: the
//! **relationship** between a vehicle, a provider and an ecosystem, and the two
//! rules that hold across it.
//!
//! # A vehicle holds several contracts, and it is not an edge case
//!
//! `[OPCP §Contract Certificate Pool]`: *"The CCP also enables Multiple
//! contracts storing for each PCID."* A car with a lease contract and a private
//! one has two, from two providers, in one pool — and the shape that gets this
//! wrong is a map from PCID to contract, which is what a platform writes on the
//! day it has one of each.
//!
//! So [`ContractBook`] is keyed by **(pool, eMAID)** and carries the PCID as a
//! field. That is also the key the ecosystem updates on — *"the update process
//! overwrites the existing Contract Certificate with the same EMAID"* — so a
//! re-issued contract replaces the one it re-issues rather than joining it.
//!
//! # Which way the deletion runs
//!
//! One direction only, and `[OPCP §Provisioning Certificate Pool]` states it:
//! deleting a provisioning certificate *"triggers a deletion of all existing
//! Contract Certificates linked to this Provisioning Certificate"*. A vehicle
//! leaving the ecosystem takes its contracts with it.
//!
//! The other direction does not run at all, and
//! `[OPCP §Root Certificate Pool]` spends a paragraph saying so: a withdrawn
//! **root** does not delete anything. [`ContractBook::retire_vehicle`] is
//! therefore the only removal here, and there is deliberately no
//! `retire_anchor`.

use std::collections::BTreeMap;

use emob_core::Emaid;

use crate::{Pcid, PoolId, VerifiedContract, VerifiedProvisioning};

/// One contract this platform holds, as it was last verified.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Contract {
    /// The contract identifier, read off the certificate that carried it.
    pub emaid: Emaid,
    /// The ecosystem that vouched for it. **A** pool, never *the* pool (R10).
    pub pool: PoolId,
    /// The vehicle it was installed against, where this platform did the
    /// installation.
    ///
    /// `None` for a contract that was only ever *presented* — at a charge
    /// point, by a car this platform never provisioned. A CPO sees plenty of
    /// those and has no business inventing a PCID for them.
    pub vehicle: Option<Pcid>,
    /// The last instant the certificate itself is valid.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub valid_until: time::OffsetDateTime,
    /// The root the path reached, by name.
    pub anchor: String,
}

impl Contract {
    /// The contract a verification established, with no vehicle behind it.
    ///
    /// What a charge point's backend gets: a car presented a chain, it checked
    /// out, and which vehicle it came from is a fact the certificate does not
    /// carry.
    #[must_use]
    pub fn presented(verified: &VerifiedContract) -> Self {
        Self {
            emaid: verified.emaid.clone(),
            pool: verified.pool.clone(),
            vehicle: None,
            valid_until: verified.valid_until,
            anchor: verified.anchor.clone(),
        }
    }

    /// The contract a Mobility Operator installed **for a vehicle it checked**.
    ///
    /// Both halves verified, and that is the point: a platform that took the
    /// PCID as a string beside the contract would link a contract to whichever
    /// vehicle the caller named. The link this stores is between two chains
    /// that each reached a root.
    #[must_use]
    pub fn installed(verified: &VerifiedContract, vehicle: &VerifiedProvisioning) -> Self {
        Self {
            emaid: verified.emaid.clone(),
            pool: verified.pool.clone(),
            vehicle: Some(vehicle.pcid.clone()),
            valid_until: verified.valid_until,
            anchor: verified.anchor.clone(),
        }
    }

    /// Whether the certificate is still inside its own validity at an instant.
    #[must_use]
    pub fn is_valid_at(&self, at: time::OffsetDateTime) -> bool {
        at <= self.valid_until
    }
}

/// What this platform holds, keyed the way the ecosystem keys it.
///
/// See the module documentation: **(pool, eMAID)**, because one vehicle holds
/// several contracts and one eMAID is one contract inside one ecosystem.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ContractBook {
    contracts: BTreeMap<(String, String), Contract>,
}

impl ContractBook {
    /// A platform holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a contract, replacing one with the same identifier in the same
    /// pool.
    ///
    /// Replacement is the specified behaviour rather than a convenience:
    /// *"the update process overwrites the existing Contract Certificate with
    /// the same EMAID"* `[OPCP §Contract Certificate Pool]`. A book that
    /// appended would hold a renewed contract beside the one it renewed, and
    /// the older one would go on being offered until it expired.
    ///
    /// Returns what it replaced, so a caller can tell a renewal from a first
    /// installation without asking twice.
    pub fn install(&mut self, contract: Contract) -> Option<Contract> {
        self.contracts.insert(
            (
                contract.pool.as_str().to_owned(),
                contract.emaid.canonical(),
            ),
            contract,
        )
    }

    /// One contract, by the pool that issued it and the identifier it carries.
    #[must_use]
    pub fn get(&self, pool: &PoolId, emaid: &Emaid) -> Option<&Contract> {
        self.contracts
            .get(&(pool.as_str().to_owned(), emaid.canonical()))
    }

    /// Every contract, in a stable order.
    pub fn contracts(&self) -> impl Iterator<Item = &Contract> {
        self.contracts.values()
    }

    /// Every contract written against one vehicle, across every pool.
    ///
    /// The question `[OPCP §Contract Certificate Pool]`'s *"multiple contracts
    /// storing for each PCID"* makes real, and the one a map keyed by PCID
    /// answers with the most recent one and no warning.
    pub fn for_vehicle<'a>(&'a self, pcid: &'a Pcid) -> impl Iterator<Item = &'a Contract> {
        self.contracts
            .values()
            .filter(move |c| c.vehicle.as_ref() == Some(pcid))
    }

    /// Contracts due for renewal at an instant, against a window the operator
    /// states.
    ///
    /// See [`VerifiedContract::renewal_due`] for why the window is an argument
    /// and not a constant.
    pub fn renewals_due(
        &self,
        at: time::OffsetDateTime,
        within: time::Duration,
    ) -> impl Iterator<Item = &Contract> {
        self.contracts
            .values()
            .filter(move |c| c.valid_until - at <= within)
    }

    /// Remove every contract written against one vehicle, and say how many.
    ///
    /// The **one** cascade `[OPCP]` describes: deleting a provisioning
    /// certificate *"triggers a deletion of all existing Contract Certificates
    /// linked to this Provisioning Certificate"*
    /// `[OPCP §Provisioning Certificate Pool]`.
    ///
    /// There is deliberately no counterpart for a withdrawn root. A revoked
    /// root refuses today's validations — see [`crate::revocation`] — and
    /// deletes nothing, because `[OPCP §Root Certificate Pool]` says in as many
    /// words that the operator *"should not delete all its certificates, and
    /// their contract data"*, and because a `retain` that ran on a
    /// misconfiguration would take an OEM's whole book with it.
    pub fn retire_vehicle(&mut self, pcid: &Pcid) -> usize {
        let before = self.contracts.len();
        self.contracts
            .retain(|_, c| c.vehicle.as_ref() != Some(pcid));
        before - self.contracts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn contract(pool: &str, emaid: &str, vehicle: Option<&str>, until: i64) -> Contract {
        Contract {
            emaid: Emaid::parse(emaid).expect("a valid EMAID"),
            pool: PoolId::new(pool),
            vehicle: vehicle.map(|v| Pcid::new(v).expect("a valid PCID")),
            valid_until: datetime!(2026-01-01 00:00 UTC) + time::Duration::days(until),
            anchor: "V2G Root".to_owned(),
        }
    }

    #[test]
    fn one_vehicle_holds_several_contracts_and_the_book_says_so() {
        // `[OPCP §Contract Certificate Pool]`: *"The CCP also enables Multiple
        // contracts storing for each PCID."* A lease contract and a private
        // one, two providers, one car. A map keyed by PCID answers this with
        // whichever arrived second.
        let mut book = ContractBook::new();
        book.install(contract(
            "hubject-ccp",
            "DE-8AA-CA2B3C4D5-L",
            Some("hubjectopenpnc0008"),
            365,
        ));
        book.install(contract(
            "hubject-ccp",
            "DE-8BB-CA2B3C4D5-M",
            Some("hubjectopenpnc0008"),
            200,
        ));
        let pcid = Pcid::new("hubjectopenpnc0008").expect("a valid PCID");
        assert_eq!(book.for_vehicle(&pcid).count(), 2);
    }

    #[test]
    fn a_renewal_replaces_the_contract_it_renews_rather_than_joining_it() {
        let mut book = ContractBook::new();
        assert!(
            book.install(contract("hubject-ccp", "DE-8AA-CA2B3C4D5-L", None, 30))
                .is_none(),
            "the first installation replaces nothing"
        );
        let replaced = book
            .install(contract("hubject-ccp", "DE-8AA-CA2B3C4D5-L", None, 400))
            .expect("the renewal replaces the contract it renews");
        assert_eq!(replaced.valid_until, datetime!(2026-01-31 00:00 UTC));
        assert_eq!(book.contracts().count(), 1, "not two");
    }

    #[test]
    fn one_identifier_in_two_pools_is_two_contracts() {
        // The other half of R10. The same eMAID minted in two ecosystems is two
        // relationships, and a book keyed on the identifier alone would hold
        // one of them — silently, and whichever was written last.
        let mut book = ContractBook::new();
        book.install(contract("hubject-ccp", "DE-8AA-CA2B3C4D5-L", None, 30));
        book.install(contract("another-scheme", "DE-8AA-CA2B3C4D5-L", None, 90));
        assert_eq!(book.contracts().count(), 2);
        let emaid = Emaid::parse("DE-8AA-CA2B3C4D5-L").expect("a valid EMAID");
        assert_eq!(
            book.get(&PoolId::new("hubject-ccp"), &emaid)
                .expect("the one in that pool")
                .valid_until,
            datetime!(2026-01-31 00:00 UTC)
        );
    }

    #[test]
    fn retiring_a_vehicle_takes_its_contracts_and_nothing_else() {
        let mut book = ContractBook::new();
        book.install(contract(
            "hubject-ccp",
            "DE-8AA-CA2B3C4D5-L",
            Some("hubjectopenpnc0008"),
            365,
        ));
        book.install(contract(
            "hubject-ccp",
            "DE-8BB-CA2B3C4D5-M",
            Some("hubjectopenpnc0009"),
            365,
        ));
        // …and one presented at a point by a car this platform never
        // provisioned, which no cascade may reach.
        book.install(contract("hubject-ccp", "DE-8CC-CA2B3C4D5-Q", None, 365));

        let retired = book.retire_vehicle(&Pcid::new("hubjectopenpnc0008").expect("a valid PCID"));
        assert_eq!(retired, 1);
        assert_eq!(book.contracts().count(), 2);
    }

    #[test]
    fn renewals_are_due_against_a_window_the_operator_states() {
        let mut book = ContractBook::new();
        book.install(contract("hubject-ccp", "DE-8AA-CA2B3C4D5-L", None, 10));
        book.install(contract("hubject-ccp", "DE-8BB-CA2B3C4D5-M", None, 300));
        let at = datetime!(2026-01-01 00:00 UTC);
        assert_eq!(book.renewals_due(at, time::Duration::days(30)).count(), 1);
        assert_eq!(book.renewals_due(at, time::Duration::days(365)).count(), 2);
    }
}
