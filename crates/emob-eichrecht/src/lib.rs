//! German calibration law (Eichrecht) for EV charging: signed meter values,
//! verified end to end, from the station's signature to the invoice line.
//!
//! # The rule this crate exists to enforce
//!
//! A customer may be billed for a measured value only if they can verify it,
//! long after the session `[MessEG §33]`, `[PTB-A 50.7]`, `[REA 6-A]`. Every
//! serious charging platform treats this as a checkbox; every open-source CSMS
//! ignores it. Here it is the invariant everything else hangs from:
//!
//! > **A value that does not verify does not bill.**
//!
//! And it is a property of the types, not a convention: the only way to obtain
//! a billable quantity is [`Evidence::billable_energy`], which returns `None`
//! whenever anything at all is wrong, and [`Evidence`] can only be built by
//! running the whole check.
//!
//! # The questions, kept apart — and the two crates that answer them
//!
//! Conflating these is how a "verified" session turns out to be a signed
//! fragment of a session somebody edited:
//!
//! | Question | Answered by | Whose |
//! |---|---|---|
//! | Did *this key* produce *these bytes*? | [`ocmf::verify()`] | the format's |
//! | Are any records missing from the session? | [`ocmf::session`] | the format's |
//! | Is this key *this charge point's* key? | [`registry::KeyRegistry`] | **ours** |
//! | Which quantity does each failure take away? | [`chain::validate()`] | **ours** |
//! | May the *energy* be billed? | [`Evidence::billable_energy`] | **ours** |
//! | May the *duration* be billed too? | [`Evidence::billable_duration`] | **ours** |
//! | Can the **customer** repeat all of that? | [`mod@transparency`] | both |
//!
//! The split is not arbitrary. Reading and verifying an OCMF record is
//! [`ocmf`]'s job, and it does it against the whole S.A.F.E. reference corpus
//! with OpenSSL as an independent oracle — evidence this crate never had. What
//! it will not do is decide money, and it says so: *"whether a session may be
//! invoiced depends on tariffs, on a key registry binding each record to this
//! charge point, and on law — none of which is in scope."*
//!
//! That sentence is this crate. A format crate can say a record is missing; only
//! law can say what a missing record costs you, and the answer is not one
//! boolean (D184).
//!
//! The last one is the one the law actually asks for. `[MessEG §33]` does not
//! require a measured value to be correct, it requires the affected party to be
//! able to **check** it — so a platform that verifies internally and reports
//! "verified" has satisfied nobody. [`transparency::to_xml`] emits the
//! container the S.A.F.E. Transparenzsoftware reads, holding each record
//! verbatim beside the key it was checked against.
//!
//! [`transparency::from_xml`] reads one back, because the export is only half
//! of the duty: the other half arrives when a driver disputes a bill and sends
//! the file back, and an operator has to check its records against **its own**
//! registry and say whether the key inside it is the key the station was
//! provisioned with.
//!
//! # A whole session
//!
//! ```
//! use emob_eichrecht::{Evidence, KeyRegistry};
//! # let raw_records: Vec<String> = vec![];
//! # let registry = KeyRegistry::new();
//! # let session_start = time::OffsetDateTime::UNIX_EPOCH;
//!
//! // The texts outlive the records: a `Record` borrows the bytes its signature
//! // covers, which is the format's central rule rather than an inconvenience.
//! let records = raw_records
//!     .iter()
//!     .map(|r| ocmf::Record::parse(r))
//!     .collect::<Result<Vec<_>, _>>()?;
//! let evidence = Evidence::assemble(&records, &registry, session_start);
//!
//! match evidence.billable_energy() {
//!     Some(energy) => println!("bill {energy}"),
//!     None => for reason in evidence.reasons() {
//!         eprintln!("blocked: {reason}");  // and the session goes to an operator
//!     },
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # No I/O, no clock
//!
//! Nothing here opens a socket, reads a file or asks the time. The key registry
//! is handed in already populated and every instant is an argument, so a whole
//! fleet's verification runs as a deterministic unit test — and a dispute from
//! two years ago is replayed exactly as it happened.
//!
//! # Scope
//!
//! The format is `ocmf`'s in full — every table, every deviation real meters
//! make, and four of the seven algorithms of `[OCMF Tab. 22]` in pure Rust. The
//! remaining three (the brainpool pair and secp192k1) have no audited pure-Rust
//! arithmetic and `ocmf` reaches them through OpenSSL, which a crate that
//! promises to open no socket and read no file may not link — so here they are
//! recognised and refused by name rather than silently failing.
//!
//! What is this crate's is everything downstream of "the bytes check out": whose
//! key it was, which quantity each failure takes away, and the file the customer
//! repeats the check with.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod chain;
pub mod error;
pub mod evidence;
pub mod registry;
pub mod transparency;

pub use chain::{ChainFinding, ChainReport, Disqualifies, SignedMarker};
pub use error::{EichrechtError, KeyLookupError};
pub use evidence::{Evidence, EvidenceProblem, VerifiedRecord};
// The format itself is `ocmf`'s. Re-exported so a caller reading this crate's
// API does not have to discover which crate a `Record` came from, and so that
// the version of `ocmf` a record was parsed with is the version this crate
// validates against — the two cannot drift when there is only one.
pub use ocmf::{
    Curve, ObisCode, ParseError, Profile, PublicKey, Record, SignatureAlgorithm, VerifyError,
};
pub use registry::{ComponentRef, KeyRegistry, RegisteredKey, RegistryError};
pub use transparency::TransparencyError;
