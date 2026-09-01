//! What can be wrong at the seam.
//!
//! Every variant names the layer it failed at, because the layers are nested
//! three deep and "the session will not bill" is the same sentence whether the
//! base64 was truncated, the record was tampered with, or the station is
//! configured for a format nobody here reads.

use emob_session::{MeterError, SessionError};

/// Something wrong between an OCPP transaction and a billable session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SeamError {
    /// The `SampledValue.value` did not hold a `SignedMeterValueType`.
    ///
    /// OCPP 1.6 serialises the whole object into that string field beside
    /// `format: "SignedData"` `[OCA SMV §3.2.1]`, so a station that puts a bare
    /// number there is one whose signed values are not where the whitepaper
    /// says they are.
    #[error(
        "a SignedData sampled value did not hold a SignedMeterValueType: {detail}. OCPP 1.6 serialises the whole object into the `value` string [OCA SMV §3.2.1]"
    )]
    BadSampledValue {
        /// What the JSON reader said.
        detail: String,
    },

    /// The station encodes its signed values in a format this crate does not
    /// read.
    ///
    /// Named rather than swallowed: a fleet configured for EDL needs a
    /// different verifier, and telling its operator that is a different message
    /// from telling them a session did not verify.
    #[error(
        "signed meter values encoded as {encoding_method} need a verifier this crate does not have; it reads OCMF"
    )]
    UnknownEncodingMethod {
        /// What `encodingMethod` said.
        encoding_method: String,
    },

    /// `signedMeterData` decoded as neither base64 nor a bare record.
    #[error("the signed meter data did not decode: {detail}")]
    UndecodableSignedData {
        /// Which layer gave up.
        detail: String,
    },

    /// The record inside did not parse as OCMF.
    #[error("the signed data set did not parse: {detail}")]
    BadRecord {
        /// What the OCMF parser said.
        detail: String,
    },

    /// The `publicKey` envelope did not decode.
    ///
    /// Not fatal to billing on its own — the key that matters comes from the
    /// registry — but a station whose claimed key is unreadable is one whose
    /// provisioning nobody can check against the cabinet.
    #[error("the public key the station claims did not decode: {detail}")]
    UndecodableClaimedKey {
        /// Which layer gave up.
        detail: String,
    },

    /// A transaction carried no signed meter value at all.
    ///
    /// **The seam rule, as an error.** OCPP's own numeric fields — `meterStart`,
    /// `meterStop`, `SampledValue.value` — are telemetry: they arrive as
    /// floating point in every ledger that carries them, and the OCA's own
    /// example message shows `meterStop` reporting the *lifetime* register
    /// while the transaction's own signed difference is three orders of
    /// magnitude smaller. There is no repair for a transaction without signed
    /// values, only a different question: whether this jurisdiction lets an
    /// unsigned value be billed at all `[MessEG §33]`.
    #[error(
        "transaction {transaction_id} carries no signed meter value: OCPP's own numeric fields are telemetry, and a kilowatt-hour nothing signed is not one this chain will bill [MessEG §33]"
    )]
    NoSignedValues {
        /// Which transaction.
        transaction_id: String,
    },

    /// A transaction with no events at all.
    #[error("a transaction needs at least one event")]
    NoEvents,

    /// The transaction never ended.
    ///
    /// A CDR for a session in progress is a claim about a number that is still
    /// changing, so the seam refuses to close one that the station has not.
    #[error("transaction {transaction_id} has no ending event: it is still running")]
    StillRunning {
        /// Which transaction.
        transaction_id: String,
    },

    /// The signed records do not form a coherent meter series.
    ///
    /// Kept apart from [`Self::Session`] because the two fail for different
    /// reasons and only one of them is the station's fault: a register that
    /// ran backwards between two *signed* records is a metrology incident, and
    /// an illegal state transition is a CSMS that lost track of its own
    /// transaction.
    #[error(transparent)]
    Meter(#[from] MeterError),

    /// The session could not be assembled from the events.
    #[error(transparent)]
    Session(#[from] SessionError),
}
