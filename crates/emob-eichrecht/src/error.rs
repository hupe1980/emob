//! What can go wrong between a signed meter value and an invoice.
//!
//! # Two layers, and only one of them is here
//!
//! Reading an OCMF record and checking its ECDSA signature are the [`ocmf`]
//! crate's questions, and it answers them with [`ocmf::ParseError`] and
//! [`ocmf::VerifyError`].
//!
//! What is here is the half `ocmf` does not have: **which key is this charge
//! point's key**. That is a registry question, answered out of band from a type
//! approval or a provisioning run, and getting it wrong is how a valid signature
//! over a valid payload comes from a meter that is not the one on the invoice.

/// A record whose signing component could not be resolved to a key.
///
/// The signature arithmetic is [`ocmf::VerifyError`]; this is everything before
/// it — finding out **whose** key to check against, and whether the registry
/// holds one that was valid when the record was signed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyLookupError {
    /// No key is registered for the signing component the record names.
    #[error("no public key is registered for signing component {component}")]
    NoKeyForComponent {
        /// The component that was looked up.
        component: String,
    },

    /// A key is registered for the component, and none of its windows covers
    /// the instant the record was signed at.
    ///
    /// Distinct from [`Self::NoKeyForComponent`], and the distinction is what
    /// an operator acts on: nothing registered is a provisioning gap, while a
    /// key whose window has closed is a key that was replaced without the
    /// replacement being registered — or a record from before the component
    /// was commissioned. Both are answers; "no key" for the second is a
    /// misdirection.
    #[error(
        "a public key is registered for signing component {component} but none of its validity \
         windows covers {at}: {windows}"
    )]
    NoKeyValidAt {
        /// The component the record identifies.
        component: String,
        /// The instant the key was needed for.
        at: time::OffsetDateTime,
        /// The windows the registry does hold, so the gap is visible.
        windows: String,
    },

    /// The record names no signing component at all, so no key can be found.
    ///
    /// `[OCMF §Relation of Serial Numbers]` gives four ways a record can
    /// identify what signed it — a gateway-and-meter pair, a meter serial, a
    /// gateway serial, or the charge point's own id — and a record carrying
    /// none of them is one no registry can answer for.
    #[error(
        "the record names no signing component: none of a meter serial, a gateway serial or a \
         charge point id is present"
    )]
    NoSigningComponent,

    /// A record that could not be read at all, so there was nothing to look up.
    #[error("the record could not be read: {0}")]
    Unreadable(#[from] ocmf::ParseError),
}

/// A record that was found a key and did not verify against it.
///
/// A thin wrapper so a caller can hold one error for the whole path from
/// "which key" to "does it check out" without flattening two very different
/// diagnostics into one string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EichrechtError {
    /// The key could not be found — see [`KeyLookupError`].
    #[error(transparent)]
    Key(#[from] KeyLookupError),

    /// The key was found and the signature did not check out, or could not be
    /// checked — see [`ocmf::VerifyError`].
    #[error(transparent)]
    Signature(#[from] ocmf::VerifyError),

    /// The record could not be read.
    #[error(transparent)]
    Parse(#[from] ocmf::ParseError),
}
