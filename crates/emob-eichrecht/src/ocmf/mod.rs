//! OCMF — the Open Charge Metering Format.
//!
//! The de-facto format for signed meter values at a charging station, and the
//! input the S.A.F.E. Transparenzsoftware accepts. A record looks like this:
//!
//! ```text
//! OCMF|{"FV":"1.4","PG":"T12345",…,"RD":[…]}|{"SD":"3045…"}
//! ```
//!
//! Three sections separated by pipes: a header, a JSON payload, and a JSON
//! signature over that payload.
//!
//! # Modules
//!
//! - [`model`] — the payload's types, following the specification's two-letter
//!   keys, with the semantics that decide billing ([`MeterState::is_billable`],
//!   [`TimeStatus::is_billable_for_time`]).
//! - [`mod@parse`] — reading a record **without destroying it**: the payload's raw
//!   byte span is kept, because that is what the signature covers.
//! - [`mod@verify`] — ECDSA verification against a registered public key.

pub mod model;
pub mod parse;
pub mod verify;

pub use model::{
    CurrentType, ErrorFlags, Identification, IdentificationLevel, MeterState, OcmfTime, Pagination,
    PaginationContext, Payload, Reading, ReadingUnit, TimeStatus, TransactionMarker,
};
pub use parse::{OcmfRecord, SignatureSection, parse};
pub use verify::{KeyType, PublicKey, SignatureAlgorithm, payload_digest, verify};
