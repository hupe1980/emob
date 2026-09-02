//! What an OCPI crossing cost, said out loud.
//!
//! The type is [`emob_core::Crossing`] — the same account three seams in this
//! workspace return, because a partner asking why their total differs and an
//! operator asking why a charge point's screen cannot show a fee are asking one
//! question. This module adds the one thing that is OCPI's alone.
//!
//! # It composes with the kit's own account
//!
//! `ocpi-kit` already reports what a **version** crossing costs — a 2.3.0
//! object downgraded to 2.2.1 loses the fields 2.2.1 has no room for, and
//! [`Lossy`] names each one by JSON Pointer. That is the same shape as a
//! [`Crossing`]'s account, for the layer below, so [`AbsorbLossy::absorb`]
//! folds one into the other and a partner on 2.2.1 gets one report rather than
//! two half-reports that have to be read together to mean anything.
//!
//! `ocpi-kit`'s pointers are into its **source** object, which is right for a
//! downgrade — the field that vanished has no target to point at — and is why
//! the fold prefixes them with the object they came from rather than pretending
//! they address the document the partner holds.

pub use emob_core::crossing::{Crossing, Note};
use ocpi_kit::convert::Lossy;

/// Folding `ocpi-kit`'s version-downgrade account into a [`Crossing`]'s.
///
/// An extension trait rather than an inherent method, because [`Crossing`] is
/// workspace vocabulary and [`Lossy`] is one wire's.
pub trait AbsorbLossy {
    /// Fold in what a **version** crossing cost, as `ocpi-kit` reported it.
    ///
    /// `prefix` is prepended to each pointer so the merged report says which
    /// object the loss was in — a page of CDRs downgraded together otherwise
    /// produces twenty notes all pointing at `/total_cost`.
    fn absorb(&mut self, prefix: &str, lossy: &Lossy);
}

impl<T> AbsorbLossy for Crossing<T> {
    fn absorb(&mut self, prefix: &str, lossy: &Lossy) {
        self.absorb_notes(
            prefix,
            lossy
                .into_iter()
                .map(|loss| Note::new(loss.pointer.clone(), loss.reason.clone())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
