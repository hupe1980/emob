//! Charging sessions: how they were authorised, what the meter said, and how
//! that divides across the quarter hours the market settles in.
//!
//! # What is here
//!
//! - [`auth`] — the five paths a session can be authorised by, and why they are
//!   not interchangeable: AFIR requires a contract-free one at every public
//!   point, and the signed record states how strongly the driver was
//!   identified.
//! - [`meter`] — cumulative register readings with the reason each was taken.
//!   `Sample.Clock` is the one that can settle a quarter hour without guessing.
//! - [`split`] — the quarter-hour split, which **conserves energy exactly by
//!   construction** and records for every slot whether it was measured or
//!   interpolated.
//! - [`session`] — the session itself, as a state machine that refuses what the
//!   protocol forbids and keeps **when** each state was entered, because
//!   "suspended" is an interval a tariff prices rather than a status light.
//!
//! # The one idea worth reading about
//!
//! Splitting a session across quarter hours naively — computing each slot
//! independently — leaves a sum that is a few milliwatt-hours off the total,
//! and the usual fix shoves the difference into the last slot, misattributing
//! energy to whoever held it. Instead [`split::into_quarter_hours`] computes
//! the **cumulative** value at each boundary once and takes differences, so the
//! sum telescopes and equals the total to the last digit, always:
//!
//! ```
//! use emob_session::{MeterReading, MeterSeries, ReadingContext, split};
//! use emob_core::{Direction, Energy};
//! use rust_decimal::Decimal;
//! use std::str::FromStr;
//! use time::macros::datetime;
//!
//! # let kwh = |s: &str| Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap();
//! // 10:01 to 10:22 — the boundary at 10:15 falls two thirds of the way through,
//! // and 7 kWh times two thirds does not terminate.
//! let series = MeterSeries::new(Direction::Import, vec![
//!     MeterReading::new(datetime!(2026-01-02 10:01 +1), kwh("0"), Direction::Import, ReadingContext::TransactionBegin),
//!     MeterReading::new(datetime!(2026-01-02 10:22 +1), kwh("7"), Direction::Import, ReadingContext::TransactionEnd),
//! ])?;
//!
//! let split = split::into_quarter_hours(&series)?;
//! assert!(split.conserves());   // exactly 7 kWh, across two slots
//! assert!(!split.fully_measured());  // …and it says it had to interpolate
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # No I/O, no clock
//!
//! Nothing here reads a clock, a socket or a file. Every instant is an
//! argument, so a session from two years ago splits today exactly as it split
//! then.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod auth;
pub mod meter;
pub mod session;
pub mod split;

pub use auth::{AuthError, AuthPath, Authorization, Subject, TokenRef};
pub use meter::{MeterError, MeterReading, MeterSeries, ReadingContext};
pub use session::{EndReason, Session, SessionError, SessionState, StateChange};
pub use split::{Provenance, SessionSplit, Slot, SplitError};
// The settlement grid is market vocabulary and lives in `emob-core`; it is
// re-exported because every consumer of a `Slot` needs it in the same breath.
pub use emob_core::QuarterHour;
