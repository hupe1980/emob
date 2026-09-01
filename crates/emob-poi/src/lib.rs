//! The charge point register, and the national access point feed it generates.
//!
//! # One number, two duties
//!
//! `[AFIR Art. 5(2)]` makes the price a driver is shown before a session the
//! price they may be charged for it. `[AFIR Art. 20(2)(c)]` makes that same
//! ad-hoc price data an operator must publish, free of charge, through the
//! national access point — the Mobilithek in Germany, in the DATEX II
//! Recharging profile, from **14 April 2026** `[DATEX-II-Profil]`.
//!
//! Two duties about one number, and almost every stack in this market computes
//! it twice: once in the billing system that rates the CDR, and once in the
//! export job that fills the feed. Two computations is two chances to be wrong,
//! and the failure is asymmetric — a feed is read by route planners and
//! comparison sites, and nobody ever reconciles it against an invoice.
//!
//! So this crate does not have a price model. It publishes
//! [`emob_tariff::Tariff`], the same value [`emob_tariff::rate`] charges with,
//! in exact decimal from the tariff to the JSON number. See [`rate`] for the
//! two things the profile's vocabulary cannot say about it.
//!
//! # The register is upstream of the feed
//!
//! `[LSV26 §4(1)]` makes three events notifiable — commissioning,
//! decommissioning and a change of operator — so an operator meeting its
//! notification duty knows, for every point, which lifecycle state it is in.
//! `[DATEX-II-Profil Tab. B.45]` has a literal for two of them. Those facts
//! usually live in different systems, and the feed is usually generated from
//! the one that does not know — which is why decommissioned points stay
//! published as `available` for months.
//!
//! [`status::Report`] cannot be constructed for a status the register
//! contradicts. That is the whole mechanism: there is no way to build the
//! document.
//!
//! # The silence between the two publications
//!
//! A status message carries no infrastructure. Every object in it is a
//! versioned reference into a table publication sent separately, and a
//! reference that does not resolve is dropped by the consumer rather than
//! rejected. Bump a point's `versionG` in one job and not the other and that
//! point leaves every map, with no error anywhere. [`feed`] is where that is
//! made to be an error.
//!
//! # What this crate is not
//!
//! It does not fetch, push or schedule. The Mobilithek's `snapshotPush` is a
//! service's business; a publication here is a value and a string. `just
//! purity` fails the build on a domain crate that opens a socket or reads a
//! clock, which is why [`datex::table::publication`] takes the publication time
//! as an argument — an export replayed two years later produces the same bytes.
//!
//! # An example
//!
//! ```
//! use emob_poi::datex::{InformationStatus, Publisher};
//! use emob_poi::feed::Feed;
//! use emob_poi::rate;
//! use emob_poi::site::*;
//! use emob_core::{EvseId, PartyId};
//! use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
//! use rust_decimal::Decimal;
//!
//! let tariff = Tariff::simple(
//!     "ad-hoc".parse()?,
//!     emob_core::Currency::EUR,
//!     TariffKind::AdHoc,
//!     vec![PriceComponent::new(Dimension::Energy, Decimal::from_str_exact("0.49")?)],
//! );
//! let (published_rate, notes) = rate::publish(&tariff, "rate-1");
//! assert!(notes.is_empty());
//!
//! let point = ChargingPoint::new(
//!     Facility::new("point-1"),
//!     EvseId::parse("DE*ABC*E00001")?,
//!     Connector::new(ConnectorType::Iec62196T2Combo, Decimal::from(150)),
//! );
//! let site = Site::new(
//!     Facility::new("site-1"),
//!     "Musterstadt Nord",
//!     Coordinates {
//!         latitude: Decimal::from_str_exact("50.779599")?,
//!         longitude: Decimal::from_str_exact("6.104507")?,
//!     },
//!     Address::default(),
//!     vec![Station::new(
//!         Facility::new("station-1"),
//!         PartyId::new("DE", "ABC")?,
//!         vec![point],
//!     )],
//! );
//!
//! let feed = Feed {
//!     publisher: Publisher {
//!         country: "DE".to_owned(),
//!         national_identifier: "DE-NAP-Example".to_owned(),
//!         language: "de".to_owned(),
//!     },
//!     information_status: InformationStatus::Test,
//!     table: Facility::new("table-1"),
//!     table_name: Some("Region Nord".to_owned()),
//!     sites: vec![site],
//!     rate_for: &|_| Some(published_rate.clone()),
//! };
//!
//! let json = feed
//!     .table(time::macros::datetime!(2026-04-14 00:00 UTC))?
//!     .to_json()?;
//!
//! // The price in the national access point feed is the tariff's own decimal.
//! assert!(json.contains(r#""value": 0.49"#));
//! // …and 150 kW is published as the watts the profile asks for.
//! assert!(json.contains(r#""maxPowerAtSocket": 150000"#));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::doc_markdown)]

pub mod datex;
pub mod error;
pub mod feed;
pub mod rate;
pub mod site;
pub mod status;

pub use error::{PoiError, Result};
pub use feed::Feed;
pub use rate::{Rate, RateNote};
pub use site::{ChargingPoint, Connector, ConnectorType, Facility, Site, Station};
pub use status::{Lifecycle, PointStatus, Report};
