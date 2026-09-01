//! The OBIS code, read rather than carried.
//!
//! # What `01-00:B2.08.00*FF` says
//!
//! An OBIS code identifies *which quantity was read* `[OCMF Tab. 7, RI]`, and
//! OCMF reserves a range of them to say exactly what a charging session's
//! registers mean `[OCMF Tab. 25]`:
//!
//! | C field | Meaning |
//! |---|---|
//! | `B0` / `B1` | Total import — mains / device |
//! | `B2` / `B3` | **Transaction** import — mains / device |
//! | `C0` / `C1` | Total export — mains / device |
//! | `C2` / `C3` | **Transaction** export — mains / device |
//!
//! So the register itself states which way the energy flowed. A workspace whose
//! central claim is that import and export **never net** cannot afford to carry
//! that as an opaque string and take the direction from somewhere else: a
//! session recorded as a draw whose signed register says `C2` is a V2G
//! discharge being billed as consumption, and nothing downstream would notice.
//!
//! # And what it does not say
//!
//! Plenty of stations emit ordinary IEC 62056 codes — `1-b:1.8.0` is a KEBA in
//! the S.A.F.E. reference samples — where the C field is decimal `1` (positive
//! active energy, so import) or `2` (negative, so export), and nothing states
//! whether the register is per-transaction or lifetime.
//!
//! Everything else is simply **unknown**, and says so. An OBIS code this crate
//! cannot classify is not an import register by default; it is a register whose
//! direction the evidence does not state, and a caller that needs one has to
//! get it from elsewhere and know that it did.

use emob_core::Direction;

/// Where in the energy path a register measures `[OCMF Tab. 25]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MeasurementPoint {
    /// At the meter.
    Mains,
    /// At the consuming device — the vehicle — so after any cable-loss
    /// compensation.
    Device,
}

/// What a register counts over `[OCMF Tab. 25]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum RegisterScope {
    /// The lifetime of the meter.
    Total,
    /// One charging transaction, reset at its start.
    Transaction,
}

/// An OBIS code, kept as text and classified as far as it can be.
///
/// `Display` and equality are the code exactly as it arrived: two spellings of
/// one register are two different strings, and a chain whose opening and
/// closing readings disagree about the spelling is a chain worth looking at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ObisCode(String);

impl ObisCode {
    /// Read a code. Never fails — an unrecognised one is carried, not refused,
    /// because a manufacturer register this crate has never seen is still
    /// evidence.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The code as it arrived.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `C` field — the one that names the quantity.
    ///
    /// `01-00:B2.08.00*FF` → `B2`; `1-b:1.8.0` → `1`.
    fn value_group(&self) -> Option<&str> {
        let after_medium = self.0.rsplit(':').next()?;
        after_medium.split('.').next().filter(|s| !s.is_empty())
    }

    /// The `D` field — the processing. `8` is a time integral, i.e. an
    /// accumulating energy register.
    fn processing(&self) -> Option<&str> {
        let after_medium = self.0.rsplit(':').next()?;
        after_medium.split('.').nth(1)
    }

    /// Which way the energy this register counts was flowing, when the code
    /// states it.
    #[must_use]
    pub fn direction(&self) -> Option<Direction> {
        match self.value_group()?.to_ascii_uppercase().as_str() {
            // The OCMF-reserved range `[OCMF Tab. 25]`, and ordinary IEC 62056
            // where positive active energy is drawn and negative is fed back.
            // Two grammars, one meaning — which is why the arms are merged
            // rather than kept apart for the sake of a comment.
            "B0" | "B1" | "B2" | "B3" | "1" | "01" => Some(Direction::Import),
            "C0" | "C1" | "C2" | "C3" | "2" | "02" => Some(Direction::Export),
            _ => None,
        }
    }

    /// Whether the register counts one transaction or the meter's whole life,
    /// when the code states it.
    #[must_use]
    pub fn scope(&self) -> Option<RegisterScope> {
        match self.value_group()?.to_ascii_uppercase().as_str() {
            "B0" | "B1" | "C0" | "C1" => Some(RegisterScope::Total),
            "B2" | "B3" | "C2" | "C3" => Some(RegisterScope::Transaction),
            _ => None,
        }
    }

    /// Where in the energy path it measures, when the code states it.
    #[must_use]
    pub fn measurement_point(&self) -> Option<MeasurementPoint> {
        match self.value_group()?.to_ascii_uppercase().as_str() {
            "B0" | "B2" | "C0" | "C2" => Some(MeasurementPoint::Mains),
            "B1" | "B3" | "C1" | "C3" => Some(MeasurementPoint::Device),
            _ => None,
        }
    }

    /// Whether this is an accumulation register — a time integral, `D = 8`.
    ///
    /// `[OCMF Tab. 7, CL]` allows cable-loss compensation to be reported "only
    /// when RI is indicating an accumulation register reading", so this is the
    /// test that rule needs.
    #[must_use]
    pub fn is_accumulation_register(&self) -> bool {
        self.processing()
            .is_some_and(|d| d.trim_start_matches('0') == "8")
    }
}

impl core::fmt::Display for ObisCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ocmf_reserved_range_states_direction_scope_and_point() {
        let transaction_import = ObisCode::new("01-00:B2.08.00*FF");
        assert_eq!(transaction_import.direction(), Some(Direction::Import));
        assert_eq!(transaction_import.scope(), Some(RegisterScope::Transaction));
        assert_eq!(
            transaction_import.measurement_point(),
            Some(MeasurementPoint::Mains)
        );
        assert!(transaction_import.is_accumulation_register());

        // The one that matters: a register whose code says the energy left the
        // vehicle. Billing it as a draw is a V2G discharge charged as
        // consumption.
        let transaction_export = ObisCode::new("01-00:C2.08.00*FF");
        assert_eq!(transaction_export.direction(), Some(Direction::Export));
        assert_eq!(transaction_export.scope(), Some(RegisterScope::Transaction));

        let total_device_import = ObisCode::new("01-00:B1.08.00*FF");
        assert_eq!(total_device_import.scope(), Some(RegisterScope::Total));
        assert_eq!(
            total_device_import.measurement_point(),
            Some(MeasurementPoint::Device)
        );
    }

    #[test]
    fn ordinary_iec_codes_still_state_a_direction() {
        // `1-b:1.8.0` is a KEBA in the S.A.F.E. reference samples.
        let import = ObisCode::new("1-b:1.8.0");
        assert_eq!(import.direction(), Some(Direction::Import));
        assert_eq!(import.scope(), None, "and nothing about the scope");
        assert!(import.is_accumulation_register());

        let export = ObisCode::new("1-b:2.8.0");
        assert_eq!(export.direction(), Some(Direction::Export));

        assert_eq!(
            ObisCode::new("01-00:01.08.00*FF").direction(),
            Some(Direction::Import)
        );
    }

    #[test]
    fn an_unrecognised_code_is_carried_rather_than_guessed() {
        // A manufacturer register this crate has never seen is still evidence,
        // and it is *not* an import register by default.
        let unknown = ObisCode::new("01-00:99.07.00*FF");
        assert_eq!(unknown.direction(), None);
        assert_eq!(unknown.scope(), None);
        assert_eq!(unknown.measurement_point(), None);
        assert!(
            !unknown.is_accumulation_register(),
            "07 is not a time integral"
        );
        assert_eq!(unknown.to_string(), "01-00:99.07.00*FF");
    }

    #[test]
    fn the_code_survives_as_text() {
        for raw in ["01-00:B2.08.00*FF", "1-b:1.8.0", "nonsense", ""] {
            assert_eq!(ObisCode::new(raw).as_str(), raw);
        }
        assert_eq!(ObisCode::new("").direction(), None);
    }

    #[test]
    fn two_spellings_of_one_register_are_two_codes() {
        // Equality is the text. A chain whose opening and closing readings
        // disagree about the spelling is a chain worth looking at.
        assert_ne!(
            ObisCode::new("01-00:B2.08.00*FF"),
            ObisCode::new("1-0:B2.8.0")
        );
    }
}
