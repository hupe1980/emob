//! The check a driver runs: one signed record, one key, one verdict.
//!
//! ```
//! use emob_verify::{Verdict, check};
//!
//! // What is not a record is named as such, never passed or failed.
//! let result = check("not an OCMF record", "04ab");
//! assert_eq!(result.verdict, Verdict::UnreadableRecord);
//! assert!(result.readings.is_empty());
//! ```
//!
//! `[MessEG §33]` asks that the affected party can **check** a measured value.
//! The receipt page hands the driver the records and the key; this crate is
//! what checks them, on the driver's own device, with nothing trusted on the
//! server's side. It is [`ocmf::verify()`] and nothing more — the same function
//! `emob-eichrecht` runs before a session may bill — plus the words a person
//! reads.
//!
//! # One function, two shells
//!
//! [`check`] is the whole of it and is tested natively against the S.A.F.E.
//! reference corpus. Built for `wasm32-unknown-unknown` the crate also exports a
//! C ABI (`buffer`, `verify`, `output`, `output_len`) that the receipt page's
//! forty lines of script call — no `wasm-bindgen`, no npm, no imports. The
//! `emob verify` CLI calls [`check`] directly. Neither shell decides anything.
//!
//! # What a check cannot say
//!
//! Three of the seven algorithms of `[OCMF Tab. 22]` (brainpoolP256r1,
//! brainpoolP384r1, secp192k1) have no audited pure-Rust arithmetic (R18). A
//! record signed with one is [`Verdict::Unavailable`]: neither verified nor
//! failed, and the driver is sent to the Transparenzsoftware.

use ocmf::payload::TransactionMarker;
use ocmf::{PublicKey, Record, VerifyError};

/// What a check says about one signed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The key produced these bytes.
    Verified,
    /// The signature does not match the bytes — the value was changed, or the
    /// key is not the one that signed it.
    NotVerified,
    /// The record is signed on a curve this build cannot check (R18).
    Unavailable,
    /// The record is not an OCMF record this crate can read.
    UnreadableRecord,
    /// The key is not a public key this crate can read.
    UnreadableKey,
    /// The key lives on another curve than the record's algorithm names.
    KeyMismatch,
}

impl Verdict {
    /// The code the WebAssembly `verify` export returns.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Verified => 0,
            Self::NotVerified => 1,
            Self::Unavailable => 2,
            Self::UnreadableRecord => 3,
            Self::UnreadableKey => 4,
            Self::KeyMismatch => 5,
        }
    }

    /// Whether the value stands.
    #[must_use]
    pub const fn holds(self) -> bool {
        matches!(self, Self::Verified)
    }

    /// One sentence, in English, for the CLI.
    #[must_use]
    pub const fn sentence(self) -> &'static str {
        match self {
            Self::Verified => "verified: this key signed exactly these bytes",
            Self::NotVerified => "NOT verified: the signature does not match the value",
            Self::Unavailable => {
                "not checkable here: the curve needs the S.A.F.E. Transparenzsoftware (R18)"
            }
            Self::UnreadableRecord => "unreadable: not an OCMF record",
            Self::UnreadableKey => "unreadable key",
            Self::KeyMismatch => "the key is on another curve than the record's algorithm",
        }
    }
}

/// One reading inside a record, as the signed bytes state it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The transaction marker, `[OCMF Tab. 7, TX]` (`B`, `E`, …), or `-`.
    pub marker: char,
    /// The register value, verbatim (`RV`).
    pub value: String,
    /// The unit, verbatim (`RU`).
    pub unit: String,
    /// The time, verbatim (`TM`).
    pub time: String,
}

/// A verdict and what the record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The verdict.
    pub verdict: Verdict,
    /// The readings, decoded from the bytes that were checked — so a device
    /// shows the value it verified, not the server's rendering of it.
    pub readings: Vec<Reading>,
}

impl Check {
    /// The readings as `TX\tRV\tRU\tTM` lines — what the WebAssembly export
    /// writes to its output.
    #[must_use]
    pub fn summary(&self) -> String {
        self.readings
            .iter()
            .map(|r| format!("{}\t{}\t{}\t{}", r.marker, r.value, r.unit, r.time))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Check one record against one key.
///
/// `key` is text in any shape `[OCMF]` stations and registries use — SEC1 or
/// `SubjectPublicKeyInfo`, hex or Base64; the record's own algorithm names the
/// curve a bare point lives on.
#[must_use]
pub fn check(record: &str, key: &str) -> Check {
    let Ok(parsed) = Record::parse(record) else {
        return Check {
            verdict: Verdict::UnreadableRecord,
            readings: Vec::new(),
        };
    };
    let readings = parsed.payload().readings().iter().map(reading).collect();
    let verdict = match PublicKey::from_text(key.trim(), parsed.signature().curve()) {
        Err(_) => Verdict::UnreadableKey,
        Ok(key) => match ocmf::verify(&parsed, &key) {
            Ok(_) => Verdict::Verified,
            Err(error) => verdict_of(&error),
        },
    };
    Check { verdict, readings }
}

/// "Authentic" is `Ok`; every other outcome is "not authentic, and here is
/// why" — except `Unsupported`, which means this build cannot tell.
const fn verdict_of(error: &VerifyError) -> Verdict {
    match error {
        VerifyError::Unsupported { .. } => Verdict::Unavailable,
        VerifyError::AlgorithmKeyMismatch { .. } => Verdict::KeyMismatch,
        VerifyError::Key(_) | VerifyError::KeyNotOnCurve { .. } => Verdict::UnreadableKey,
        _ => Verdict::NotVerified,
    }
}

fn reading(r: &ocmf::payload::Reading<'_>) -> Reading {
    Reading {
        marker: r.transaction().map_or('-', marker),
        value: r.value().map(ToString::to_string).unwrap_or_default(),
        unit: r.unit().map(|u| u.as_str().to_owned()).unwrap_or_default(),
        time: r.time().map(|t| t.to_string()).unwrap_or_default(),
    }
}

const fn marker(m: TransactionMarker) -> char {
    match m {
        TransactionMarker::Begin => 'B',
        TransactionMarker::Charging => 'C',
        TransactionMarker::Exception => 'X',
        TransactionMarker::End => 'E',
        TransactionMarker::EndedLocally => 'L',
        TransactionMarker::EndedRemotely => 'R',
        TransactionMarker::Aborted => 'A',
        TransactionMarker::PowerFailure => 'P',
        TransactionMarker::Suspended => 'S',
        TransactionMarker::TariffChange => 'T',
        TransactionMarker::Undefined(c) => c,
        _ => '?',
    }
}

/// The C ABI the receipt page calls. One input buffer, one output buffer, no
/// allocator exported and no import needed: the module is a pure function of
/// the bytes the page writes into it.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)] // `no_mangle` is an unsafe attribute in edition 2024; nothing else is.
mod abi {
    use core::cell::RefCell;

    thread_local! {
        static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }

    /// A pointer to an input buffer of at least `len` bytes.
    #[unsafe(no_mangle)]
    pub extern "C" fn buffer(len: usize) -> *mut u8 {
        INPUT.with(|input| {
            let mut input = input.borrow_mut();
            input.clear();
            input.resize(len, 0);
            input.as_mut_ptr()
        })
    }

    /// Check the record (the first `record_len` bytes of the buffer) against
    /// the key (the next `key_len`); the code of [`crate::Verdict`].
    #[unsafe(no_mangle)]
    pub extern "C" fn verify(record_len: usize, key_len: usize) -> i32 {
        let check = INPUT.with(|input| {
            let input = input.borrow();
            let (Some(record), Some(key)) = (
                input.get(..record_len),
                input.get(record_len..record_len.saturating_add(key_len)),
            ) else {
                return None;
            };
            let (Ok(record), Ok(key)) = (core::str::from_utf8(record), core::str::from_utf8(key))
            else {
                return None;
            };
            Some(crate::check(record, key))
        });
        let check = check.unwrap_or(crate::Check {
            verdict: crate::Verdict::UnreadableRecord,
            readings: Vec::new(),
        });
        OUTPUT.with(|output| *output.borrow_mut() = check.summary().into_bytes());
        check.verdict.code()
    }

    /// The summary of the last [`verify`].
    #[unsafe(no_mangle)]
    pub extern "C" fn output() -> *const u8 {
        OUTPUT.with(|output| output.borrow().as_ptr())
    }

    /// Its length.
    #[unsafe(no_mangle)]
    pub extern "C" fn output_len() -> usize {
        OUTPUT.with(|output| output.borrow().len())
    }
}
