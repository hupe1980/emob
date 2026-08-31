//! The domain model every other `emob` crate is written against.
//!
//! `emob` is the open-source e-mobility operating stack: the CSMS a charging
//! station connects to, the roaming node a partner peers with, the Eichrecht
//! evidence chain a signed meter value survives in, and the EMP that turns all
//! of it into a driver's invoice. This crate holds the vocabulary — the
//! identifiers, the quantities, the facts about a charge point, and the
//! regulatory calendar those facts are judged against.
//!
//! # What is here
//!
//! - [`ids`] — [`EvseId`], [`Emaid`] and friends:
//!   two grammars per identifier, canonical equality, and a `Display` that
//!   returns the text that arrived.
//! - [`quantity`] — [`Energy`],
//!   [`Money`], [`PricePerKwh`],
//!   [`Direction`]: exact decimals, no binary floats
//!   anywhere, and direction as a field rather than a sign.
//! - [`station`] — [`ChargePointProfile`]: the
//!   facts about a charge point that regulation actually asks about.
//! - [`obligation`] — the obligation calendar: AFIR, DA-656, LSV 2026,
//!   MessEG/PTB-A and the THG preconditions as dated, cited, executable rules,
//!   and [`assess`](obligation::assess()) to judge a point against all of them.
//!
//! # No I/O, no clock
//!
//! Nothing in this crate reads a clock, a socket or a file. Every function that
//! needs "now" takes it as an argument, so a compliance question about a date
//! two years out is the same call as one about today, and `just purity` fails
//! the build if that ever stops being true.
//!
//! # Example: the whole crate in one question
//!
//! ```
//! use emob_core::obligation::{assess, Verdict};
//! use emob_core::station::{AdHocPayment, ChargePointProfile, V2gCommunication};
//! use time::macros::date;
//!
//! let mut point = ChargePointProfile::bare("DE*AB7*E840*6487".parse()?, date!(2026-06-01));
//! point.ad_hoc_payment = AdHocPayment::CardReader;
//! point.v2g = V2gCommunication::both_generations();
//!
//! let report = assess(&point, date!(2027-01-01));
//! for finding in report.failing() {
//!     println!("{} — {}", finding.obligation.citation, finding.obligation.remedy);
//! }
//! assert_eq!(report.verdict(), Verdict::Failing); // the data duties are still open
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod ids;
pub mod obligation;
pub mod quantity;
pub mod station;

pub use error::{CoreError, IdError, QuantityError, Result};
pub use ids::{
    CdrId, ContractId, Emaid, EvcoId, EvseId, LocationId, PartyId, SessionId, StationId, TariffId,
};
pub use quantity::{Currency, Direction, Energy, Money, PricePerKwh};
pub use station::ChargePointProfile;
