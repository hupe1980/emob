//! A deterministic e-mobility fleet, driven through the whole chain.
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
pub use station::{ChargedSession, SessionPlan, VirtualStation};
