//! Why a limit write does not become a ceiling.
//!
//! Three refusals, and each is a NACK the Energy Guard can act on rather than a
//! value substituted for one nobody sent. That is the whole discipline of this
//! module: `[LPC-001]` and `[LPC-004]` are conditions on the *write*, and a
//! reader that repaired a broken one would apply a limit the grid operator did
//! not set — under a statute whose evidence is the pair of what was asked for
//! and what was answered (`[LPC §4.1.5]`).

use rust_decimal::Decimal;

/// Why a payload does not carry a limit this side can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EebusError {
    /// The payload is not a limit write, or carries no entry for this limit.
    #[error("this is not a limit write: {detail}")]
    NotALimitWrite {
        /// What was wrong with it.
        detail: String,
    },

    /// It carries a limit whose value this side cannot represent.
    ///
    /// Refused rather than approximated. A `scale` is a signed 16-bit number
    /// the peer chooses, so a value outside a decimal is one well-formed
    /// message away — and a substituted figure is a limit nobody asked for,
    /// applied to a site that will obey it.
    #[error(
        "the limit cannot be read exactly and this side will not approximate one: {detail} — a \
         NACK is a fact the Energy Guard can act on, and a substituted value is not"
    )]
    UnreadableLimit {
        /// What could not be read.
        detail: String,
    },

    /// It carries a limit below zero, which `[LPC-001]` forbids.
    ///
    /// Worth its own variant because of where it would land: a ceiling below
    /// zero is below the 6 A floor every point has, so the site would hold
    /// every box at zero — over a value the specification says cannot arrive.
    #[error(
        "the limit is {watts} W and [LPC-001] puts it at or above zero: a ceiling below zero is \
         under the floor every point has, and the site would hold every box at nothing"
    )]
    NegativeLimit {
        /// What arrived.
        watts: Decimal,
    },
}
