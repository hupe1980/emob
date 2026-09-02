//! The shell every emob daemon shares — and the one thing in it that is about
//! charging.
//!
//! # What a daemon gets, and what it brings
//!
//! Nine daemons is nine copies of the same forty lines: read a configuration
//! file, let the environment override it, start structured logging, bind a
//! socket, answer a health probe, and stop when the orchestrator says so.
//! Written nine times, those forty lines diverge — and they diverge in the
//! direction that costs most, because the one that is wrong is the one whose
//! readiness probe lies.
//!
//! So [`config`], [`telemetry`], [`health`], [`serve`] and [`shutdown`] are
//! here, and they know nothing about electricity.
//!
//! # …and three modules that do
//!
//! | Module | Why it is emob's rather than a copy |
//! |---|---|
//! | [`authority`] | a principal is an **OCPI party**, and the worst thing a roaming node can do is serve one party's records to another |
//! | [`events`] | the `CloudEvents` catalogue this workspace emits, as constants a subscription is checked against at compile time |
//! | [`webhook`] | one signer, so a receiver implements one verifier |
//!
//! # Why this is not `mako-service`
//!
//! Extracting it was considered and rejected, for the reason `hems-service`
//! gives and one more of our own. `mako`'s authorisation is built on
//! **Marktrollen** — LF, NB, MSB — and its OIDC layer carries a `Sparte` grant;
//! its Cedar schema is those roles. emob's principals are OCPI parties with an
//! OCPI role, scoped by the party that owns each record, and the check that
//! matters is one `mako` has no reason to have: *may this credential reach a
//! record this other company owns*.
//!
//! What was left after removing the market model was five domain-free modules,
//! and copying five domain-free modules is cheaper than maintaining a diff guard
//! against a fork that is *supposed* to diverge.
//!
//! # Sans-I/O ends here
//!
//! Every domain crate in this workspace takes its instants as parameters and
//! opens no socket, so a dispute about a session from two years ago is answered
//! by replaying the check exactly as it ran. This crate is where that stops
//! being true, and it is the **only** shared place it does. `just purity` fails
//! the build if a domain crate reaches for a clock; this crate is not in that
//! list, and everything above it is a daemon.
//!
//! Two things stay pure even here, and deliberately: [`webhook::sign`] takes the
//! instant it signs, so a delivery replayed from an outbox is byte-identical to
//! the first attempt; and [`authority`] reads no clock at all, so a credential's
//! reach is a property of the credential rather than of when it was asked.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::module_name_repetitions,
    reason = "`ServerError` in `serve` is the name the type has at every call site"
)]

pub mod authority;
pub mod config;
pub mod events;
pub mod health;
pub mod serve;
pub mod shutdown;
pub mod telemetry;
pub mod webhook;

// `Role` is `emob-core`'s — see its documentation for why one concept has one
// type here.
pub use authority::{Capabilities, PartyScope, Principal, Token, caps};
pub use config::{ConfigError, Secret, load, load_from};
pub use emob_core::Role;
pub use health::{Identity, Probe, Readiness};
pub use serve::{Server, ServerError};
pub use shutdown::Shutdown;
pub use webhook::{Delivery, SecretError};
