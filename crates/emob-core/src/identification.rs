//! How strongly a session was tied to a person, on an ordered scale.
//!
//! # Why this is vocabulary rather than a detail of one crate
//!
//! Two independent records make a claim about it, and the whole point is that
//! they can be compared:
//!
//! - the **session** knows which mechanism authorised it — a card at the point,
//!   a roaming `Authorize`, a contract certificate the vehicle presented;
//! - the **signed meter record** states how the signature component identified
//!   the user `[OCMF Tab. 11]`, and that statement is inside the bytes a
//!   private key covered.
//!
//! When the two disagree, the one with a signature behind it is the one to
//! believe. That comparison is only possible if both sides speak the same
//! scale, which is why the scale lives here — in the crate both depend on —
//! rather than in either of them.
//!
//! # What is deliberately not on the scale
//!
//! OCMF's error levels — `MISMATCH`, `INVALID`, `OUTDATED`, `UNKNOWN` — are not
//! weak assignments. They are failures: the certificate did not check out, the
//! trust anchor expired, the UIDs did not match. Putting them at the bottom of
//! an ordered scale would make "the certificate was rejected" compare as
//! *slightly worse than* an RFID UID, and a `>=` somewhere would then bill it.
//! They are handled where they belong, as findings in `emob-eichrecht`.

/// How strongly a user was tied to a session, ordered weakest to strongest.
///
/// A coarsening of `[OCMF Tab. 11]`'s levels with the error states left out, so
/// that comparing two strengths is always meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum IdentificationStrength {
    /// No assignment at all — `IS: false`, or `IL: NONE`.
    ///
    /// The default, because a claim nobody made is the weakest possible claim
    /// and every comparison against it should come out that way.
    #[default]
    None,
    /// Unsecured — a bare RFID UID, a MAC address.
    Hearsay,
    /// A backend vouched for it.
    Trusted,
    /// The signature component verified it by special measures.
    Verified,
    /// A cryptographic signature certifies the assignment.
    Certified,
    /// A secure feature established it — a secure card, Plug & Charge.
    Secure,
}

impl core::fmt::Display for IdentificationStrength {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Hearsay => "hearsay",
            Self::Trusted => "trusted",
            Self::Verified => "verified",
            Self::Certified => "certified",
            Self::Secure => "secure",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_is_ordered_and_none_is_the_floor() {
        assert!(IdentificationStrength::Secure > IdentificationStrength::Certified);
        assert!(IdentificationStrength::Certified > IdentificationStrength::Verified);
        assert!(IdentificationStrength::Verified > IdentificationStrength::Trusted);
        assert!(IdentificationStrength::Trusted > IdentificationStrength::Hearsay);
        assert!(IdentificationStrength::Hearsay > IdentificationStrength::None);
        assert_eq!(
            IdentificationStrength::default(),
            IdentificationStrength::None,
            "a claim nobody made is the weakest claim"
        );
    }

    #[test]
    fn strengths_render_as_prose() {
        assert_eq!(IdentificationStrength::Secure.to_string(), "secure");
        assert_eq!(IdentificationStrength::None.to_string(), "none");
    }
}
