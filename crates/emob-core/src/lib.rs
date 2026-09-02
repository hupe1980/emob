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
//! - [`quantity`] — [`Energy`], [`Money`], [`Currency`], [`Direction`]: exact
//!   decimals, no binary floats anywhere, direction as a field rather than a
//!   sign, and rounding that follows the currency's own minor unit.
//! - [`identification`] — [`IdentificationStrength`]: the ordered scale a
//!   session's authorisation path and its signed meter record are compared on.
//! - [`period`] — [`QuarterHour`]: the grid the German market settles on, a
//!   meter stores its load profile in, and a price may move on — and the
//!   footnote about *which* instant names it that shifts a settlement file by
//!   fifteen minutes when it is read past. Also [`ClockResolution`], the floor
//!   below which a measured span is not a number an invoice may use.
//! - [`station`] — [`ChargePointProfile`] and
//!   [`ProviderProfile`]: the facts about a charge point, and about a mobility
//!   service provider, that regulation actually asks about.
//! - [`zone`] — [`TimeZone`]: the named IANA zone a tariff's wall clock is read
//!   in. An offset is what a timestamp was written with; a zone is the rule that
//!   decides the offset, and a price per hour of the day is meaningless without
//!   one.
//! - [`crossing`] — [`Crossing`] and [`Note`]: a value carried onto a wire and
//!   the account of what the crossing cost, by JSON Pointer into the document
//!   the recipient will be reading. Shared, because three seams — OCPI, the
//!   DATEX II national access point feed and OCPP 2.1's tariff — answer the
//!   same question and must answer it in the same words.
//! - [`obligation`] — the obligation calendar: AFIR, DA-656, LSV 2026,
//!   MessEG/PTB-A and the THG preconditions as dated, cited, executable rules,
//!   with [`assess`](obligation::assess()) and
//!   [`assess_provider`](obligation::assess_provider()) to judge each side
//!   against all of them.
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
//! assert_eq!(report.verdict(), Verdict::Failing); // the data and register duties are still open
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

mod check_digit;
pub mod crossing;
pub mod error;
pub mod identification;
pub mod ids;
pub mod obligation;
pub mod period;
pub mod quantity;
pub mod station;
pub mod wire;
pub mod zone;

pub use crossing::{Crossing, Note};
pub use error::{CoreError, IdError, QuantityError, Result};
pub use identification::IdentificationStrength;
pub use ids::{
    CdrId, ContractId, Emaid, EvcoId, EvseId, LocationId, PartyId, Role, SessionId, StationId,
    TariffId,
};
pub use period::{ClockResolution, ClockResolutionError, QuarterHour};
pub use quantity::{Currency, Direction, Energy, Money};
pub use station::{
    Accessibility, AdHocPayment, ChargePointProfile, ChargingMode, CurrentType, DataPublication,
    EnergyMeasurementPoint, FurtherIdentifiers, MeteringPosture, Nis2Class, Notice, OperatorChange,
    Ownership, PriceConduct, PriceTransparency, ProviderProfile, QuotaPosture, RegisterPublication,
    Registration, RiskManagement, UndertakingProfile, V2gCommunication,
};
pub use zone::{Local, TimeZone, ZoneError};
