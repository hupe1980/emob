//! The curves, and the one function that turns a chain into a contract.
//!
//! # The crypto is the workspace's, not a second copy of it
//!
//! `iso15118`'s `pnc` feature takes the curve as a **trait** the caller
//! implements, which is what lets ISO 15118 path validation live in a domain
//! crate here at all. D304 holds this workspace one `RustCrypto` generation
//! behind the head — `sha2` 0.10, `ecdsa` 0.16, `p256` 0.13 — because `ocmf`
//! links that one and the simulator is only worth something if the signature it
//! writes and the signature `ocmf` checks go through the same code. Enabling
//! the kit's own `pnc-rustcrypto` backend would have put `p256` 0.14 and
//! `sha2` 0.11 in the same binary as `p256` 0.13 and `sha2` 0.10: two
//! implementations of one primitive, compiling perfectly, because the boundary
//! between them is SEC1 bytes.
//!
//! So [`Curves`] implements the trait on this workspace's own crates, and
//! `crates/emob-pnc/tests/a_contract_is_installed.rs` runs it against Hubject's
//! published test PKI — which is the only thing that proves the two agree.
//!
//! # Both curves Annex F admits
//!
//! `[V2G2-884]` sends every certificate to an Annex F profile, and those name
//! secp256r1 for ISO 15118-2 and secp521r1 for -20's higher level. Both are on
//! the `ecdsa` 0.16 generation, so taking the second costs no second
//! implementation of the first — which is the whole of D304's rule, and the
//! reason it was worth checking rather than assuming.

use emob_core::Emaid;
use iso15118::pnc::pki::{Profile, VerifyWith};
use iso15118::pnc::{PncError as KitError, Suite};

use crate::{Pcid, PncError, PoolId, PoolRegistry, RevocationList};

/// ECDSA verification for the two curves ISO 15118's Annex F admits, on this
/// workspace's own `RustCrypto` generation.
///
/// Verification only. Nothing in this crate signs anything: a contract
/// certificate is minted by a Mobility Operator's CA through the Certificate
/// Provisioning Service `[OPCP §Certificate Provisioning Service]`, and a
/// platform that could mint one is a platform whose separation of duties is a
/// comment.
#[derive(Debug, Clone, Copy, Default)]
pub struct Curves;

impl VerifyWith for Curves {
    fn verify_with(
        &self,
        suite: Suite,
        public_key: &[u8],
        data: &[u8],
        signature: &[u8],
    ) -> Result<(), KitError> {
        match suite {
            Suite::EcdsaSha256 => {
                use p256::ecdsa::signature::Verifier as _;
                let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| KitError::Backend("not a valid P-256 public point"))?;
                let signature = p256::ecdsa::Signature::from_slice(signature)
                    .map_err(|_| KitError::BadSignature)?;
                key.verify(data, &signature)
                    .map_err(|_| KitError::BadSignature)
            }
            Suite::EcdsaSha512 => {
                use p521::ecdsa::signature::Verifier as _;
                let key = p521::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| KitError::Backend("not a valid P-521 public point"))?;
                let signature = p521::ecdsa::Signature::from_slice(signature)
                    .map_err(|_| KitError::BadSignature)?;
                key.verify(data, &signature)
                    .map_err(|_| KitError::BadSignature)
            }
            // `Suite` is `#[non_exhaustive]`: a generation this build does not
            // know is a signature it cannot check, and an unchecked signature
            // is not a weaker verification — it is none. Refused by name, the
            // way `emob-eichrecht` refuses the three `[OCMF Tab. 22]` curves it
            // has no pure implementation of (R18).
            _ => Err(KitError::Backend(
                "a signature suite this build has no curve for: refused rather than skipped",
            )),
        }
    }
}

/// A contract certificate that this platform has checked, and what it says.
///
/// There is no constructor. [`verify_contract`] is the only thing that produces
/// one, and it takes the pool — so a value of this type cannot exist without a
/// path to a root **that pool** publishes, unrevoked over its whole length, at
/// an instant the caller named. The same construction `emob_smart::Allocation`
/// uses for a grid ceiling, for the same reason: a rule enforced by the only
/// door a value comes through cannot be forgotten at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct VerifiedContract {
    /// The contract identifier, **read off the certificate**.
    ///
    /// `[V2G2-108]` makes the leaf's Common Name the EMAID, and Table F.4 says
    /// the same. So it is read there and parsed through [`emob_core::Emaid`],
    /// check digit and all — never taken as an argument beside the chain.
    ///
    /// A check fed by a caller is not a check: a platform that accepted the
    /// eMAID and the certificate separately would authorise whichever contract
    /// the caller named, and the certificate would be decoration.
    pub emaid: Emaid,
    /// Which ecosystem vouched for it.
    pub pool: PoolId,
    /// The last instant the leaf itself is valid, from its own `notAfter`.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub valid_until: time::OffsetDateTime,
    /// The `Common Name` of the root the path reached, for a record that has to
    /// say *which* trust anchor a session rested on two years later.
    pub anchor: String,
}

impl VerifiedContract {
    /// How much longer this contract certificate is valid.
    ///
    /// Negative after expiry, which cannot arise from [`verify_contract`] —
    /// the path validation refuses an expired certificate — and can arise from
    /// a stored value asked about a later instant.
    #[must_use]
    pub fn remaining(&self, at: time::OffsetDateTime) -> time::Duration {
        self.valid_until - at
    }

    /// Whether it should be renewed, against a window the operator states.
    ///
    /// # Why the window is an argument
    ///
    /// Nothing in `[OPCP]` fixes one, and ISO 15118-2 leaves the trigger to the
    /// vehicle. How long before expiry a Mobility Operator re-issues is a
    /// commercial and operational decision — how often its fleet charges, how
    /// long its CA takes, what its contract term is — and a constant here
    /// presented as a requirement would be a citation this crate cannot
    /// support. It is the same refusal `sepa` makes about a collection date and
    /// `tarifd` makes about a publication lead: the number belongs to whoever
    /// can be held to it.
    #[must_use]
    pub fn renewal_due(&self, at: time::OffsetDateTime, within: time::Duration) -> bool {
        self.remaining(at) <= within
    }
}

/// A provisioning certificate that this platform has checked.
///
/// The vehicle's end of the seam. `[V2G2-933]` makes the leaf's Common Name the
/// `CertID`, which `[OPCP §Handling of IDs]` spells PCID, and it is read there
/// for the same reason the eMAID is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct VerifiedProvisioning {
    /// The vehicle, read off the certificate.
    pub pcid: Pcid,
    /// Which ecosystem vouched for it.
    pub pool: PoolId,
    /// The last instant the leaf itself is valid.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub valid_until: time::OffsetDateTime,
    /// The `Common Name` of the root the path reached.
    pub anchor: String,
}

/// Check a contract certificate chain, and read the contract off it.
///
/// `chain` is the leaf followed by its intermediates, leaf first, as DER — the
/// order `CertificateChainType` carries on the wire, and the order
/// `[OPCP §Contract Certificate Pool]` returns signed contract data in.
///
/// # Errors
///
/// Everything [`PoolRegistry::validate_in`] refuses, and two more that are this
/// crate's: [`PncError::CommonNameIsNotAnEmaid`] where the leaf's Common Name
/// is absent or is not a well-formed contract identifier, and
/// [`PncError::NotThisProvider`] where it is one and names a provider the
/// caller is not expecting.
///
/// ```no_run
/// # use emob_pnc::{PoolId, PoolRegistry, RevocationList, verify_contract};
/// # use iso15118::pnc::pki::Profile;
/// # let (registry, chain, revocations): (PoolRegistry, Vec<&[u8]>, RevocationList) = unimplemented!();
/// let contract = verify_contract(
///     &registry,
///     &PoolId::new("hubject-ccp"),
///     &chain,
///     &revocations,
///     time::OffsetDateTime::UNIX_EPOCH,
///     None,
/// )?;
/// // The identifier came off the certificate, not off the caller.
/// println!("{}", contract.emaid.canonical());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn verify_contract<'a>(
    pools: &'a PoolRegistry,
    pool: &PoolId,
    chain: &[&'a [u8]],
    revocations: &RevocationList,
    at: time::OffsetDateTime,
    expected_provider: Option<&str>,
) -> Result<VerifiedContract, PncError> {
    let validated =
        pools.validate_in(pool, chain, Profile::ContractCertificate, at, revocations)?;

    let common_name =
        validated
            .subject_common_name()
            .ok_or_else(|| PncError::CommonNameIsNotAnEmaid {
                common_name: None,
                detail: "the leaf carries no Common Name".to_owned(),
            })?;
    let emaid = Emaid::parse(common_name).map_err(|source| PncError::CommonNameIsNotAnEmaid {
        common_name: Some(common_name.to_owned()),
        detail: source.to_string(),
    })?;

    // `[OPCP §Contract Certificate Pool]`: *"The CCP keeps contracts of each MO
    // separated. The defined access rules prevent unauthorized requests to
    // others MOs contracts"*. The identifier's own provider section is what
    // says which MO a contract belongs to, so a caller that knows which
    // provider it asked about can have that checked here rather than comparing
    // strings at the call site and forgetting to in one of them.
    if let Some(expected) = expected_provider
        && !emaid.provider_id().eq_ignore_ascii_case(expected)
    {
        return Err(PncError::NotThisProvider {
            emaid: emaid.canonical(),
            provider: emaid.provider_id().to_owned(),
            expected: expected.to_owned(),
        });
    }

    Ok(VerifiedContract {
        emaid,
        pool: pool.clone(),
        valid_until: instant(validated.leaf.not_after),
        anchor: validated
            .anchor
            .subject
            .common_name
            .unwrap_or("an anchor with no Common Name")
            .to_owned(),
    })
}

/// Check an OEM provisioning certificate chain, and read the vehicle off it.
///
/// The other end of the installation seam: before a Mobility Operator writes a
/// contract for a car, it fetches that car's provisioning certificate by PCID
/// and checks it `[OPCP §Provisioning Certificate Pool]`.
///
/// # Errors
///
/// Everything [`PoolRegistry::validate_in`] refuses, and
/// [`PncError::CommonNameIsNotAPcid`] where the leaf's Common Name is absent or
/// is not a well-formed PCID.
pub fn verify_provisioning<'a>(
    pools: &'a PoolRegistry,
    pool: &PoolId,
    chain: &[&'a [u8]],
    revocations: &RevocationList,
    at: time::OffsetDateTime,
) -> Result<VerifiedProvisioning, PncError> {
    let validated = pools.validate_in(
        pool,
        chain,
        Profile::OemProvisioningCertificate,
        at,
        revocations,
    )?;

    let common_name =
        validated
            .subject_common_name()
            .ok_or_else(|| PncError::CommonNameIsNotAPcid {
                common_name: None,
                detail: "the leaf carries no Common Name".to_owned(),
            })?;
    let pcid = Pcid::new(common_name).map_err(|source| PncError::CommonNameIsNotAPcid {
        common_name: Some(common_name.to_owned()),
        detail: source.to_string(),
    })?;

    Ok(VerifiedProvisioning {
        pcid,
        pool: pool.clone(),
        valid_until: instant(validated.leaf.not_after),
        anchor: validated
            .anchor
            .subject
            .common_name
            .unwrap_or("an anchor with no Common Name")
            .to_owned(),
    })
}

/// A certificate's `notBefore`/`notAfter`, which the kit gives as Unix seconds.
///
/// Saturating rather than fallible: the value came out of a certificate the
/// path validation has already accepted at `at`, so it is inside a range
/// `OffsetDateTime` holds — and a clamp that can never fire is better here than
/// a `Result` every caller would unwrap.
fn instant(unix_seconds: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(unix_seconds)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suite_this_build_cannot_check_is_refused_rather_than_skipped() {
        // Both curves Annex F admits are here; what this asserts is the shape
        // of the arm that is not. An unchecked signature is not a weaker
        // verification, it is none.
        let outcome = Curves.verify_with(Suite::EcdsaSha256, &[0x04], b"data", &[0u8; 64]);
        assert!(outcome.is_err(), "a one-byte key is not a key");
    }

    #[test]
    fn renewal_is_a_window_the_operator_states() {
        let contract = VerifiedContract {
            emaid: Emaid::parse("DE-8AA-CA2B3C4D5-L").expect("a valid EMAID"),
            pool: PoolId::new("hubject-ccp"),
            valid_until: time::macros::datetime!(2026-07-01 00:00 UTC),
            anchor: "V2G Root".to_owned(),
        };
        let at = time::macros::datetime!(2026-06-15 00:00 UTC);
        assert_eq!(contract.remaining(at), time::Duration::days(16));
        assert!(!contract.renewal_due(at, time::Duration::days(14)));
        assert!(contract.renewal_due(at, time::Duration::days(30)));
    }
}
