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
//! # The four cross-checks nobody runs
//!
//! Each asks the same question of a different quantity: **does the session's own
//! account of itself agree with what a meter signed?** They stay four because
//! they take different things away — a failed assignment removes the payer, an
//! untrustworthy clock removes the duration — and one that collapsed them would
//! refuse a whole record over a fee nobody was charged. All four read the
//! evidence rather than a field a caller filled in, which is what
//! [`EvidenceRef::from_evidence`](cdr::EvidenceRef::from_evidence) is for.
//!
//! **Whether the energy may be billed.** A CDR's energy comes from the
//! *session's* meter series, so without this a record is priced off a register
//! nothing verified while carrying an `EvidenceRef` that makes it look checked —
//! a settled record for a forged session, with every unit test passing (D70).
//!
//! **Which way it flowed.** `[OCMF Tab. 25]` reserves `B*` for import and `C*`
//! for export, so the signed register *states* the direction, and a record
//! claiming a draw over a `C2` register is a V2G discharge billed as consumption
//! — the operator paying for energy the driver supplied.
//!
//! **Who was charging.** A session records *how* it was authorised; the signed
//! meter record states *how strongly* the driver was identified
//! `[OCMF Tab. 11]`. Those are two statements about one event, and they can
//! disagree — a session claiming Plug & Charge whose signed record reports a
//! bare RFID UID, for instance. When they do, the one with a signature behind
//! it is the one to believe, and the CDR is refused rather than billed at the
//! stronger claim's tariff.
//!
//! The strength is read off the evidence by
//! [`EvidenceRef::from_evidence`](cdr::EvidenceRef::from_evidence), never
//! filled in by a caller: a hand-filled field can be filled with whatever value
//! makes the record build, which is the opposite of a check.
//!
//! **Whether a duration may be billed at all.** OCMF states how far the
//! station's clock can be trusted `[OCMF Tab. 19]` and flags a time value as
//! unusable separately from an energy one. A tariff charging per minute — the
//! occupancy fee `[AFIR Art. 5(4)]` permits at 50 kW and above — is billing a
//! duration, and a duration billed off a clock the signed record does not vouch
//! for is a number nobody can defend. The energy is unaffected, so the builder
//! names the fix: price the session per kWh.
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

pub use cdr::{Cdr, CdrBuilder, CdrKey, ChargingPeriod, Cost, EvidenceRef};
pub use error::CdrError;
pub use ledger::{Acceptance, CdrLedger};
pub use validate::{Finding, Report, Severity, validate};
