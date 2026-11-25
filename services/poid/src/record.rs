//! The fourth ledger, and the smallest.
//!
//! `poid` holds one fact about the past: **which** snapshot the national access
//! point took. `[DATEX-II-Profil]` makes every dynamic update address a
//! facility at the `versionG` the table published it at, and a consumer has no
//! way to ask about a version it never received — so a status message built
//! against one is dropped in silence (D308).
//!
//! Lost, that fact reads as *"no snapshot has ever been accepted"*, and
//! [`crate::Poid::status`] then **refuses** rather than guessing. That is the
//! safe direction and is why this ledger was the last of the four to be
//! journalled. It is not the right one: a publisher that re-sends its whole
//! estate after every restart is one an access point rate-limits.

use emob_poi::datex::{Published, TablePublication};
use emob_service::Actor;
use emob_service::journal::{Journal, JournalError, Kinded, Record};

use crate::{Accepted, Poid};

/// The one fact this service decides.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// The dynamic receiver accepted the source positions, or delivery failed at that cut.
    Dynamic {
        /// Independently checkpointed dynamic feed.
        checkpoint: crate::live::Checkpoint,
    },
    /// A delivery attempt, failure or acceptance after the initial destination is known.
    Progress {
        /// Last attempt and confirmed delivery state.
        delivery: crate::live::DeliveryState,
        /// Only positions newly acknowledged by this receiver.
        changed: std::collections::BTreeMap<String, crate::live::Cursor>,
        /// Confirmed removals from the receiver's inventory.
        removed: Vec<String>,
    },
    /// The access point took a snapshot, and this is what that snapshot put on
    /// the record.
    Accepted {
        /// The document's own instant, read off it rather than stated beside
        /// it.
        #[serde(with = "time::serde::rfc3339")]
        published_at: time::OffsetDateTime,
        /// When the access point answered.
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
        /// The facility versions and rate names a status message may address.
        published: Published,
    },
}

impl Kinded for Event {
    fn kind(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Dynamic { .. } => "dynamic",
            Self::Progress { .. } => "progress",
        }
    }
}

/// The publication ledger, folded from a journal.
///
/// Not a [`emob_service::journal::Folded`]: [`Poid`] borrows the closure that
/// prices a point, so it cannot be owned by a long-lived wrapper without
/// infecting every caller with that lifetime. What is here instead is the two
/// halves of a fold done by hand — [`Self::last_accepted`] on the way up, and a
/// record written after the decision on the way through — which is the same
/// contract in four lines rather than a generic parameter nobody needs.
#[derive(Debug)]
pub struct Ledger<J: Journal> {
    journal: J,
    halted: Option<String>,
}

impl<J: Journal> Ledger<J> {
    /// Read the journal and hand the service what it says.
    ///
    /// # Errors
    ///
    /// [`JournalError::Broken`] when the journal does not chain, and
    /// [`JournalError::Encoding`] when it holds a fact this build cannot read.
    pub fn open(journal: J) -> Result<Self, JournalError> {
        // Read once here so that a journal that does not chain stops the daemon
        // at start-up rather than at the first publication.
        let _ = Self::fold(&journal)?;
        Ok(Self {
            journal,
            halted: None,
        })
    }

    fn fold(journal: &J) -> Result<(Option<Accepted>, crate::live::Checkpoint), JournalError> {
        let mut dynamic = crate::live::Checkpoint::default();
        let mut held = None;
        for record in journal.records()? {
            let event: Event =
                serde_json::from_str(&record.body).map_err(|source| JournalError::Encoding {
                    doing: "read back",
                    source,
                })?;
            match event {
                Event::Dynamic { checkpoint } => {
                    if checkpoint.destinations != dynamic.destinations {
                        held = None;
                    }
                    dynamic = checkpoint;
                }
                Event::Progress {
                    delivery,
                    changed,
                    removed,
                } => {
                    dynamic.delivery = delivery;
                    dynamic.points.extend(changed);
                    for id in removed {
                        dynamic.points.remove(&id);
                    }
                }
                Event::Accepted {
                    published_at,
                    at,
                    published,
                } => {
                    dynamic.delivery.static_accepted_at = Some(at);
                    held = Some(Accepted {
                        published_at,
                        at,
                        published,
                    });
                }
            }
        }
        Ok((held, dynamic))
    }

    /// What the access point last took, as the journal records it.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn last_accepted(&self) -> Result<Option<Accepted>, JournalError> {
        Self::fold(&self.journal).map(|(held, _)| held)
    }

    /// Restore the dynamic receiver and its original delivery evidence.
    ///
    /// # Errors
    /// A journal that cannot be read or decoded.
    pub fn last_dynamic(&self) -> Result<crate::live::Checkpoint, JournalError> {
        Self::fold(&self.journal).map(|(_, checkpoint)| checkpoint)
    }

    /// Commit dynamic delivery evidence before publishing the new read model.
    ///
    /// # Errors
    /// A broken or unavailable signed journal stops this worker.
    pub fn checkpoint(&mut self, checkpoint: &crate::live::Checkpoint) -> Result<(), JournalError> {
        self.append(
            &Actor::Timer,
            &Event::Dynamic {
                checkpoint: checkpoint.clone(),
            },
        )
        .map(|_| ())
    }

    /// Preserve delivery evidence and any changed receiver positions in one journal record.
    ///
    /// # Errors
    /// A broken or unavailable journal stops the worker.
    pub fn progress(
        &mut self,
        previous: &crate::live::Checkpoint,
        delivery: &crate::live::DeliveryState,
        accepted: Option<&std::collections::BTreeMap<String, crate::live::Cursor>>,
    ) -> Result<(), JournalError> {
        let mut changed = std::collections::BTreeMap::new();
        let mut removed = Vec::new();
        if let Some(points) = accepted {
            changed.extend(
                points
                    .iter()
                    .filter(|(id, cursor)| previous.points.get(*id) != Some(cursor))
                    .map(|(id, cursor)| (id.clone(), cursor.clone())),
            );
            removed.extend(
                previous
                    .points
                    .keys()
                    .filter(|id| !points.contains_key(*id))
                    .cloned(),
            );
        }
        self.append(
            &Actor::Timer,
            &Event::Progress {
                delivery: delivery.clone(),
                changed,
                removed,
            },
        )
        .map(|_| ())
    }

    /// The journal under it.
    #[must_use]
    pub const fn journal(&self) -> &J {
        &self.journal
    }

    /// Whether the service has stopped recording.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.halted.is_some()
    }

    /// Record that the access point took a publication, and tell the service.
    ///
    /// The order is the one every ledger here keeps: the service is told, and
    /// then the fact is written — a process killed between the two loses both,
    /// which is the safe direction. A journal that **refuses** leaves the
    /// service ahead of its record, and this stops recording rather than
    /// letting the next restart forget a snapshot the access point holds.
    ///
    /// `by` is who caused it: the publish loop's push came back accepted, so
    /// it is [`Actor::Timer`] — the access point answered a call this daemon
    /// made on its own cadence, not a request anybody sent in (SR-100).
    ///
    /// # Errors
    ///
    /// Whatever the journal could not do.
    pub fn accepted(
        &mut self,
        by: &Actor,
        service: &mut Poid<'_>,
        publication: &TablePublication,
        at: time::OffsetDateTime,
    ) -> Result<Record, JournalError> {
        if let Some(why) = &self.halted {
            return Err(JournalError::Encoding {
                doing: "written after this service halted",
                source: serde::de::Error::custom(why.clone()),
            });
        }
        service.accepted(publication, at);
        let event = Event::Accepted {
            published_at: publication.published_at(),
            at,
            published: publication.published().clone(),
        };
        self.append(by, &event)
    }

    fn append(&mut self, by: &Actor, event: &Event) -> Result<Record, JournalError> {
        if let Some(why) = &self.halted {
            return Err(JournalError::Encoding {
                doing: "written after this service halted",
                source: serde::de::Error::custom(why.clone()),
            });
        }
        let body = serde_json::to_string(event).map_err(|source| JournalError::Encoding {
            doing: "written",
            source,
        })?;
        match self.journal.append(event.kind(), by, &body) {
            Ok(record) => Ok(record),
            Err(error) => {
                self.halted = Some(error.to_string());
                Err(error)
            }
        }
    }
}
