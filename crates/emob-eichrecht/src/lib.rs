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
//! # The four questions, kept apart
//!
//! Conflating these is how a "verified" session turns out to be a signed
//! fragment of a session somebody edited:
//!
//! | Question | Answered by |
//! |---|---|
//! | Did *this key* produce *these bytes*? | [`ocmf::verify()`] |
//! | Is this key *this charge point's* key? | [`registry::KeyRegistry`] |
//! | Are any records missing from the session? | [`chain::validate()`] |
//! | May these readings be billed at all? | [`chain::validate()`], via [`ocmf::MeterState`] |
//! | May the *duration* be billed too? | [`chain::validate()`], via [`ocmf::TimeStatus`] |
//! | Who was charging, and did the assignment hold? | [`chain::validate()`], via [`ocmf::IdentificationLevel`] |
//! | Can the **customer** repeat all of that? | [`mod@transparency`] |
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
//! use emob_eichrecht::{Evidence, KeyRegistry, ocmf};
//! # let raw_records: Vec<String> = vec![];
//! # let registry = KeyRegistry::new();
//! # let session_start = time::OffsetDateTime::UNIX_EPOCH;
//!
//! let records = raw_records.iter().map(|r| ocmf::parse(r)).collect::<Result<Vec<_>, _>>()?;
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
//! What is implemented is what OCMF 1.4 defines, plus the chain rules the
//! specification assigns to a "check component". Two curves from
//! `[OCMF Tab. 22]` — the brainpool pair — are recognised and refused with a
//! named error rather than silently failing: no audited pure-Rust
//! implementation exists, and a wrong answer here is worse than no answer.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod chain;
pub mod error;
pub mod evidence;
pub mod ocmf;
pub mod registry;
pub mod transparency;

pub use chain::{ChainFinding, ChainReport};
pub use error::{OcmfError, VerifyError};
pub use evidence::{Evidence, EvidenceProblem, VerifiedRecord};
pub use ocmf::{KeyType, OcmfRecord, PublicKey};
pub use registry::{ComponentRef, KeyRegistry, RegisteredKey};
pub use transparency::{TransparencyError, TransparencyValue};
