//! `[EnWG §14a]` as executable law: which charge points a grid operator may switch
//! down, what their operator is owed back, and an allocation that cannot exceed
//! the setpoint.
//!
//! # The rule, in the Festlegung's own order
//!
//! `[EnWG §14a]` lets a grid operator reduce the draw of *steuerbare
//! Verbrauchseinrichtungen* to relieve a local overload, and
//! `[BK6-22-300 Ziff. 2.4.1]` says which devices those are. Three of the four
//! conditions are facts this workspace already holds about a charge point, so
//! [`SteuVe::of`] reads them rather than taking a flag beside them.
//!
//! The first condition is the one that decides whether any of this applies at
//! all, and it is the one every summary drops:
//!
//! > ein Ladepunkt für Elektromobile, **der kein öffentlich zugänglicher
//! > Ladepunkt im Sinne des § 2 Nr. 5 der Ladesäulenverordnung (LSV) ist**
//!
//! A publicly accessible point is **not** a steuerbare Verbrauchseinrichtung.
//! The fast charger on the forecourt is outside § 14a entirely; the depot, the
//! workplace car park and the fleet yard are inside it.
//!
//! # What the operator gets back
//!
//! `[BK6-22-300 Ziff. 4.5]` gives the operator an *Anspruch* on a minimum draw
//! whatever the grid operator asks for — 4.2 kW per directly controlled device,
//! or `4.2 kW + (n − 1) × GZF × 4.2 kW` for the ones on an energy-management
//! system, where `n` counts **only** those. [`minimum_for`] is that arithmetic,
//! and [`allocate`] holds a site at it rather than obeying a setpoint below it,
//! because a claim given up silently is one nobody finds out about.
//!
//! # And silence is not permission
//!
//! § 14a is carried over EEBUS's *Limitation of Power Consumption* use case, and
//! `[LPC §2.3.2]` gives a controllable site **five** states where a summary
//! gives it two. Three allow full power and they are not one fact: the grid
//! operator is there and allowing it, the grid operator has been gone for hours
//! `[LPC-922]`, or no control box was ever commissioned. So [`GridLimit`] is
//! five values rather than an `Option`, and two of them put a note on the
//! allocation — a depot at 60 kW because it may and one at 60 kW because its
//! control box died are the same number and different problems.
//!
//! The fourth is the failsafe: after a restart, and 120 seconds after the last
//! heartbeat, a site is limited to its Failsafe Consumption Active Power Limit
//! `[LPC-901/1]`, `[LPC-912]`. The timers that decide when belong beside the
//! socket that notices the silence; what arrives here is the state they reached.
//!
//! # And the ceiling is a door rather than a check
//!
//! `[BK6-22-300 Ziff. 4.6]` makes the grid operator's reduction take precedence
//! over *"marktliche Laststeuerung"* — a dynamic tariff, a departure schedule, a
//! driver in a hurry. Every one of those is a reason somebody writes code that
//! reaches past the limit, so [`Allocation`] has no constructor and no public
//! fields, [`allocate`] is the only thing that returns one, and a market plan is
//! a [`Demand`] going in.
//!
//! ```
//! use emob_smart::{Control, Demand, GridLimit, Site, SteuVe, allocate};
//! use emob_core::station::{Accessibility, ChargePointProfile};
//! use rust_decimal::Decimal;
//! use time::macros::{date, datetime};
//!
//! # fn depot(id: &str) -> ChargePointProfile {
//! #     let mut point = ChargePointProfile::bare(id.parse().unwrap(), date!(2026 - 01 - 01));
//! #     point.accessibility = Accessibility::Private;
//! #     point.grid.capacity_kw = Decimal::from(11);
//! #     point
//! # }
//! // Two 11 kW boxes in a yard, on one energy-management system.
//! let devices: Vec<SteuVe> = ["DE*ABC*E00001", "DE*ABC*E00002"]
//!     .iter()
//!     .map(|id| {
//!         SteuVe::of(&depot(id), Control::Ems)
//!             .expect("a private 11 kW point in low voltage is a SteuVe")
//!     })
//!     .collect();
//! let site = Site { connection_kw: Decimal::from(22), devices: devices.clone() };
//!
//! // 4.2 + (2 − 1) × 0.8 × 4.2
//! assert_eq!(site.minimum().kw, Decimal::try_from(7.56)?);
//!
//! // The market wants both boxes flat out; the grid operator has said 8 kW.
//! let plan: Vec<Demand> = devices
//!     .iter()
//!     .map(|device| Demand { evse_id: device.evse_id().clone(), wanted_kw: Decimal::from(11) })
//!     .collect();
//! let allocation = allocate(
//!     &site,
//!     &plan,
//!     &GridLimit::curtailed(Decimal::from(8), datetime!(2026-06-01 18:00 +2)),
//! );
//!
//! assert_eq!(allocation.total_kw(), Decimal::from(8));
//! assert!(allocation.within_ceiling());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What is not here
//!
//! **The wire, and the timers on it.** A setpoint arrives over EEBUS SHIP/SPINE
//! and an allocation leaves as an OCPP charging profile; the heartbeats that
//! decide when a site has gone quiet are the protocol engine's to count. None of
//! it is this crate's: the rule lives where a test can run it and the socket
//! lives in a daemon, the same seam `emob-ocpp` sits on for metering. This crate
//! reads no clock and opens nothing.
//!
//! **The other three device families.** `[BK6-22-300 Ziff. 2.4.1]` also names
//! heat pumps, space cooling and storage, and the Mindestleistung formula has a
//! limb for a heat pump over 11 kW on the same energy-management system. A site
//! with one is owed more than [`minimum_for`] returns, which is the direction an
//! under-claim falls in. Household devices are `hems`'s.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]

pub mod site;
pub mod steuve;

pub use site::{
    Allocation, Curtailment, Demand, Failsafe, GridLimit, Site, SmartNote, allocate,
    is_one_ems_group,
};
pub use steuve::{
    Control, Minimum, MinimumRule, SteuVe, THRESHOLD_KW, minimum_for, simultaneity_factor,
};

// The conditions are `emob-core`'s, beside the profile the obligation calendar
// judges, so a duty and a load manager cannot disagree about whether
// `[EnWG §14a]` binds a point. Re-exported so a reader of this crate does not
// have to discover which one holds them.
pub use emob_core::station::{GridConnection, GridLevel, NotSteuVe};
