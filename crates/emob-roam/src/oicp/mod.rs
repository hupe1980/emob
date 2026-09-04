//! The OICP wire: Hubject's hub-and-spoke roaming, and what a session loses on
//! the way onto it.
//!
//! # Hub-and-spoke, not peer-to-peer
//!
//! OCPI peers talk to each other. OICP partners talk only to **Hubject**, over
//! mutual TLS, and Hubject calls back for the reverse direction. That changes
//! the transport and almost nothing about the translation, which is why this
//! module sits beside [`crate::ocpi`] rather than under an abstraction over
//! both: the canonical record is the same record, and each wire is written
//! natively.
//!
//! # The one difference that is not transport
//!
//! **An OICP charge detail record carries no money.** There is no `total_cost`,
//! no price, no per-dimension breakdown — the fields are the session's
//! timestamps, the register readings, `ConsumedEnergy`, the identification, the
//! signed meter values, and a `PartnerProductID`. The amount is settled out of
//! band, against a *pricing product* the two parties agreed on beforehand
//! `[OICP 2.3 §PricingProductData]`.
//!
//! So the crossing cannot lose a rounding on the total, because the total does
//! not cross. What it loses instead is bigger and is the first thing
//! [`cdr::to_oicp`] says: the receiving provider re-derives the price from the
//! product, and a product it holds a different version of is a settlement that
//! disagrees with nothing to compare against. That is a note on every record,
//! not an exception — it is how the protocol works.
//!
//! # …and one place it gains
//!
//! OICP has **four** timestamps where OCPI has two: `SessionStart`/`SessionEnd`
//! and `ChargingStart`/`ChargingEnd`. The span an OCPI reader silently
//! attributes to a measured period — the thirty seconds a car sits connected
//! before its charge begins — is expressible here, and the crossing states it
//! rather than leaving it to be inferred. A translation is not only a loss.
//!
//! # What is here
//!
//! - [`cdr`] — the canonical CDR onto OICP's `ChargeDetailRecord`, and back.
//! - [`pricing`] — the canonical tariff onto a `PricingProductDataRecord`, which
//!   is the only place a price crosses this wire at all.

pub mod cdr;
pub mod pricing;
