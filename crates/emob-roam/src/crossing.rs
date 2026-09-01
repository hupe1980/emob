//! What a crossing cost, said out loud.
//!
//! A canonical record carried onto a wire is not a re-encoding. The wire has
//! its own model, and where the two models disagree something has to give:
//! a quantity is rounded, a distinction is collapsed, a fact has no field to
//! live in. A `From` impl makes those decisions silently, once, at the moment
//! nobody is looking — and the consequence surfaces weeks later as two
//! companies holding two different numbers for one session.
//!
//! So every translation in this crate returns a [`Crossing`]: the value, and
//! the account. The account is not decoration. It is the answer to *"why does
//! your total differ from mine"*, and it is the only artefact either party
//! has when the session is six weeks old.
//!
//! # It composes with the kit's own account
//!
//! `ocpi-kit` already reports what a **version** crossing costs — a 2.3.0
//! object downgraded to 2.2.1 loses the fields 2.2.1 has no room for, and
//! [`Lossy`] names each one by JSON Pointer. That is the same shape as this,
//! for the layer below, so [`Crossing::absorb`] folds one into the other and a
//! partner on 2.2.1 gets one report rather than two half-reports that have to
//! be read together to mean anything.

use core::fmt;

use ocpi_kit::convert::Lossy;

/// One thing a crossing could not carry, or could carry only approximately.
///
/// The pointer is RFC 6901 into the **target** document — the thing the
/// partner will be looking at when they ask. `ocpi-kit`'s [`Lossy`] points
/// into its source object instead, which is right for a downgrade (the field
/// that vanished has no target to point at) and wrong here (the field exists;
/// what is in it is approximate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// RFC 6901 JSON Pointer to the value the note is about.
    pub pointer: String,
    /// What happened to it, and why.
    pub reason: String,
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
        self.notes.push(Note {
            pointer: pointer.into(),
            reason: reason.into(),
        });
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
    /// only record of what the partner's copy does not say, and the name is
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

    /// Fold in what a **version** crossing cost, as `ocpi-kit` reported it.
    ///
    /// `prefix` is prepended to each pointer so the merged report says which
    /// object the loss was in — a page of CDRs downgraded together otherwise
    /// produces twenty notes all pointing at `/total_cost`.
    pub fn absorb(&mut self, prefix: &str, lossy: &Lossy) {
        for loss in lossy {
            self.notes.push(Note {
                pointer: format!("{prefix}{}", loss.pointer),
                reason: loss.reason.clone(),
            });
        }
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
    fn a_note_names_the_field_a_partner_will_be_looking_at() {
        let mut crossing = Crossing::lossless(42);
        crossing.note("/total_time", "0.3333 h is 20 minutes rounded");
        assert_eq!(
            crossing.reasons().next().unwrap(),
            "/total_time: 0.3333 h is 20 minutes rounded"
        );
    }

    #[test]
    fn an_empty_pointer_reads_as_the_whole_document() {
        let note = Note {
            pointer: String::new(),
            reason: "the record itself".to_owned(),
        };
        assert_eq!(note.to_string(), "/: the record itself");
    }

    #[test]
    fn the_version_crossings_account_merges_into_this_one() {
        // A 2.2.1 partner gets one report, not two half-reports.
        let mut multi = ocpi_kit::v2_3_0::Price::new("5.00".parse().unwrap());
        multi
            .taxes
            .push(ocpi_kit::v2_3_0::TaxAmount::new("GST", None, "0.25".parse().unwrap()).unwrap());
        multi
            .taxes
            .push(ocpi_kit::v2_3_0::TaxAmount::new("QST", None, "0.50".parse().unwrap()).unwrap());
        let downgraded = ocpi_kit::convert::Downgrade::<ocpi_kit::v2_2_1::Price>::downgrade(multi);
        assert!(!downgraded.lossy.is_empty());

        let mut crossing = Crossing::lossless(());
        crossing.absorb("/total_cost", &downgraded.lossy);
        assert!(!crossing.is_lossless());
        assert!(
            crossing.notes()[0].pointer.starts_with("/total_cost"),
            "a merged note has to say which object it came from: {}",
            crossing.notes()[0]
        );
    }
}
