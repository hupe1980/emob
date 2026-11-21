//! The **§ 14a wire**: a grid operator's setpoint, lifted off a SPINE datagram
//! as an exact decimal, and the five states LPC gives a controllable site.
//!
//! # Which half of the use case is here
//!
//! `eebus` implements *Limitation of Power Consumption* on both sides, against
//! all 203 of its published abstract test cases, and nothing here re-implements
//! any of it: the datagrams, the bindings, the acknowledgement a write is owed
//! and the `msgCounter` it references are the kit's, and this crate drives its
//! [`Engine`](eebus::spine::Engine).
//!
//! What is here is the seam — the two things the kit leaves to a caller:
//!
//! 1. **The value, exactly.** SPINE carries power as a `ScaledNumber` — an
//!    integer mantissa and a power of ten, which *is* a decimal — and
//!    [`watts_of`] reads that pair. `[BK6-22-300 Ziff. 4.5]`'s Mindestleistung
//!    is 4.2 kW against a three-phase 6 A of 4.14: sixty watts, which is the
//!    distance a rounding eats and a compliance question turns on.
//! 2. **Which state the site is in.** `[LPC §2.3.2]` gives a controllable system
//!    five, three of which allow full power and are three different facts.
//!    `emob_smart::GridLimit` is those five; [`Watchdog`] is the clock that says
//!    which one holds now.
//!
//! # The line this crate draws
//!
//! `csmsd` draws the same one with `ocpp-kit`: that crate's CSMS ledger answers
//! an *operational* question from a numeric register, and **no billed
//! kilowatt-hour is routed through it**. Here the kit answers the
//! *communication* question, and the value that **binds an allocation** comes
//! off the `ScaledNumber` on the datagram as a `Decimal`.
//!
//! Since `eebus` 0.13 the kit holds every regulatory quantity exactly too, so
//! this is no longer about floats. It is about two things the kit's
//! `ControllableSystem` is not built to be: a **pure query** — [`Watchdog`]
//! answers "which state at this instant" from facts, the same answer whether it
//! is asked at 21:00 or two years later in a dispute, where the kit's machine is
//! advanced by timeouts — and a site **nobody has commissioned**, which
//! `emob_smart::GridLimit` calls `Uncontrolled` (D302) and the kit, describing a
//! commissioned device, starts in its failsafe. And [`read_limit_write`] says
//! *why* a write is refused, which is what `sited` reports.

//! # No I/O, no clock
//!
//! A datagram is a value and every instant is an argument; the sockets, the TLS
//! and the mDNS live in `sited`. `eebus` is taken **without `runtime`**, so
//! there is no tokio in this tree and `cargo xtask check-graph` says so.
//!
//! ```
//! use emob_eebus::{Watchdog, watts_of};
//! use eebus::model::ScaledNumber;
//! use rust_decimal::Decimal;
//! use time::macros::datetime;
//!
//! // 42 × 10² watts, off the wire, exactly.
//! assert_eq!(watts_of(&ScaledNumber::new(42, 2)), Some(Decimal::from(4200)));
//!
//! let mut watchdog = Watchdog::new(Decimal::from_str_exact("4.2")?, time::Duration::hours(2));
//! watchdog.heard_from(datetime!(2026-06-01 18:00 +2));
//! // Two hours of silence is the failsafe `[LPC-912]`…
//! assert_eq!(
//!     watchdog.limit_at(datetime!(2026-06-01 20:30 +2)).lpc_state(),
//!     Some("failsafe state")
//! );
//! // …and two more release it `[LPC-022]`, which every summary of § 14a drops.
//! assert_eq!(
//!     watchdog.limit_at(datetime!(2026-06-01 23:00 +2)).lpc_state(),
//!     Some("unlimited/autonomous")
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

pub mod error;
pub mod limit;
pub mod watchdog;

pub use error::EebusError;
pub use limit::{LimitWrite, read_limit_write, watts_of};
pub use watchdog::{FAILSAFE_AFTER, Watchdog};
