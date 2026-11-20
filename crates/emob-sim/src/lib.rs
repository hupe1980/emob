//! A deterministic e-mobility fleet, driven through the whole chain.
//!
//! A virtual station signs genuine OCMF with its own key; the same seed gives
//! the same keys, registers and records on every machine.
//!
//! ```
//! use emob_sim::{Rng, SessionPlan, VirtualStation};
//!
//! let mut rng = Rng::stream(2026, "fleet");
//! let mut station = VirtualStation::new(1, &mut rng);
//! let start = time::macros::datetime!(2026-06-01 08:00 UTC);
//! let plan = SessionPlan {
//!     started_at: start,
//!     ended_at: start + time::Duration::minutes(30),
//!     energy: emob_core::Energy::from_wh(rust_decimal::Decimal::from(12_000)).unwrap(),
//!     clock_aligned: true,
//!     connected_before_charging: time::Duration::seconds(30),
//! };
//! let session = station.charge(&plan, &[]);
//! // Every record verifies under the station's own key.
//! for text in &session.records {
//!     let record = ocmf::Record::parse(text).unwrap();
//!     assert!(ocmf::verify(&record, &station.public_key()).is_ok());
//! }
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod day;
pub mod fault;
pub mod rng;
pub mod station;

pub use day::{DayOutcome, ReferenceDay, ReferenceDayBuilder, Refused};
pub use fault::{Fault, FaultPlan, Rate};
pub use rng::Rng;
pub use station::{ChargedSession, Identification, SessionPlan, VirtualStation};
