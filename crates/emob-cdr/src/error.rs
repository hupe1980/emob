//! What can go wrong building a CDR.

use emob_core::Energy;
use emob_session::{AuthPath, IdentificationStrength, SessionError};

/// A CDR that could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CdrError {
    /// The session is still running.
    #[error("a CDR cannot be built from a session that has not ended")]
    SessionNotEnded,

    /// No key was given.
    #[error("a CDR needs a key: a party and an id unique within it")]
    NoKey,

    /// The periods do not sum to the total.
    #[error("the periods sum to {periods} but the total is {total}")]
    DoesNotConserve {
        /// What the periods add up to.
        periods: Energy,
        /// What was claimed.
        total: Energy,
    },

    /// The signed record claims a stronger identification than the
    /// authorisation path can support.
    #[error(
        "the session claims {claimed} authorisation, which supports at most {ceiling} identification, but the signed record reports {signed}"
    )]
    AuthStrengthMismatch {
        /// The path the session claims.
        claimed: AuthPath,
        /// The strongest level that path can honestly support.
        ceiling: IdentificationStrength,
        /// What the signed record states.
        signed: IdentificationStrength,
    },

    /// The session could not be split.
    #[error(transparent)]
    Session(#[from] SessionError),
}
