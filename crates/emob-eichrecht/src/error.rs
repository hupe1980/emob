//! What can go wrong between a signed meter value and an invoice.

/// An OCMF record that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OcmfError {
    /// The record does not start with the `OCMF` header.
    #[error("not an OCMF record: expected the header 'OCMF', found {found:?}")]
    BadHeader {
        /// What was in the header position.
        found: String,
    },

    /// A pipe-separated section is missing.
    #[error("the record has no {section} section")]
    MissingSection {
        /// Which section.
        section: &'static str,
    },

    /// More than three sections: a pipe appeared inside one of them, which the
    /// format forbids.
    #[error("the record has more than three sections: '|' must not appear inside a section")]
    TooManySections,

    /// A section is not valid JSON.
    #[error("the {section} section is not valid JSON: {detail}")]
    BadJson {
        /// Which section.
        section: &'static str,
        /// What the JSON parser said.
        detail: String,
    },

    /// A mandatory field is absent.
    #[error("the mandatory field {field} is missing")]
    MissingField {
        /// The field's OCMF key.
        field: &'static str,
    },

    /// A field has the wrong JSON type.
    #[error("{field} must be a {expected}")]
    BadFieldType {
        /// The field's OCMF key.
        field: &'static str,
        /// What was expected.
        expected: &'static str,
    },

    /// `PG` is not `<T|F><number>`.
    #[error(
        "{value:?} is not a pagination value: expected T or F followed by a number without leading zeros"
    )]
    BadPagination {
        /// The value that was rejected.
        value: String,
    },

    /// `TM` could not be read.
    #[error("{value:?} is not an OCMF time: {detail}")]
    BadTime {
        /// The value that was rejected.
        value: String,
        /// Why.
        detail: String,
    },

    /// A numeric field could not be read as an exact decimal.
    #[error("{value} is not an exact decimal: {detail}")]
    BadNumber {
        /// The value that was rejected.
        value: String,
        /// Why.
        detail: String,
    },

    /// `ST` is outside Table 10.
    #[error("{code:?} is not a meter state (OCMF Table 10)")]
    UnknownMeterState {
        /// The code that was rejected.
        code: String,
    },

    /// `TX` is outside Table 7.
    #[error("{code:?} is not a transaction marker (OCMF Table 7)")]
    UnknownTransactionMarker {
        /// The code that was rejected.
        code: String,
    },

    /// The time-status letter is outside Table 19.
    #[error("{code:?} is not a time status (OCMF Table 19)")]
    UnknownTimeStatus {
        /// The code that was rejected.
        code: String,
    },

    /// `RU` is outside Table 20.
    #[error("{unit:?} is not a known unit (OCMF Table 20)")]
    UnknownUnit {
        /// The unit that was rejected.
        unit: String,
    },

    /// `RT` is neither `AC` nor `DC`.
    #[error("{value:?} is not a current type (OCMF Table 21)")]
    UnknownCurrentType {
        /// The value that was rejected.
        value: String,
    },

    /// `IL` is outside Table 11.
    #[error("{level:?} is not an identification level (OCMF Table 11)")]
    UnknownIdentificationLevel {
        /// The value that was rejected.
        level: String,
    },

    /// `SE` names an encoding the format does not define.
    #[error("{encoding:?} is not a signature encoding: expected 'hex' or 'base64'")]
    UnknownSignatureEncoding {
        /// The encoding that was rejected.
        encoding: String,
    },

    /// The signature data did not decode.
    #[error("the signature is not valid {encoding}: {detail}")]
    BadSignatureEncoding {
        /// Which encoding was declared.
        encoding: &'static str,
        /// Why it failed.
        detail: String,
    },
}

/// A signature that could not be checked, or did not check out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VerifyError {
    /// **The signature does not match the payload.**
    ///
    /// The one that matters: either the bytes changed after signing, or they
    /// were never signed by this key.
    #[error("the signature does not match the payload")]
    SignatureMismatch,

    /// The algorithm identifier is not in Table 22.
    #[error("{algorithm:?} is not a known signature algorithm (OCMF Table 22)")]
    UnknownAlgorithm {
        /// The identifier that was rejected.
        algorithm: String,
    },

    /// The algorithm is known but this build cannot check it.
    #[error("{algorithm} is a valid OCMF algorithm that this build cannot verify")]
    UnsupportedAlgorithm {
        /// The identifier.
        algorithm: String,
    },

    /// The record and the key are on different curves.
    #[error("the record is signed with {record} but the key is {key}")]
    AlgorithmMismatch {
        /// What the record declared.
        record: String,
        /// What the key is.
        key: String,
    },

    /// The signature is not DER.
    #[error("signature MIME type {mime_type:?} is not supported: expected application/x-der")]
    UnsupportedSignatureFormat {
        /// What was declared.
        mime_type: String,
    },

    /// The public key did not decode.
    #[error("the public key could not be read: {detail}")]
    BadKeyEncoding {
        /// Why.
        detail: String,
    },

    /// The signature bytes did not decode.
    #[error("the signature could not be read: {detail}")]
    BadSignatureEncoding {
        /// Why.
        detail: String,
    },

    /// No key is registered for the signing component the record names.
    #[error("no public key is registered for signing component {serial:?}")]
    NoKeyForComponent {
        /// The serial that was looked up.
        serial: String,
    },

    /// The record names no signing component at all, so no key can be found.
    #[error("the record names neither a meter serial nor a gateway serial")]
    NoSigningComponent,
}
