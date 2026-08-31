//! Charge detail records: the claim two companies settle against.
//!
//! A session is what happened; a CDR is a claim about it, sent to somebody who
//! was not there and who will pay against it. This crate builds those claims so
//! they cannot be internally inconsistent, accepts incoming ones exactly once,
//! and checks the ones somebody else built.
//!
//! # What is here
//!
//! - [`cdr`] — the record and its builder. The periods sum to the total,
//!   checked at construction; the record names the signed evidence it rests on
//!   by digest; and it is immutable, so a correction is a new record that
//!   supersedes the old one.
//! - [`ledger`] — idempotent acceptance that tells a **retransmission** from a
//!   **conflict**. An upsert keyed on the CDR id cannot, and quietly lets a
//!   partner restate a settled number.
//! - [`mod@validate`] — the pre-flight for a CDR from somewhere else: every problem
//!   at once, separated into what blocks settlement and what is worth knowing,
//!   and nothing repaired behind the caller's back.
//!
//! # The cross-check nobody runs
//!
//! A session records *how* it was authorised; the signed meter record states
//! *how strongly* the driver was identified `[OCMF Tab. 11]`. Those are two
//! statements about one event, and they can disagree — a session claiming Plug
//! & Charge whose signed record reports a bare RFID UID, for instance. When
//! they do, the one with a signature behind it is the one to believe, and the
//! CDR is refused rather than billed at the stronger claim's tariff.
//!
//! ```
//! # use emob_cdr::{CdrBuilder, EvidenceRef};
//! # use emob_session::IdentificationStrength;
//! // An ad-hoc session — a card at the point — cannot have established a
//! // secure, certificate-backed identity. Building this CDR fails.
//! # let _ = IdentificationStrength::Secure;
//! ```
//!
//! # No I/O, no clock
//!
//! Nothing here reads a clock, a socket or a file. The ledger is in memory and
//! persisting it is a service's job, so a month of roaming traffic replays as a
//! unit test.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod cdr;
pub mod error;
pub mod ledger;
pub mod validate;

pub use cdr::{Cdr, CdrBuilder, CdrKey, ChargingPeriod, EvidenceRef};
pub use error::CdrError;
pub use ledger::{Acceptance, CdrLedger};
pub use validate::{Finding, Report, Severity, validate};
