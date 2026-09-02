//! What a crossing cost, said out loud.
//!
//! # Why this is vocabulary rather than one wire's detail
//!
//! A canonical value carried onto a wire is not a re-encoding. The wire has its
//! own model, and where the two models disagree something has to give: a
//! quantity is rounded, a distinction is collapsed, a fact has no field to live
//! in. A `From` impl makes those decisions silently, once, at the moment nobody
//! is looking — and the consequence surfaces weeks later as two parties holding
//! two different numbers for one session.
//!
//! So every translation onto a wire in this workspace returns a [`Crossing`]:
//! the value, and the account. The account is not decoration. It is the answer
//! to *"why does your number differ from mine"*, and it is the only artefact
//! either party has when the session is six weeks old.
//!
//! Three seams now state that in the same words — OCPI in `emob-roam`, the
//! DATEX II national access point feed in `emob-poi`, and OCPP 2.1's tariff in
//! `emob-ocpp` — so the type lives in the crate all three depend on rather than
//! being spelled three ways. A partner reading an account of an OCPI downgrade
//! and an operator reading an account of what a charge point's own screen
//! cannot show are asking the same question.
//!
//! # The pointer points at what the reader is looking at
//!
//! [`Note::pointer`] is an RFC 6901 JSON Pointer into the **target** document —
//! the thing the recipient will have open when they ask. A note that points
//! into the source names a field the reader cannot see.

use core::fmt;

/// One thing a crossing could not carry, or could carry only approximately.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Note {
    /// RFC 6901 JSON Pointer to the value the note is about, in the target
    /// document.
    pub pointer: String,
    /// What happened to it, and why.
    pub reason: String,
}

impl Note {
    /// A note about one field.
    pub fn new(pointer: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = if self.pointer.is_empty() {
            "/"
        } else {
            &self.pointer
        };
        write!(f, "{at}: {}", self.reason)
    }
}

/// A translated value and the account of what the translation cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossing<T> {
    /// The translated value.
    pub value: T,
    notes: Vec<Note>,
}

impl<T> Crossing<T> {
    /// A crossing that lost nothing.
    #[must_use]
    pub const fn lossless(value: T) -> Self {
        Self {
            value,
            notes: Vec::new(),
        }
    }

    /// Record something the crossing could not carry exactly.
    pub fn note(&mut self, pointer: impl Into<String>, reason: impl Into<String>) {
        self.notes.push(Note::new(pointer, reason));
    }

    /// Everything the crossing could not carry exactly, in document order.
    #[must_use]
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// Whether nothing was lost or approximated.
    #[must_use]
    pub fn is_lossless(&self) -> bool {
        self.notes.is_empty()
    }

    /// One line per note, for an operator queue or a dispute file.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.notes.iter().map(ToString::to_string)
    }

    /// The value, discarding the account.
    ///
    /// Deliberately verbose. A caller reaching for this is throwing away the
    /// only record of what the recipient's copy does not say, and the name is
    /// the last place to notice.
    #[must_use]
    pub fn into_value_discarding_notes(self) -> T {
        self.value
    }

    /// Carry the account onto a new value.
    #[must_use]
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Crossing<U> {
        Crossing {
            value: f(self.value),
            notes: self.notes,
        }
    }

    /// Append notes from anywhere — a lower layer's own account, prefixed so
    /// the merged report says which object each loss was in.
    ///
    /// A page of records translated together otherwise produces twenty notes
    /// all pointing at `/total_cost`.
    pub fn absorb_notes(&mut self, prefix: &str, notes: impl IntoIterator<Item = Note>) {
        self.notes.extend(notes.into_iter().map(|note| Note {
            pointer: format!("{prefix}{}", note.pointer),
            reason: note.reason,
        }));
    }

    /// Fold in another crossing's account, taking its value.
    pub fn absorb_from<U>(&mut self, other: Crossing<U>) -> U {
        self.notes.extend(other.notes);
        other.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lossless_crossing_has_nothing_to_say() {
        let crossing = Crossing::lossless(42);
        assert!(crossing.is_lossless());
        assert_eq!(crossing.reasons().count(), 0);
    }

    #[test]
    fn a_note_names_the_field_the_reader_will_be_looking_at() {
        let mut crossing = Crossing::lossless(42);
        crossing.note("/total_time", "0.3333 h is 20 minutes rounded");
        assert_eq!(
            crossing.reasons().next().unwrap(),
            "/total_time: 0.3333 h is 20 minutes rounded"
        );
    }

    #[test]
    fn an_empty_pointer_reads_as_the_whole_document() {
        assert_eq!(
            Note::new("", "the record itself").to_string(),
            "/: the record itself"
        );
    }

    #[test]
    fn an_absorbed_account_says_which_object_it_came_from() {
        // A recipient gets one report, not two half-reports that have to be
        // read together to mean anything.
        let mut crossing = Crossing::lossless(());
        crossing.absorb_notes(
            "/total_cost",
            [Note::new("/taxes", "2.2.1 carries one tax amount, not two")],
        );
        assert_eq!(crossing.notes()[0].pointer, "/total_cost/taxes");
    }

    #[test]
    fn a_crossings_account_survives_being_mapped_onto_a_new_value() {
        let mut inner = Crossing::lossless(1);
        inner.note("/a", "rounded");
        let outer = inner.map(|n| n + 1);
        assert_eq!(outer.value, 2);
        assert_eq!(outer.notes().len(), 1);
    }
}
