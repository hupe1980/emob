//! The witness: somebody else's memory of where each journal ended.
//!
//! # Why a daemon's own signed head is not enough
//!
//! [`crate::head::Signing`] refuses a journal shorter than the head beside it —
//! but whoever can cut the journal can delete that file too, and a restore
//! from last week's backup brings last week's head with it. The answer is a
//! copy the daemon's volume does not hold: a witness that reads
//! `GET /v1/<name>/head` from time to time and remembers the highest position
//! it saw and the hash there. A head that comes back **lower** is a lost tail;
//! one at the same position with another hash is a **fork**; one signed by
//! another key is a **rekey** — and each is an alarm.
//!
//! # An alarm stays
//!
//! Every observation is a record in the witness's own chained journal, so an
//! alarm is history rather than a flag the next good head clears: Heute shows
//! it until somebody moves the witness file aside, and an edited witness file
//! does not open. A witness that kept only the latest head would forget the
//! one thing it exists to remember.
//!
//! # What it does not check
//!
//! A head that moved **forward** is accepted as it is. The journal is a chain,
//! not a Merkle tree, so there is no proof that the new head extends the old
//! one; a history rewritten *and* extended past the witnessed position is out
//! of reach — the daemon's own start check ([`crate::head`]) is what refuses a
//! rewrite under a head it signed.
//!
//! # Whose key
//!
//! The estate's `[witness] keys` names a daemon's verifier key when the
//! operator wrote it down; otherwise the first key seen is pinned (trust on
//! first use, and Heute says which).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use crate::head::VerifierKey;
use crate::journal::{Actor, File, Folded, JournalError, Kinded};

/// What one observation concluded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// The first head seen from this daemon; its key is pinned.
    First,
    /// The head moved forward.
    Advanced {
        /// The highest position witnessed before.
        from: u64,
    },
    /// The same head as before.
    Unchanged,
    /// The head went backwards: records were removed or an older copy restored.
    Regressed {
        /// The highest position witnessed before.
        witnessed: u64,
    },
    /// The same position with another hash: the history was rewritten.
    Forked,
    /// Signed by a key other than the pinned one.
    Rekeyed {
        /// The key the witness holds for this daemon.
        pinned: String,
    },
}

impl Verdict {
    /// Whether this is an alarm.
    #[must_use]
    pub const fn is_alarm(&self) -> bool {
        matches!(
            self,
            Self::Regressed { .. } | Self::Forked | Self::Rekeyed { .. }
        )
    }
}

/// One head the witness was shown, and what it made of it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Observed {
    daemon: String,
    #[serde(with = "time::serde::rfc3339")]
    at: time::OffsetDateTime,
    key: String,
    position: u64,
    hash: String,
    #[serde(flatten)]
    verdict: Verdict,
}

impl Kinded for Observed {
    fn kind(&self) -> &'static str {
        if self.verdict.is_alarm() {
            "alarm"
        } else {
            "witnessed"
        }
    }
}

/// What the witness holds about one daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// Which daemon.
    pub daemon: String,
    /// The highest position witnessed.
    pub position: u64,
    /// The hash at it.
    pub hash: String,
    /// When it was last read.
    pub at: time::OffsetDateTime,
    /// The verifier key held for it.
    pub key: String,
    /// Whether that key is the estate's (else pinned at first sight).
    pub key_from_estate: bool,
    /// The position the last read named — below `position` after a regression.
    pub last: u64,
    /// The first alarm, kept.
    pub alarm: Option<Verdict>,
}

#[derive(Debug, Default)]
struct State(BTreeMap<String, Seen>);

fn apply(state: &mut State, fact: Observed) {
    let seen = state.0.entry(fact.daemon.clone()).or_insert_with(|| Seen {
        daemon: fact.daemon.clone(),
        position: fact.position,
        hash: fact.hash.clone(),
        at: fact.at,
        key: fact.key.clone(),
        key_from_estate: false,
        last: fact.position,
        alarm: None,
    });
    seen.at = fact.at;
    seen.last = fact.position;
    match fact.verdict {
        Verdict::First | Verdict::Advanced { .. } | Verdict::Unchanged => {
            seen.position = fact.position;
            seen.hash = fact.hash;
        }
        alarm => {
            seen.alarm.get_or_insert(alarm);
        }
    }
}

/// Why the witness could not record a head.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WitnessError {
    /// Its own file could not be read or written.
    #[error("the witness's own record: {0}")]
    Journal(#[from] JournalError),
    /// An estate key does not parse.
    #[error("the estate's witness.keys.{daemon} is not a verifier key: {reason}")]
    EstateKey {
        /// Which daemon.
        daemon: String,
        /// Why.
        reason: String,
    },
    /// The note verifies under no key the witness can accept.
    #[error("{daemon}'s head does not verify: {reason}")]
    Unverified {
        /// Which daemon.
        daemon: String,
        /// Why.
        reason: String,
    },
}

/// A witness over its own chained file.
pub struct Witness {
    folded: Mutex<Folded<State, Observed, File>>,
    estate: BTreeMap<String, VerifierKey>,
}

impl core::fmt::Debug for Witness {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Witness").finish_non_exhaustive()
    }
}

impl Witness {
    /// Open, or start, the witness file at `path`, trusting `keys` (daemon →
    /// verifier key) over anything pinned.
    ///
    /// # Errors
    ///
    /// [`WitnessError::EstateKey`], and a witness file that does not open — an
    /// edited one among them.
    pub fn open(path: &Path, keys: &BTreeMap<String, String>) -> Result<Self, WitnessError> {
        let mut estate = BTreeMap::new();
        for (daemon, key) in keys {
            let parsed =
                key.parse()
                    .map_err(|error: crate::head::NoteError| WitnessError::EstateKey {
                        daemon: daemon.clone(),
                        reason: error.to_string(),
                    })?;
            estate.insert(daemon.clone(), parsed);
        }
        Ok(Self {
            folded: Mutex::new(Folded::open(State::default(), File::open(path)?, apply)?),
            estate,
        })
    }

    /// Record the head `note` that `daemon` served with `offered` as its key,
    /// `by` whoever asked, at `at`.
    ///
    /// # Errors
    ///
    /// [`WitnessError::Unverified`] for a note no acceptable key signed —
    /// nothing is recorded — and [`WitnessError::Journal`] when the
    /// observation could not be written.
    pub fn observe(
        &self,
        daemon: &str,
        note: &str,
        offered: Option<&str>,
        at: time::OffsetDateTime,
        by: &Actor,
    ) -> Result<Verdict, WitnessError> {
        let unverified = |reason: String| WitnessError::Unverified {
            daemon: daemon.to_owned(),
            reason,
        };
        let offered: Option<VerifierKey> = offered.and_then(|key| key.parse().ok());
        let mut folded = self
            .folded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = folded.state().0.get(daemon).cloned();
        let pinned: Option<VerifierKey> = self
            .estate
            .get(daemon)
            .cloned()
            .or_else(|| current.as_ref().and_then(|seen| seen.key.parse().ok()));

        let (key, head, rekeyed) = if let Some(pinned) = pinned {
            match pinned.open(note) {
                Ok(head) => (pinned, head, None),
                Err(error) => {
                    let other = offered
                        .filter(|key| *key != pinned)
                        .and_then(|key| key.open(note).ok().map(|head| (key, head)));
                    let Some((key, head)) = other else {
                        return Err(unverified(error.to_string()));
                    };
                    (key, head, Some(pinned.to_string()))
                }
            }
        } else {
            let key = offered.ok_or_else(|| unverified("no verifier key".to_owned()))?;
            let head = key
                .open(note)
                .map_err(|error| unverified(error.to_string()))?;
            (key, head, None)
        };
        let verdict = match (rekeyed, &current) {
            (Some(pinned), _) => Verdict::Rekeyed { pinned },
            (None, None) => Verdict::First,
            (None, Some(seen)) if head.position < seen.position => Verdict::Regressed {
                witnessed: seen.position,
            },
            (None, Some(seen)) if head.position == seen.position && head.hash != seen.hash => {
                Verdict::Forked
            }
            (None, Some(seen)) if head.position == seen.position => Verdict::Unchanged,
            (None, Some(seen)) => Verdict::Advanced {
                from: seen.position,
            },
        };
        folded.record(
            by,
            Observed {
                daemon: daemon.to_owned(),
                at,
                key: key.to_string(),
                position: head.position,
                hash: head.hash,
                verdict: verdict.clone(),
            },
        )?;
        drop(folded);
        Ok(verdict)
    }

    /// What the witness holds, per daemon, by name.
    #[must_use]
    pub fn view(&self) -> Vec<Seen> {
        let folded = self
            .folded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        folded
            .state()
            .0
            .values()
            .map(|seen| {
                let mut seen = seen.clone();
                if let Some(key) = self.estate.get(&seen.daemon) {
                    seen.key = key.to_string();
                    seen.key_from_estate = true;
                }
                seen
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::head::SigningKey;
    use crate::journal::Head;

    fn head(position: u64, byte: u8) -> Head {
        Head {
            position,
            hash: hex::encode([byte; 32]),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("emob-witness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("witness.ndjson")
    }

    const AT: time::OffsetDateTime = time::OffsetDateTime::UNIX_EPOCH;

    #[test]
    fn sr_098_a_head_that_goes_backwards_or_forks_is_an_alarm_that_stays() {
        let path = scratch("alarm");
        let key = SigningKey::from_seed(&[1; 32], "csmsd");
        let vkey = key.verifier().to_string();
        let witness = Witness::open(&path, &BTreeMap::new()).unwrap();
        let see = |h: Head| {
            witness
                .observe("csmsd", &key.sign(&h), Some(&vkey), AT, &Actor::Timer)
                .unwrap()
        };
        assert_eq!(see(head(5, 1)), Verdict::First);
        assert_eq!(see(head(5, 1)), Verdict::Unchanged);
        assert_eq!(see(head(7, 2)), Verdict::Advanced { from: 5 });
        assert_eq!(see(head(6, 3)), Verdict::Regressed { witnessed: 7 });
        assert_eq!(see(head(7, 9)), Verdict::Forked);
        // A later good head does not clear it.
        assert_eq!(see(head(8, 4)), Verdict::Advanced { from: 7 });
        let seen = &witness.view()[0];
        assert_eq!(seen.position, 8);
        assert_eq!(seen.alarm, Some(Verdict::Regressed { witnessed: 7 }));
        assert!(!seen.key_from_estate);
        drop(witness);

        // …nor does a restart.
        let reopened = Witness::open(&path, &BTreeMap::new()).unwrap();
        assert_eq!(
            reopened.view()[0].alarm,
            Some(Verdict::Regressed { witnessed: 7 })
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sr_099_only_the_pinned_or_estate_key_is_accepted() {
        let path = scratch("keys");
        let key = SigningKey::from_seed(&[1; 32], "billd");
        let intruder = SigningKey::from_seed(&[2; 32], "billd");
        let estate = BTreeMap::from([("billd".to_owned(), key.verifier().to_string())]);
        let witness = Witness::open(&path, &estate).unwrap();

        // Pinned by the estate, so a first head under another key is a rekey.
        let verdict = witness
            .observe(
                "billd",
                &intruder.sign(&head(3, 1)),
                Some(&intruder.verifier().to_string()),
                AT,
                &Actor::Timer,
            )
            .unwrap();
        assert!(matches!(verdict, Verdict::Rekeyed { .. }), "{verdict:?}");

        // A note nobody acceptable signed is refused and not recorded.
        let mut forged = key.sign(&head(4, 1));
        forged = forged.replacen("\n4\n", "\n40\n", 1);
        let refused = witness
            .observe("billd", &forged, None, AT, &Actor::Timer)
            .unwrap_err();
        assert!(
            matches!(refused, WitnessError::Unverified { .. }),
            "{refused}"
        );

        let good = witness
            .observe("billd", &key.sign(&head(4, 1)), None, AT, &Actor::Timer)
            .unwrap();
        assert_eq!(good, Verdict::Advanced { from: 3 });
        let seen = &witness.view()[0];
        assert!(seen.key_from_estate);
        assert!(matches!(seen.alarm, Some(Verdict::Rekeyed { .. })));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// SC-001's other half: a journal restored **with** its signed head from
    /// an older backup is self-consistent, so the daemon opens it — and the
    /// witness is what says a record is gone.
    #[test]
    fn sr_098_a_journal_restored_from_an_older_backup_is_caught_by_the_witness() {
        use crate::head::{Signing, beside};
        use crate::journal::{File, Journal as _};
        let path = scratch("restored");
        let journal = path.with_file_name("csmsd.ndjson");
        let key = || SigningKey::from_seed(&[5; 32], "csmsd");
        let vkey = key().verifier().to_string();
        let witness = Witness::open(&path, &BTreeMap::new()).unwrap();
        let backup = path.with_file_name("backup");
        {
            let signed =
                Signing::open(File::open(&journal).unwrap(), key(), beside(&journal)).unwrap();
            signed.append("added", &Actor::Timer, "{}").unwrap();
            signed.append("added", &Actor::Timer, "{}").unwrap();
            std::fs::create_dir_all(&backup).unwrap();
            std::fs::copy(&journal, backup.join("journal")).unwrap();
            std::fs::copy(beside(&journal), backup.join("head")).unwrap();
            signed.append("added", &Actor::Timer, "{}").unwrap();
            let seen = witness
                .observe(
                    "csmsd",
                    &signed.latest().note(),
                    Some(&vkey),
                    AT,
                    &Actor::Timer,
                )
                .unwrap();
            assert_eq!(seen, Verdict::First);
        }
        std::fs::copy(backup.join("journal"), &journal).unwrap();
        std::fs::copy(backup.join("head"), beside(&journal)).unwrap();
        let restored = Signing::open(File::open(&journal).unwrap(), key(), beside(&journal))
            .expect("journal and head agree, so the daemon starts");
        let seen = witness
            .observe(
                "csmsd",
                &restored.latest().note(),
                Some(&vkey),
                AT,
                &Actor::Timer,
            )
            .unwrap();
        assert_eq!(seen, Verdict::Regressed { witnessed: 3 });
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sr_098_an_edited_witness_file_does_not_open() {
        let path = scratch("edited");
        let key = SigningKey::from_seed(&[1; 32], "roamd");
        let vkey = key.verifier().to_string();
        {
            let witness = Witness::open(&path, &BTreeMap::new()).unwrap();
            witness
                .observe(
                    "roamd",
                    &key.sign(&head(9, 1)),
                    Some(&vkey),
                    AT,
                    &Actor::Timer,
                )
                .unwrap();
            witness
                .observe(
                    "roamd",
                    &key.sign(&head(4, 1)),
                    Some(&vkey),
                    AT,
                    &Actor::Timer,
                )
                .unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("regressed", "unchanged")).unwrap();
        assert!(Witness::open(&path, &BTreeMap::new()).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
