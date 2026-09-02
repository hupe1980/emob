//! The roaming edge: one canonical record, every wire native, and the cost of
//! the crossing written down.
//!
//! A charge detail record is a claim sent to somebody who was not there and
//! who will pay against it. When that somebody is another company, on another
//! protocol version, the claim has to survive a translation — and translation
//! is where roaming money goes missing, because the losses are decided once,
//! silently, by a `From` impl, and surface six weeks later as two companies
//! holding two different numbers for one session.
//!
//! # What is here
//!
//! - [`partner`] — who this node peers with, and **routing read out of the
//!   contract identifier itself** rather than out of a hand-maintained map.
//! - [`token`] — the token OCPI requires on every CDR and a canonical session
//!   deliberately does not hold, with the check digit that routes the money
//!   verified at the last moment anybody looks at it.
//! - [`crossing`] — the account: every quantity that had to be rounded, every
//!   distinction the wire cannot carry, by JSON Pointer into the document the
//!   partner will be reading.
//! - [`ocpi`] — the wire itself: CDRs, tariffs and the location the national
//!   access point publishes, onto OCPI 2.3.0 and down to 2.2.1, and back.
//!   [`ocpi::preflight`] asks OCPI's own questions of the document that
//!   arrived — before [`ocpi::inbound`] converts it, because every conversion
//!   repairs something and the pre-flight exists to find what would be
//!   repaired.
//!
//! # Three things OCPI cannot say, and one it must not be allowed to
//!
//! **A duration in hours is usually not a decimal.** OCPI carries `total_time`
//! and every period's `TIME` in hours. An hour is 3600 seconds and 3600 has
//! two factors of three, so a twenty-minute session is a third of an hour and
//! has no exact decimal spelling. The money on the record was computed from
//! whole seconds; a partner re-deriving it from the rounded figure gets
//! something else, and [`ocpi::cdr::to_ocpi`] says so by how much. It is the
//! same arithmetic that makes an occupancy fee of €2.50 an hour unlawful under
//! `[AFIR Art. 5(4)]`, met again one layer out.
//!
//! **A charging period has a start and no end.** A reader takes each period as
//! running to the next one's start. A canonical period has both ends, because
//! a station that authorises at 10:00 and first meters at 10:20 must not
//! produce a record claiming twenty minutes of measurement that never
//! happened. Nothing is invented to fill the hole — a zero-energy period would
//! assert that no energy moved, which is exactly what nobody measured — and
//! the uncovered span is reported.
//!
//! **Plug & Charge and `AutoCharge` are one value.** `AUTH_REQUEST` covers both:
//! a contract certificate the vehicle presented, and a MAC address, which is
//! not a standard, not authenticated and trivially spoofable. This workspace
//! keeps them apart precisely because the market conflates them, and the
//! crossing points at the one place the distinction survives — the
//! identification strength read off the signed meter record.
//!
//! **And energy has no direction.** `ENERGY_EXPORT` is *Session Only*
//! `[OCPI 2.3.0 §mod_cdrs_cdrdimensiontype_enum]`, so a CDR has only `ENERGY`
//! and `total_energy` carries no sign. A V2G discharge would arrive at the
//! provider as an ordinary draw and settle backwards — the operator paid for
//! energy the driver supplied. Import and export never net, enforced one layer
//! down against the OBIS code the meter signed, and a translation that quietly
//! re-signed it as import would be that invariant broken at the last possible
//! moment, by us. So it is [`RoamError::ExportNotExpressible`], not a note.
//!
//! # Self-roaming is the same path
//!
//! A session between this operator's own EMP and its own CPO — the German
//! normal case, where one company wears both hats — goes through this module
//! exactly as a partner's does. That is the point: going multi-party later
//! changes the transport and nothing about the arithmetic, and a bug in the
//! crossing is found on your own records rather than on a partner's.
//!
//! # No I/O, no clock
//!
//! Nothing here opens a socket or reads a clock. `last_updated` is an
//! argument, so an export replayed two years into a dispute produces the same
//! bytes it did the first time.
//!
//! # An example
//!
//! ```no_run
//! use emob_roam::{Partner, PartnerRegistry, Reach, RoamingToken, TokenType};
//! # let cdr: emob_cdr::Cdr = unimplemented!();
//! # let context: emob_roam::ocpi::cdr::Context<'_> = unimplemented!();
//!
//! let registry = PartnerRegistry::new("DE*CPO".parse()?)
//!     .with(Partner::emsp("NL*TNM".parse()?).on_signed_data());
//!
//! // Routed by what the contract itself says, not by a map somebody edits.
//! let Some(Reach::Direct(party)) = registry.route(&"NL-TNM-000122045-U".parse()?) else {
//!     unreachable!()
//! };
//! let partner = registry.get(&party).unwrap();
//!
//! let crossing = emob_roam::ocpi::cdr::to_ocpi(&cdr, partner, &context)?;
//! for reason in crossing.reasons() {
//!     eprintln!("the crossing cost: {reason}");
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod crossing;
pub mod error;
pub mod ocpi;
pub mod partner;
pub mod token;

pub use crossing::{Crossing, Note};
pub use error::RoamError;
pub use ocpi::inbound::{Inbound, from_ocpi};
pub use ocpi::preflight::{Finding, Report, Severity, SignedDataPolicy, preflight};
// `Role` is `emob-core`'s: two crates state rules about what a party does on an
// OCPI wire, and two enums for one concept is a conversion table between two
// vocabularies that agree.
pub use emob_core::Role;
pub use partner::{OcpiVersion, Partner, PartnerRegistry, Reach};
pub use token::{RoamingToken, TokenType};
