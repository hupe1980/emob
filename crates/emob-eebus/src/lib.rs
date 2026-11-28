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
//!    five states. Each control decision uses the last heartbeat, the current
//!    limit's validity and the configured minimum residence in failsafe.
//!
//! A newly started controller holds the configured initialization power until
//! the guard sends a heartbeat and a following limit. A site without a
//! commissioned guard can use `emob_smart::GridLimit::Uncontrolled` directly.
//!
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
//! // Initialization holds the site at failsafe power until the first paired write.
//! assert_eq!(watchdog.limit_at(datetime!(2026-06-01 18:00 +2)).lpc_state(), Some("init"));
//! assert_eq!(watchdog.limit_at(datetime!(2026-06-01 18:00 +2)).stated_kw(), Some(Decimal::from_str_exact("4.2")?));
//!
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

pub mod error;
pub mod limit;
pub mod watchdog;

pub use error::EebusError;
pub use limit::{LimitWrite, read_limit_write, watts_of};
pub use watchdog::{MIN_FAILSAFE_RESIDENCE, Watchdog};
