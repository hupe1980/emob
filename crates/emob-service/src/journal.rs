//! The state a service holds, and how it outlives the process.
//!
//! A daemon does not persist its aggregate. It appends the facts it decided, in
//! order, and rebuilds the aggregate by folding them on the way up.
//!
//! # Why a fold is sound here
//!
//! The domain crates take every instant and every key as an argument — the rule
//! `just purity` and `cargo xtask check-graph` enforce, so that a dispute about
//! a session from two years ago is answered by replaying the verification
//! exactly as it ran. A fold over functions whose inputs are all stated gives
//! the same answer the second time, for the same reason.
//!
//! # Facts, not commands
//!
//! An entry says what was **decided**, not what was asked. A command log
//! re-runs the decision at replay, which means re-reading everything it read —
//! the tariff, the clock, the partner registry — and a platform that rebuilt
//! its invoice numbers by re-deciding them is one whose books change when a
//! tariff is corrected.
//!
//! So an event carries everything the fold needs and nothing it has to look up:
//! where one names a document, the document is in it.
//!
//! # A file, chained, rather than a table
//!
//! Each record carries the digest of the one before it, the construction
//! `emob-eichrecht` reads on a meter's chain. What a service needs of its state
//! is append, replay in order, and a proof that nothing was rewritten — none of
//! which is a query, and all of which a table makes harder to show. A
//! newline-delimited file is checkable with `sha256sum` by somebody who does not
//! trust the platform.
//!
//! # What the chain does not prove
//!
//! It proves nothing was rewritten and nothing was removed from the **middle**.
//! It cannot prove nothing was removed from the **end**, and no chain can: a
//! journal with its last record cut off is byte-for-byte one that was never
//! written, which is the file a process killed mid-append leaves — and
//! [`File::records`] has to tolerate that one.
//!
//! So the tail is closed twice. [`crate::head::Signing`] signs [`Journal::head`]
//! after every append and keeps it beside the journal, and refuses to open a
//! journal shorter than what it signed; [`crate::witness`] keeps the heads it
//! was shown somewhere this process cannot reach, and a head that has gone
//! **backwards** between two observations is a lost tail — the case where the
//! signed head was removed with it. Saying so is better than a `verify()` that
//! returns `true` for the one case it cannot see.
//!
//! # Who decided it
//!
//! Every record names its [`Actor`], inside the digest. `[NIS2 Art. 21(2)]`
//! asks for incident handling, and a disputed block or a cancelled invoice that
//! cannot be attributed is an incident nobody can write up. The actor is set by
//! the daemon from whoever **authenticated** — never from what a request says
//! about itself — and it is readable without the data key, so attribution
//! survives a key destroyed on purpose.
//!
//! # Wiring one
//!
//! ```
//! use emob_service::journal::{Folded, Journal as _, Kinded, Memory};
//!
//! #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
//! enum Fact {
//!     Counted(u32),
//! }
//!
//! impl Kinded for Fact {
//!     fn kind(&self) -> &'static str {
//!         "counted"
//!     }
//! }
//!
//! fn apply(total: &mut u64, fact: Fact) {
//!     let Fact::Counted(n) = fact;
//!     *total += u64::from(n);
//! }
//!
//! // On the way up, the state is what the journal says it is. A `File` here is
//! // the same call with the same semantics and a disk behind it.
//! let mut folded = Folded::open(0_u64, Memory::new(), apply)?;
//!
//! // A decision, then the record of it — and the caller is told last. Every
//! // fact names who caused it; one without an author does not compile.
//! let by = emob_service::journal::Actor::Timer;
//! folded.record(&by, Fact::Counted(7))?;
//! folded.record(&by, Fact::Counted(5))?;
//! assert_eq!(*folded.state(), 12);
//!
//! // …and the chain over what it decided.
//! assert_eq!(folded.journal().records()?.len(), 2);
//! assert_eq!(folded.journal().head()?.position, 2);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Serialize, de::DeserializeOwned};

pub use crate::actor::{Actor, ActorError};
use sha2::{Digest as _, Sha256};

/// The digest a first record chains from: thirty-two zero bytes, as hex.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Where a journal ends: the last record's position and digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The last record's `seq`; `0` for an empty journal.
    pub position: u64,
    /// The last record's digest, or [`GENESIS`].
    pub hash: String,
}

impl Head {
    /// The head of an empty journal.
    #[must_use]
    pub fn genesis() -> Self {
        Self {
            position: 0,
            hash: GENESIS.to_owned(),
        }
    }
}

/// What went wrong reaching the journal.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// The file could not be opened, appended to or read.
    #[error("the journal at {path} could not be {doing}: {source}")]
    Io {
        /// Which file.
        path: PathBuf,
        /// What was being attempted, as a verb phrase.
        doing: &'static str,
        /// The underlying failure.
        source: std::io::Error,
    },

    /// An event could not be written as, or read back from, JSON.
    #[error("a journal entry could not be {doing}: {source}")]
    Encoding {
        /// What was being attempted.
        doing: &'static str,
        /// The underlying failure.
        source: serde_json::Error,
    },

    /// A record's digest does not follow from the record before it.
    ///
    /// The journal was edited, truncated in the middle, or concatenated with
    /// another. Refusing to start is the only safe answer: a service that
    /// replayed past this would rebuild a state nobody can account for.
    #[error(
        "journal record {seq} does not chain: it carries {found} and the records before it \
         produce {expected} — the file was edited or spliced, and a fold over it is not a \
         reconstruction of what this service decided"
    )]
    Broken {
        /// Which record.
        seq: u64,
        /// The digest the record states.
        found: String,
        /// The digest the chain produces.
        expected: String,
    },

    /// The journal is shorter than the head this daemon last signed.
    ///
    /// Records were removed from the end — a cut file, or a restore from an
    /// older backup. The chain cannot see this; the signed head beside it can.
    #[error(
        "the journal holds {journal} records and this daemon signed a head at {signed}: the \
         last {missing} were removed or an older copy was restored — the daemon does not start \
         from a history shorter than one it vouched for"
    )]
    Truncated {
        /// How many records the journal holds.
        journal: u64,
        /// The position the last signed head names.
        signed: u64,
        /// How many are missing.
        missing: u64,
    },

    /// The record at the signed position is not the one that was signed.
    #[error(
        "journal record {position} is not the record this daemon signed at {position}: the \
         history was rewritten after it was vouched for"
    )]
    Diverged {
        /// The signed position.
        position: u64,
    },

    /// The journal runs past its last signed head by more than the one record
    /// a crash between the append and the signature can leave.
    #[error(
        "the journal holds {journal} records and the last signed head is at {signed}: records \
         were added without this daemon signing them, or its signed head was removed"
    )]
    Unsigned {
        /// How many records the journal holds.
        journal: u64,
        /// The last signed position (`0` when there is no signed head).
        signed: u64,
    },

    /// The signed head beside the journal does not read or does not verify
    /// under this daemon's signing key.
    #[error(
        "the signed head at {path} is not one this daemon can vouch for: {reason} — restore the \
         signing key it was made under"
    )]
    HeadUnreadable {
        /// Which file.
        path: PathBuf,
        /// Why.
        reason: String,
    },

    /// A sealed record does not open under this daemon's data key
    /// ([`crate::vault::Sealing`]).
    #[error(
        "journal record {seq} is sealed and does not open: {reason} — the daemon does not start \
         from records it cannot read; restore the data key the journal was written under"
    )]
    Sealed {
        /// Which record.
        seq: u64,
        /// Why.
        reason: String,
    },
}

/// One journal record, as it is written.
///
/// `body` holds the event already serialised, as a string, so the bytes the
/// digest covers are the bytes on disk — the same reason `ocmf` parses a signed
/// record without disturbing it. Re-serialising a parsed value to check a
/// digest checks the serialiser, not the file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// Position in the journal, from 1.
    pub seq: u64,
    /// The digest of the record before this one.
    pub prev: String,
    /// This record's digest, over `prev`, `seq`, `kind`, `actor` and `body`.
    pub hash: String,
    /// What kind of fact this is, for a reader with no schema to hand.
    pub kind: String,
    /// Who caused it.
    pub actor: Actor,
    /// The event, serialised.
    pub body: String,
}

impl Record {
    /// The digest a record with these parts must carry.
    #[must_use]
    pub fn digest(prev: &str, seq: u64, kind: &str, actor: &Actor, body: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(seq.to_string().as_bytes());
        hasher.update(b"\x1f");
        hasher.update(kind.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(actor.to_string().as_bytes());
        hasher.update(b"\x1f");
        hasher.update(body.as_bytes());
        hex(&hasher.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use core::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// An ordered, append-only, hash-chained record of what a service decided.
///
/// Two implementations, and the difference is only whether the records reach a
/// disk: [`Memory`] for a test and for a daemon told to keep nothing, [`File`]
/// for one an operator runs.
pub trait Journal: Send + Sync + 'static {
    /// Append one already-encoded fact, caused by `actor`, and return the
    /// record written.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] when the record cannot be made durable. A caller
    /// must treat that as the decision not having been made.
    fn append(&self, kind: &str, actor: &Actor, body: &str) -> Result<Record, JournalError>;

    /// Every record, in order, with the chain checked.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] when the journal cannot be read, and
    /// [`JournalError::Broken`] when a record does not follow from the one
    /// before it.
    fn records(&self) -> Result<Vec<Record>, JournalError>;

    /// Where the journal ends: the last position and digest, or
    /// [`Head::genesis`] for an empty one.
    ///
    /// The half a chain cannot prove about itself. See the module
    /// documentation: a head signed, kept and compared later is what makes a
    /// truncated tail visible, and nothing inside the file can do that job.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::records`] could not do.
    fn head(&self) -> Result<Head, JournalError> {
        Ok(self
            .records()?
            .last()
            .map_or_else(Head::genesis, |record| Head {
                position: record.seq,
                hash: record.hash.clone(),
            }))
    }
}

/// Check a chain and return the digest it ends on.
fn verify(records: &[Record]) -> Result<String, JournalError> {
    let mut prev = GENESIS.to_owned();
    for (index, record) in records.iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        let expected = Record::digest(&prev, seq, &record.kind, &record.actor, &record.body);
        if record.prev != prev || record.hash != expected || record.seq != seq {
            return Err(JournalError::Broken {
                seq,
                found: record.hash.clone(),
                expected,
            });
        }
        prev.clone_from(&record.hash);
    }
    Ok(prev)
}

/// A journal that keeps nothing past the process.
///
/// Not a stub: a daemon that must not write to a disk — one in a test, one in a
/// sandbox, one an operator points at a read-only estate — gets exactly the
/// same semantics minus durability, so the wiring above it is the same wiring.
#[derive(Debug, Default)]
pub struct Memory(Mutex<Vec<Record>>);

impl Memory {
    /// An empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify a complete, quiescent journal and copy its records into memory.
    /// Unlike startup after a crash, a recovery drill requires every input line
    /// to be present. Subsequent appends affect only this disposable copy.
    ///
    /// # Errors
    ///
    /// Returns an error for a partial final line, malformed record or broken chain.
    pub fn from_cold_cut(bytes: &[u8]) -> Result<Self, JournalError> {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            return Err(JournalError::Encoding {
                doing: "checked for a complete final line",
                source: serde_json::Error::io(std::io::ErrorKind::UnexpectedEof.into()),
            });
        }
        let mut records = Vec::new();
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            records.push(serde_json::from_slice(line).map_err(|source| {
                JournalError::Encoding {
                    doing: "read from a cold cut",
                    source,
                }
            })?);
        }
        verify(&records)?;
        Ok(Self(Mutex::new(records)))
    }
}

impl Journal for Memory {
    fn append(&self, kind: &str, actor: &Actor, body: &str) -> Result<Record, JournalError> {
        let record = {
            let mut held = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let seq = u64::try_from(held.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let prev = held
                .last()
                .map_or_else(|| GENESIS.to_owned(), |r| r.hash.clone());
            let record = Record {
                seq,
                hash: Record::digest(&prev, seq, kind, actor, body),
                prev,
                kind: kind.to_owned(),
                actor: actor.clone(),
                body: body.to_owned(),
            };
            held.push(record.clone());
            record
        };
        Ok(record)
    }

    fn records(&self) -> Result<Vec<Record>, JournalError> {
        let held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let records = held.clone();
        drop(held);
        verify(&records)?;
        Ok(records)
    }
}

/// Where a daemon's journal lives, from the environment.
///
/// `EMOB_JOURNAL_DIR`, and the default is deliberately **not** a temporary
/// directory: a service that silently journalled to `/tmp` would come back from
/// a reboot with an empty ledger and no error, which is the failure this module
/// exists to make impossible. `./var` is a path an operator notices.
#[must_use]
pub fn dir() -> PathBuf {
    std::env::var_os("EMOB_JOURNAL_DIR").map_or_else(|| PathBuf::from("var"), PathBuf::from)
}

/// The journal file for one daemon, under [`dir`].
///
/// # Errors
///
/// [`JournalError::Io`] when the directory or the file cannot be opened.
pub fn for_daemon(name: &str) -> Result<File, JournalError> {
    File::open(dir().join(format!("{name}.ndjson")))
}

/// A journal on disk: one JSON object per line, appended and flushed.
///
/// # A half-written line is a record that did not happen
///
/// A process killed mid-append leaves a trailing fragment. [`Self::records`]
/// stops at the last line that parses and chains, which is the only reading
/// that is safe in both directions: a decision whose record did not reach the
/// disk is one the caller was never told succeeded, and a decision whose record
/// did reach it is replayed. What it does **not** do is repair the file — a
/// journal this code rewrote is no longer evidence — so the fragment is
/// reported and left where it is.
#[derive(Debug)]
pub struct File {
    path: PathBuf,
    writing: Mutex<()>,
}

impl File {
    /// Open, or create, the journal at this path.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] when the parent directory cannot be created or the
    /// file cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| JournalError::Io {
                path: path.clone(),
                doing: "created",
                source,
            })?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| JournalError::Io {
                path: path.clone(),
                doing: "opened",
                source,
            })?;
        Ok(Self {
            path,
            writing: Mutex::new(()),
        })
    }

    /// Where the records are.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many trailing bytes do not parse as a record.
    ///
    /// Zero for a journal no process was killed in the middle of. Named rather
    /// than silently skipped, because a fragment is the one thing a reader of
    /// this file would want to be told about and the one thing a `for` loop
    /// over `lines()` throws away.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] when the file cannot be read.
    pub fn fragment(&self) -> Result<usize, JournalError> {
        let text = self.read()?;
        Ok(text
            .rsplit_once('\n')
            .map_or(text.len(), |(_, tail)| tail.len()))
    }

    fn read(&self) -> Result<String, JournalError> {
        std::fs::read_to_string(&self.path).map_err(|source| JournalError::Io {
            path: self.path.clone(),
            doing: "read",
            source,
        })
    }
}

impl Journal for File {
    fn append(&self, kind: &str, actor: &Actor, body: &str) -> Result<Record, JournalError> {
        // One writer at a time, and the read of the tail happens inside it: two
        // appends that each read the same last digest would produce two records
        // claiming one position, which is the shape `[UStG §14(4) Nr. 4]`
        // forbids one layer up.
        let held = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let existing = self.records()?;
        let seq = u64::try_from(existing.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let prev = existing
            .last()
            .map_or_else(|| GENESIS.to_owned(), |r| r.hash.clone());
        let record = Record {
            seq,
            hash: Record::digest(&prev, seq, kind, actor, body),
            prev,
            kind: kind.to_owned(),
            actor: actor.clone(),
            body: body.to_owned(),
        };
        let mut line = serde_json::to_string(&record).map_err(|source| JournalError::Encoding {
            doing: "written",
            source,
        })?;
        line.push('\n');

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|source| JournalError::Io {
                path: self.path.clone(),
                doing: "opened",
                source,
            })?;
        file.write_all(line.as_bytes())
            .map_err(|source| JournalError::Io {
                path: self.path.clone(),
                doing: "appended to",
                source,
            })?;
        // The decision is not made until it is on the disk. A caller told
        // `Ok` by a buffered write and then killed has an invoice number spent
        // in a file that does not mention it.
        file.sync_all().map_err(|source| JournalError::Io {
            path: self.path.clone(),
            doing: "flushed",
            source,
        })?;
        drop(held);
        Ok(record)
    }

    fn records(&self) -> Result<Vec<Record>, JournalError> {
        let text = self.read()?;
        let mut records = Vec::new();
        for line in std::io::Cursor::new(text.as_bytes()).lines() {
            let line = line.map_err(|source| JournalError::Io {
                path: self.path.clone(),
                doing: "read",
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            // A line that does not parse is the tail of a killed process, and
            // the only lawful one is the last. One in the middle is a file
            // somebody edited, which the chain reports on the next record.
            let Ok(record) = serde_json::from_str::<Record>(&line) else {
                break;
            };
            records.push(record);
        }
        verify(&records)?;
        Ok(records)
    }
}

/// A service's state, folded from its journal and extended by appending to it.
///
/// `S` is the aggregate, `E` the events it folds. The one rule this type
/// exists to enforce is the order: **the fact reaches the journal before it
/// reaches the state**, so a process killed between the two loses a decision
/// nobody was told about rather than keeping one nothing recorded.
pub struct Folded<S, E, J> {
    state: S,
    journal: J,
    apply: fn(&mut S, E),
}

impl<S, E, J> core::fmt::Debug for Folded<S, E, J>
where
    S: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Folded")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<S, E, J> Folded<S, E, J>
where
    E: Serialize + DeserializeOwned + Kinded,
    J: Journal,
{
    /// Rebuild the state by folding every record in the journal over `apply`.
    ///
    /// # Errors
    ///
    /// [`JournalError::Broken`] when the chain does not hold — a journal that
    /// was edited is not a record of what this service decided, and starting
    /// from it would be inventing one — and [`JournalError::Encoding`] when a
    /// record's body is not an event this build understands, which is a
    /// downgrade rather than a corruption and is refused for the same reason.
    pub fn open(empty: S, journal: J, apply: fn(&mut S, E)) -> Result<Self, JournalError> {
        let mut state = empty;
        for record in journal.records()? {
            let event: E =
                serde_json::from_str(&record.body).map_err(|source| JournalError::Encoding {
                    doing: "read back",
                    source,
                })?;
            apply(&mut state, event);
        }
        Ok(Self {
            state,
            journal,
            apply,
        })
    }

    /// Record a fact `by` its actor and fold it in.
    ///
    /// # Errors
    ///
    /// Whatever the journal could not do. The state is **not** advanced when
    /// the record could not be written.
    pub fn record(&mut self, by: &Actor, event: E) -> Result<Record, JournalError> {
        let body = serde_json::to_string(&event).map_err(|source| JournalError::Encoding {
            doing: "written",
            source,
        })?;
        let record = self.journal.append(event.kind(), by, &body)?;
        (self.apply)(&mut self.state, event);
        Ok(record)
    }

    /// Record a fact the caller has **already** applied.
    ///
    /// The other order, for a service whose decision and mutation are one call
    /// — which is most of them, because the checks a decision makes read the
    /// state it is about to change. The caller owes one `note` per fact and the
    /// obligation is not mechanisable here; what is, and what matters, is what
    /// happens when this fails. The state has advanced and the journal has not,
    /// so the caller's answer is to **stop**, not to retry: a service that made
    /// a second decision from a state its journal cannot reproduce is one whose
    /// books nobody can reconstruct.
    ///
    /// # Errors
    ///
    /// Whatever the journal could not do.
    pub fn note(&mut self, by: &Actor, event: &E) -> Result<Record, JournalError> {
        let body = serde_json::to_string(event).map_err(|source| JournalError::Encoding {
            doing: "written",
            source,
        })?;
        self.journal.append(event.kind(), by, &body)
    }

    /// The folded state.
    pub const fn state(&self) -> &S {
        &self.state
    }

    /// The folded state, to decide on and change in one call.
    ///
    /// Every change made through this has to be followed by [`Self::note`], or
    /// the service holds a fact its journal does not and the next restart
    /// forgets it.
    pub const fn state_mut(&mut self) -> &mut S {
        &mut self.state
    }

    /// The folded state, for a caller that only reads through it.
    ///
    /// Deliberately not a `&mut`: a service that mutated its aggregate directly
    /// would hold a fact its journal does not, which is the whole failure this
    /// type exists to prevent.
    pub const fn journal(&self) -> &J {
        &self.journal
    }
}

/// An event that can say what kind of fact it is.
///
/// One short word, so somebody reading the file with `jq` can tell an issue
/// from a booking without a schema.
pub trait Kinded {
    /// The kind, as it is written into the record.
    fn kind(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
    enum Fact {
        Added(u32),
        Removed(u32),
    }

    impl Kinded for Fact {
        fn kind(&self) -> &'static str {
            match self {
                Self::Added(_) => "added",
                Self::Removed(_) => "removed",
            }
        }
    }

    fn apply(total: &mut i64, fact: Fact) {
        match fact {
            Fact::Added(n) => *total += i64::from(n),
            Fact::Removed(n) => *total -= i64::from(n),
        }
    }

    #[test]
    fn a_fold_over_a_memory_journal_is_the_sum_of_its_facts() {
        let mut folded = Folded::open(0_i64, Memory::new(), apply).expect("an empty journal folds");
        folded
            .record(&Actor::Timer, Fact::Added(7))
            .expect("recorded");
        folded
            .record(&Actor::Timer, Fact::Added(5))
            .expect("recorded");
        folded
            .record(&Actor::Timer, Fact::Removed(2))
            .expect("recorded");
        assert_eq!(*folded.state(), 10);
    }

    #[test]
    fn a_file_journal_folds_to_the_same_state_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("emob-journal-{}", std::process::id()));
        let path = dir.join("restart.ndjson");
        let _ = std::fs::remove_file(&path);

        {
            let mut folded = Folded::open(0_i64, File::open(&path).expect("opened"), apply)
                .expect("an empty journal folds");
            folded
                .record(&Actor::Timer, Fact::Added(41))
                .expect("recorded");
            folded
                .record(&Actor::Timer, Fact::Added(1))
                .expect("recorded");
            assert_eq!(*folded.state(), 42);
        }

        // The process is gone. Everything it decided is in the file.
        let reopened = Folded::open(0_i64, File::open(&path).expect("reopened"), apply)
            .expect("the journal folds");
        assert_eq!(*reopened.state(), 42, "a restart is a replay");
        assert_eq!(reopened.journal().records().expect("chained").len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edited_record_breaks_the_chain_and_the_service_refuses_to_start() {
        let dir = std::env::temp_dir().join(format!("emob-journal-edit-{}", std::process::id()));
        let path = dir.join("edited.ndjson");
        let _ = std::fs::remove_file(&path);
        {
            let mut folded =
                Folded::open(0_i64, File::open(&path).expect("opened"), apply).expect("folds");
            folded
                .record(&Actor::Timer, Fact::Added(10))
                .expect("recorded");
            folded
                .record(&Actor::Timer, Fact::Added(10))
                .expect("recorded");
            folded
                .record(&Actor::Timer, Fact::Added(10))
                .expect("recorded");
        }
        // Somebody edits the middle of the file — the amount, not the digest,
        // which is what an edit made to change an answer looks like.
        let text = std::fs::read_to_string(&path).expect("read");
        let tampered = text.replacen("{\\\"Added\\\":10}", "{\\\"Added\\\":99}", 1);
        assert_ne!(tampered, text, "the fixture must actually change a body");
        std::fs::write(&path, tampered).expect("written");

        let refused = Folded::open(0_i64, File::open(&path).expect("opened"), apply)
            .expect_err("a rewritten journal is not a record of what was decided");
        assert!(
            matches!(refused, JournalError::Broken { seq: 1, .. }),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sr_100_an_edited_actor_breaks_the_chain() {
        let journal = Memory::new();
        let station = Actor::station("CP-1").unwrap();
        journal.append("added", &station, "{}").unwrap();
        journal.append("added", &Actor::Timer, "{}").unwrap();
        let mut records = journal.records().unwrap();
        records[0].actor = Actor::operator("somebody else").unwrap();
        let refused = verify(&records).expect_err("the actor is inside the digest");
        assert!(
            matches!(refused, JournalError::Broken { seq: 1, .. }),
            "{refused:?}"
        );
    }

    #[test]
    fn sr_100_a_record_without_an_actor_does_not_read_back() {
        let line = r#"{"seq":1,"prev":"0000000000000000000000000000000000000000000000000000000000000000","hash":"ab","kind":"added","body":"{}"}"#;
        assert!(serde_json::from_str::<Record>(line).is_err());
    }

    #[test]
    fn sr_101_an_actor_is_a_closed_kind_and_a_narrow_alphabet() {
        for text in [
            "station:CP-1",
            "partner:DE*MSP",
            "operator:demo operator",
            "service:csmsd",
            "timer",
            "estate",
            "station:urn:emob:cp=1",
        ] {
            let actor: Actor = text.parse().unwrap();
            assert_eq!(actor.to_string(), text);
            let json = serde_json::to_string(&actor).unwrap();
            assert_eq!(serde_json::from_str::<Actor>(&json).unwrap(), actor);
        }
        for refused in [
            "station:",
            "user:alice",
            "operator:Bearer abc/def",
            "station:CP-1\nforged",
            "operator:\"quoted\"",
            "timer:x",
            &format!("station:{}", "x".repeat(65)),
        ] {
            assert!(refused.parse::<Actor>().is_err(), "{refused}");
        }
    }

    #[test]
    fn a_half_written_line_is_a_decision_that_did_not_happen() {
        let dir = std::env::temp_dir().join(format!("emob-journal-tail-{}", std::process::id()));
        let path = dir.join("killed.ndjson");
        let _ = std::fs::remove_file(&path);
        {
            let mut folded =
                Folded::open(0_i64, File::open(&path).expect("opened"), apply).expect("folds");
            folded
                .record(&Actor::Timer, Fact::Added(3))
                .expect("recorded");
        }
        // The process died with half a record on the disk.
        let mut text = std::fs::read_to_string(&path).expect("read");
        text.push_str("{\"seq\":2,\"prev\":\"ab\",\"ha");
        std::fs::write(&path, text).expect("written");

        let journal = File::open(&path).expect("opened");
        assert_eq!(journal.records().expect("chained").len(), 1);
        assert!(
            journal.fragment().expect("measured") > 0,
            "the tail is named"
        );

        let folded = Folded::open(0_i64, journal, apply).expect("the complete records fold");
        assert_eq!(*folded.state(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
