//! The OCPP seam: from a charging station's transaction events to a session
//! this workspace will bill.
//!
//! # The rule this crate exists to make structural
//!
//! OCPP carries two kinds of meter value, and only one of them is money.
//!
//! The **numeric** ones — `meterStart`, `meterStop`, `SampledValue.value` — are
//! operational telemetry. They answer whether every event arrived and whether
//! the sequence is complete, and they are floating point by the time any ledger
//! holds them: `ocpp-kit`'s CSMS ledger carries `meter_wh: Option<f64>`, and
//! even the type OCPP 2.1 calls `Decimal` is a newtype over `f64`. That is the
//! right shape for telemetry and the wrong shape for an invoice, because a
//! binary float cannot represent `0.10` and `2935.600` and `2935.6` stop being
//! different claims about resolution.
//!
//! The **signed** one is a `SignedMeterValueType` carrying an OCMF data set,
//! and it is the only thing here that becomes a billed kilowatt-hour.
//!
//! This crate makes that a property of the types rather than a rule somebody
//! remembers: **its input vocabulary has no numeric meter value in it at all.**
//! [`TransactionEvent`] carries signed values, instants and whether energy was
//! flowing. There is no field to put a float in, so there is no path from one
//! to a `Cdr` — and `cargo xtask no-floats` fails the build if an `f32` or an
//! `f64` appears anywhere in the workspace outside a test.
//!
//! The OCA's own example message is what makes the point concrete: its
//! `meterStop` is `108814`, the meter's **lifetime** register in watt-hours,
//! while the transaction's signed difference is `0.636 kWh`. A CSMS billing the
//! protocol's numbers would bill a number nothing signed, taken from a register
//! that is not the session's `[OCA SMV §5.2]`.
//!
//! # The seam runs both ways
//!
//! The money comes **in** across this seam, out of a signature. The **price**
//! goes out across it: OCPP 2.1's *Tariff and Cost* block lets a CSMS install a
//! structured tariff on an EVSE and requires the station to show its
//! description to the driver — which is where `[AFIR Art. 5(4)]`'s "known to
//! end users before they initiate" actually happens. [`tariff`] carries an
//! [`emob_tariff::Tariff`] onto that object, so the price on the charge point's
//! screen is the object that rates the CDR rather than a field somebody typed.
//!
//! That is why this crate depends on `emob-tariff` and the crates that decide
//! money still do not depend on `ocpp-kit`: the seam is the only place both
//! vocabularies are in scope, in either direction.
//!
//! # What it does not do
//!
//! It does not speak OCPP. `ocpp-kit` does that — the framing, the sans-I/O
//! engine, the transports, the security profiles, and the version-neutral
//! [`DomainEvent`] this crate reads. [`kit`] is a `match` over that event and
//! nothing more.
//!
//! It also does not verify. [`Transaction::assemble`] gets the bytes out of the
//! transport intact; [`emob_eichrecht::Evidence::assemble`] decides whether
//! they hold up, against a **registry** and never against the key the station
//! sent beside them.
//!
//! [`DomainEvent`]: ocpp_kit::csms::events::DomainEvent
//!
//! # Why this is a crate and not a module
//!
//! It holds one job, and it is a boundary rather than a quarantine. The protocol
//! knowledge that once lived here — `[OCA SMV §3.2.1]`'s 1.6 nesting,
//! `[OCA SMV §3.2.2]`'s `publicKey` envelope, lifting a signed value out of three
//! generations of typed message — belongs to `ocpp-kit` and lives there.
//!
//! Folding what is left into `emob-cdr` would put **`ocpp-kit` in the dependency
//! graph of every crate that decides money**. `emob-core`, `emob-session`,
//! `emob-eichrecht`, `emob-tariff` and `emob-cdr` build with no OCPP anywhere in
//! their tree, and the seam is the reason: it is the only crate on both sides.
//!
//! The billing chain should be buildable, testable and auditable without a
//! protocol implementation in it, and a boundary the compiler enforces is the
//! only kind that stays true.
//!
//! # A whole transaction
//!
//! ```
//! use emob_ocpp::{SignedMeterValue, SignedReading, Transaction, TransactionEvent};
//! use emob_session::{Authorization, EndReason};
//! use emob_core::Direction;
//! # let raw_ocmf = emob_ocpp::fixtures::OCA_OCMF;
//! # let started = time::macros::datetime!(2023-05-19 15:52:39 +2);
//! # let ended = time::macros::datetime!(2023-05-19 15:53:58 +2);
//!
//! let transaction = Transaction::new(
//!     "t-96".parse()?,
//!     "DE*SIM*E00001".parse()?,
//!     Authorization::ad_hoc(),
//! )
//! .with(TransactionEvent::started(started, vec![]))
//! .with(TransactionEvent::ended(
//!     ended,
//!     vec![SignedReading::new(
//!         SignedMeterValue::new(raw_ocmf),
//!         Some("Transaction.End".to_owned()),
//!     )],
//!     EndReason::Local,
//! ));
//!
//! let assembled = transaction.assemble(Direction::Import)?;
//! assert_eq!(
//!     assembled.session.total(Direction::Import).unwrap().to_string(),
//!     "0.636 kWh",
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::doc_markdown)]

pub mod error;
pub mod fixtures;
pub mod kit;
pub mod tariff;
pub mod transaction;

pub use error::SeamError;
pub use tariff::{cost_updated, to_ocpp};
pub use transaction::{
    Assembled, EventKind, SignedReading, Transaction, TransactionEvent, record_of,
};

/// The signed meter value, as `ocpp-kit` delivers it.
///
/// Re-exported rather than redefined: getting a `SignedMeterValueType` out of
/// three generations of typed message, out of Base64 or plain, and out of the
/// `publicKey` envelope is protocol knowledge, and it lives in the protocol
/// crate now.
pub use ocpp_kit::metering::SignedMeterValue;
