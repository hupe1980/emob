//! Tariffs: what a session costs, what the driver is shown, and whether the
//! two are allowed to be what they are.
//!
//! # One tariff, two readers
//!
//! A charging tariff has two jobs that platforms normally implement twice: it
//! **rates** a finished session, and it is **displayed** to a driver before the
//! session starts `[AFIR Art. 5(4)]`. When those come from two places they
//! drift, and the result is a driver charged something other than the price
//! they were shown — precisely the breach the article exists to prevent.
//!
//! Here [`rate`] and [`describe`] read the same
//! [`PriceComponent`] values off the same [`Tariff`]. Neither can quote a
//! number the other does not use, and a test asserts it across every dimension.
//!
//! ```
//! use emob_tariff::{Chargeable, Dimension, PriceComponent, Tariff, TariffKind, describe, rate};
//! use emob_core::{Currency, Energy};
//! # use rust_decimal::Decimal;
//! # use std::str::FromStr;
//! # use time::macros::datetime;
//! # let dec = |s: &str| Decimal::from_str(s).unwrap();
//! # let at = datetime!(2026-01-02 10:00 +1);
//! let tariff = Tariff::simple(
//!     "ad-hoc".parse()?,
//!     Currency::EUR,
//!     TariffKind::AdHoc,
//!     vec![
//!         PriceComponent::new(Dimension::Flat, dec("0.50")),
//!         PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
//!     ],
//! );
//!
//! // What the driver sees — per kWh first, whatever order it was written in.
//! assert_eq!(describe(&tariff, at).one_line(), "0.49 EUR / kWh · 0.50 EUR / session");
//!
//! // What the driver pays, from the same numbers.
//! let session = Chargeable::energy_only(
//!     Energy::from_kwh(dec("29.500"))?,
//!     at,
//!     at + time::Duration::minutes(30),
//! )?;
//! let rated = rate(&tariff, &session);
//! assert_eq!(rated.gross().to_string(), "14.96 EUR");   // 29.500 × 0.49 + 0.50
//! assert_eq!(rated.tax().to_string(), "2.31 EUR");      // 19 % of the electricity
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # The order is the regulation's
//!
//! Below 50 kW `[AFIR Art. 5(4)]` prescribes the display order in as many
//! words: price per kWh, then per minute, then per session, then anything else.
//! [`Dimension`] is declared in that order and derives [`Ord`], so **sorting
//! the components is complying with the article** — in the display and on the
//! invoice alike.
//!
//! # A tariff can be unlawful on its own
//!
//! At 50 kW and above the ad-hoc price "shall be based on the price per kWh",
//! with an occupancy fee per minute permitted *in addition*. So a
//! per-minute-only tariff is lawful on a 22 kW post and unlawful on the 150 kW
//! charger beside it, and [`check_afir`] is the function that says so.
//!
//! # A session is a sequence of periods, so tiers actually tier
//!
//! [`rate`] walks the session's periods in order and asks which element applies
//! at the start of each one, carrying the cumulative energy and duration. That
//! is what makes "the first 10 kWh at 0.39, the rest at 0.59" mean what it
//! says — rated against the session *total* instead, a 30 kWh session reprices
//! all thirty at 0.59, including the ten the driver was quoted at 0.39. The
//! quarter-hour slots `emob-session` already produces are exactly the right
//! periods, so the split that conserves energy is also the input that prices
//! it.
//!
//! # Every term of the total is a line
//!
//! [`rate`] returns one [`Line`] per *distinct price* that applied, with its
//! quantity, its unit price and its amount — so a tiered session shows both
//! tiers. The total is their sum plus at most one [`Adjustment`], the minimum
//! or maximum, which is a term a reader can see rather than a number that
//! moved. Rounding up to a block size produces a [`RatingNote`], because it is
//! always against the customer.
//!
//! # Tax is a breakdown, not a number
//!
//! [`Rated::tax_summary`] returns one [`TaxLine`] per VAT rate — the shape
//! EN 16931 requires and `[UStG §14]` makes mandatory between businesses in
//! Germany. Electricity and a service fee can sit in different categories, so
//! an invoice stating only a gross total cannot be checked.
//!
//! # No I/O, no clock
//!
//! Every instant is an argument, including the one that selects a time-of-day
//! element — which is also what lets a display answer "what will this cost at
//! 22:00" rather than only "what does it cost now".

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod conformance;
pub mod display;
pub mod rating;
pub mod tariff;
pub mod version;

pub use conformance::{Conformance, Objection, check_afir};
pub use display::{DisplayLine, PriceDescription, Tier, describe};
pub use rating::{
    Adjustment, AdjustmentKind, Chargeable, ChargeableError, Line, Period, Rated, RatingNote,
    SessionState, TaxLine, rate,
};
pub use tariff::{
    Dimension, PriceComponent, Restrictions, Tariff, TariffElement, TariffKind, TaxIncluded,
};
pub use version::{TariffFingerprint, TariffHistory, TariffHistoryError};
