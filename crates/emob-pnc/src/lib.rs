//! ISO 15118 **Plug & Charge**: the contract lifecycle, and the questions the
//! protocol libraries leave to whoever runs a platform.
//!
//! # What this crate is, by what it is not
//!
//! It speaks to no vehicle. The EXI codec, the message sets, the session
//! engines and the TLS are `iso15118`'s; they run on a charging station and on
//! an ECU, and re-stating any of them here would be a second implementation of
//! something that already round-trips against the reference encoder.
//!
//! What is left is everything the protocol assumes somebody else did:
//!
//! * **which ecosystem** a contract belongs to, when there is more than one
//!   (R10, [`pool`]);
//! * whether anything on its path has been **withdrawn**, which
//!   `iso15118::pnc::pki::validate` names as what it does not answer
//!   ([`revocation`]);
//! * which **vehicle** a contract was installed against, and which contracts go
//!   when that vehicle does ([`contract`]);
//! * and the two identifiers the relationship is keyed on —
//!   [`emob_core::Emaid`] and [`Pcid`].
//!
//! # The rule everything here is arranged around
//!
//! `[V2G2-925]`, which is easy to read as a formality:
//!
//! > \[a leaf\] shall be treated as invalid, if the trust anchor at the end of
//! > the chain does not match the specific root certificate required for a
//! > certain use.
//!
//! With one Plug & Charge pool it is unobservable. With two it is the whole
//! security property, because a contract from another ecosystem **validates**:
//! the mathematics is sound, every signature checks, every Annex F profile
//! holds — and the driver has no relationship with this platform. Nothing
//! fails. A session starts, a CDR is written, and an invoice goes to a provider
//! that never heard of them.
//!
//! So there is no global trust store here. Anchors live inside a [`Pool`], the
//! pool is named at the call, and [`PoolRegistry::validate_in`] cannot reach
//! another one's.
//!
//! # The output
//!
//! An authorisation `emob-session` already models — `AuthPath::PlugAndCharge`,
//! whose `strongest_plausible_level` is `Secure` because `[OCMF Tab. 15]`'s
//! `ISO15118_PNC` is Table 11's own example of it. That ceiling is what a signed
//! meter record is cross-checked against, so a contract this crate refused
//! cannot become a session that claims one.
//!
//! # No I/O, no clock
//!
//! The four `[OPCP]` services are all on the far side of a socket. A revocation
//! status is a value a caller supplies with the instant it was compiled at; a
//! validation takes the instant to judge against.
//!
//! ```
//! use emob_pnc::{Pcid, Pool, PoolId, PoolRegistry, RevocationList, TrustAnchor};
//!
//! // A vehicle names its manufacturer in its own identifier.
//! let vehicle: Pcid = "hubjectopenpnc0008".parse()?;
//! assert_eq!(vehicle.wmi(), "HUB");
//!
//! // …and a platform is connected to ecosystems, plural, from the first line.
//! let registry = PoolRegistry::new()
//!     .with(
//!         "hubject-ccp",
//!         Pool::new().with(TrustAnchor::unbounded(vec![0x30], "over OFTP2, 2026-01")),
//!     )
//!     .with("another-scheme", Pool::new());
//! assert_eq!(registry.pools().count(), 2);
//!
//! // A list that holds nothing says so, rather than saying everything is fine.
//! assert_eq!(RevocationList::new().issuers().count(), 0);
//! # let _ = PoolId::new("hubject-ccp");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

pub mod contract;
pub mod error;
pub mod fixtures;
pub mod id;
pub mod pool;
pub mod revocation;
pub mod verify;

pub use contract::{Contract, ContractBook};
pub use error::PncError;
pub use id::{Pcid, PcidError};
pub use pool::{Pool, PoolId, PoolRegistry, TrustAnchor};
pub use revocation::{DEFAULT_FRESHNESS, RevocationList, Standing, Withdrawals};
pub use verify::{
    Curves, VerifiedContract, VerifiedProvisioning, verify_contract, verify_provisioning,
};

/// Which manufacturers an account may publish provisioning certificates for.
///
/// `[OPCP §Provisioning Certificate Pool]`'s **first** control step, and it is
/// first for a reason: *"Verifies the PCIDs world manufacturer identifier (WMI)
/// against the OEM accounts authorized WMI list"*. Everything after it is
/// arithmetic over a certificate that may belong to somebody else's production
/// line.
///
/// > To achieve this, the client credentials of each OEM get white-listed for a
/// > list of World manufacturer identifier (WMI) codes (see ISO 3780).
///
/// # Errors
///
/// [`PncError::NotThisManufacturer`] where the vehicle's WMI is not one the
/// account is enrolled for. An empty list authorises nothing, which is the
/// same default [`RevocationList`] takes and for the same reason.
pub fn may_publish_for(pcid: &Pcid, authorized: &[&str]) -> Result<(), PncError> {
    if authorized
        .iter()
        .any(|wmi| wmi.eq_ignore_ascii_case(pcid.wmi()))
    {
        return Ok(());
    }
    Err(PncError::NotThisManufacturer {
        pcid: pcid.as_str().to_owned(),
        wmi: pcid.wmi().to_owned(),
        authorized: authorized.iter().map(|s| (*s).to_owned()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_may_publish_only_for_the_manufacturers_it_is_enrolled_for() {
        let pcid: Pcid = "hubjectopenpnc0008".parse().expect("a valid PCID");
        assert!(may_publish_for(&pcid, &["HUB", "WVW"]).is_ok());
        assert!(
            may_publish_for(&pcid, &["hub"]).is_ok(),
            "an enrolment list is not case-sensitive; a PCID is normalised"
        );

        let refused = may_publish_for(&pcid, &["WVW"]).expect_err("not this account's WMI");
        assert!(
            matches!(&refused, PncError::NotThisManufacturer { wmi, .. } if wmi == "HUB"),
            "{refused}"
        );
    }

    #[test]
    fn an_empty_enrolment_authorises_nobody() {
        // The same default `RevocationList` takes: a platform is not doing this
        // until somebody has configured it, and a default that quietly said yes
        // would let that never happen.
        let pcid: Pcid = "hubjectopenpnc0008".parse().expect("a valid PCID");
        assert!(may_publish_for(&pcid, &[]).is_err());
    }
}
