//! The errors the domain model can produce.
//!
//! Every variant names the value that was wrong and, where it is not obvious,
//! what the right shape would have been. A message that says only "invalid" is
//! a support ticket; a message that says which section of which grammar failed
//! is a fix.

use core::fmt;

/// An identifier that does not fit the grammar it claims.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The value was blank.
    #[error("{kind} must not be empty")]
    Empty {
        /// Which identifier kind was being parsed.
        kind: &'static str,
    },

    /// The value is shorter than the grammar's minimum.
    #[error("{kind} must be at least {min} characters")]
    TooShort {
        /// Which identifier kind was being parsed.
        kind: &'static str,
        /// The grammar's minimum length.
        min: usize,
    },

    /// The first two characters are not an ISO 3166-1 alpha-2 country code.
    #[error("the first two characters must be an ISO 3166-1 alpha-2 country code")]
    BadCountry,

    /// The operator/party section is not three alphanumerics.
    #[error("the operator id must be three alphanumeric characters")]
    BadOperator,

    /// The provider section of a contract id is not three alphanumerics.
    #[error("the provider id must be three alphanumeric characters")]
    BadProvider,

    /// The instance section of a contract id contains something else.
    #[error("the contract instance must be alphanumeric")]
    BadInstance,

    /// The power-outlet section is empty, too long, or has illegal characters.
    #[error("the power outlet id must be 1 to 30 alphanumeric characters or '*'")]
    BadOutlet,

    /// The `E` that marks an EVSE id is missing.
    #[error("not an EVSE id: the outlet section must start with 'E' (a contract id has no 'E')")]
    NotAnEvse,

    /// A contract id is neither 14 nor 15 characters once separators are gone.
    #[error("a contract id is 14 or 15 characters, got {len}")]
    BadContractLength {
        /// The length that was actually seen.
        len: usize,
    },
}

/// A quantity that cannot mean what it claims.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuantityError {
    /// Energy that is billed must not be negative; direction is a separate
    /// field, so a negative magnitude is a sign error rather than an export.
    #[error("{what} must not be negative, got {value}")]
    Negative {
        /// Which quantity.
        what: &'static str,
        /// The value that was rejected.
        value: String,
    },

    /// Two quantities in different units were combined.
    #[error("cannot combine {left} with {right}")]
    UnitMismatch {
        /// The left-hand unit.
        left: &'static str,
        /// The right-hand unit.
        right: &'static str,
    },

    /// Two money amounts in different currencies were combined.
    #[error("cannot combine {left} with {right}: different currencies")]
    CurrencyMismatch {
        /// The left-hand currency.
        left: String,
        /// The right-hand currency.
        right: String,
    },

    /// A currency code that is not three letters.
    #[error("a currency code is three letters (ISO 4217), got {0:?}")]
    BadCurrency(String),
}

/// A convenient result alias for this crate.
pub type Result<T, E = CoreError> = core::result::Result<T, E>;

/// Anything `emob-core` can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// An identifier failed to parse.
    #[error(transparent)]
    Id(#[from] IdError),

    /// A quantity was inconsistent.
    #[error(transparent)]
    Quantity(#[from] QuantityError),

    /// An obligation was asked about outside any validity window it has.
    #[error("no version of obligation {0} is in force on the given date")]
    NoObligationInForce(&'static str),
}

impl fmt::Display for crate::ids::Spelling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Separated => f.write_str("separated"),
            Self::Packed => f.write_str("packed"),
        }
    }
}
