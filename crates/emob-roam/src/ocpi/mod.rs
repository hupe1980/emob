//! The OCPI wire.
//!
//! One canonical model, translated at the edge, with the crossing's cost
//! written down. The canonical version is **2.3.0** — the richest of the
//! three, and the one `ocpi-kit` defines the others as deltas from — so a
//! partner on 2.2.1 is reached by translating once more, and the second
//! account is folded into the first.

pub mod cdr;
pub mod inbound;
pub mod location;
pub mod tariff;
