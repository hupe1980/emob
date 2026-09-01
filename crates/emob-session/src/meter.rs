//! The meter readings a session leaves behind, and where each one came from.
//!
//! A charging station reports its energy register several times during a
//! session: once at the start, once at the end, periodically in between, and —
//! when configured for it — on every quarter-hour boundary of the clock. OCPP
//! calls that last kind `Sample.Clock`, and it is the one that matters most
//! here, because it is the only kind that can settle a quarter hour without
//! anybody having to guess.

use emob_core::{Direction, Energy};

/// Why a reading was taken.
///
/// The OCPP `ReadingContext` values, kept because the distinction decides how
/// much a reading is worth for settlement. `Sample.Clock` is a measurement of a
/// quarter-hour boundary; `Sample.Periodic` is a measurement of whenever the
/// station felt like it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ReadingContext {
    /// `Transaction.Begin` — the opening register value.
    TransactionBegin,
    /// `Transaction.End` — the closing register value.
    TransactionEnd,
    /// `Sample.Clock` — aligned to the clock, at `AlignedDataInterval`.
    SampleClock,
    /// `Sample.Periodic` — at `SampledDataTxUpdatedInterval`, from an arbitrary
    /// phase.
    SamplePeriodic,
    /// `Interruption.Begin` — charging stopped.
    InterruptionBegin,
    /// `Interruption.End` — charging resumed.
    InterruptionEnd,
    /// `Trigger` — the backend asked for it.
    Trigger,
    /// `Other`.
    Other,
}

impl ReadingContext {
    /// Whether this reading lands on a clock boundary by construction.
    ///
    /// Only `Sample.Clock` does. A periodic sample that happens to fall on
    /// `:15:00` is a coincidence, and settling a quarter hour on a coincidence
    /// is how two suppliers end up disagreeing by the same kilowatt-hour in
    /// opposite directions.
    #[must_use]
    pub const fn is_clock_aligned(self) -> bool {
        matches!(self, Self::SampleClock)
    }

    /// Whether this reading bounds the transaction.
    #[must_use]
    pub const fn is_transaction_boundary(self) -> bool {
        matches!(self, Self::TransactionBegin | Self::TransactionEnd)
    }

    /// The OCPP spelling, identical in 1.6, 2.0.1 and 2.1.
    ///
    /// Beside the variants rather than in the OCPP crate, because these *are*
    /// the OCPP names — the variant documentation gives them one line up — and a
    /// spelling kept somewhere else is a spelling that drifts from the enum it
    /// spells.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TransactionBegin => "Transaction.Begin",
            Self::TransactionEnd => "Transaction.End",
            Self::SampleClock => "Sample.Clock",
            Self::SamplePeriodic => "Sample.Periodic",
            Self::InterruptionBegin => "Interruption.Begin",
            Self::InterruptionEnd => "Interruption.End",
            Self::Trigger => "Trigger",
            Self::Other => "Other",
        }
    }
}

impl core::fmt::Display for ReadingContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One reading of a cumulative energy register.
///
/// Cumulative, not incremental: this is the register's own running total, the
/// quantity OCMF signs and the quantity a difference between two readings is
/// taken of. An incremental value cannot be checked against anything.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MeterReading {
    /// When the register held this value.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub at: time::OffsetDateTime,
    /// The register's value.
    pub register: Energy,
    /// Which way the energy this register counts was flowing.
    pub direction: Direction,
    /// Why this reading was taken.
    pub context: ReadingContext,
    /// Whether a signed record backs this reading.
    ///
    /// Set by the layer that has the evidence. A reading nobody signed may
    /// still be perfectly good telemetry; it may not be the basis of an
    /// invoice `[MessEG §33]`.
    pub signed: bool,
}

impl MeterReading {
    /// A reading, unsigned.
    #[must_use]
    pub const fn new(
        at: time::OffsetDateTime,
        register: Energy,
        direction: Direction,
        context: ReadingContext,
    ) -> Self {
        Self {
            at,
            register,
            direction,
            context,
            signed: false,
        }
    }

    /// The same reading, marked as backed by a signed record.
    #[must_use]
    pub const fn signed(mut self) -> Self {
        self.signed = true;
        self
    }
}

/// A session's readings for one direction, in time order.
///
/// One direction, because import and export are separate registers counting
/// separate quantities. Interleaving them into one series and taking
/// differences would net a V2G discharge against a draw, and both would leave
/// their supplier's balance group unaccounted for — the same rule
/// `mako-emob` enforces on the market side `[A6 §IV.1]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MeterSeries {
    direction: Direction,
    readings: Vec<MeterReading>,
}

impl MeterSeries {
    /// Build a series from readings, sorting them and checking their coherence.
    ///
    /// # Errors
    ///
    /// [`MeterError`] when the readings are empty, mix directions, or contain a
    /// register that runs backwards — the last being a fault to escalate, never
    /// a negative quantity to bill.
    pub fn new(direction: Direction, mut readings: Vec<MeterReading>) -> Result<Self, MeterError> {
        if readings.is_empty() {
            return Err(MeterError::Empty);
        }
        if let Some(wrong) = readings.iter().find(|r| r.direction != direction) {
            return Err(MeterError::MixedDirections {
                expected: direction,
                found: wrong.direction,
            });
        }

        readings.sort_by_key(|r| r.at);

        for pair in readings.windows(2) {
            if pair[1].register < pair[0].register {
                return Err(MeterError::RegisterRanBackwards {
                    at: pair[1].at,
                    from: pair[0].register,
                    to: pair[1].register,
                });
            }
        }

        Ok(Self {
            direction,
            readings,
        })
    }

    /// Which direction these readings count.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The readings, in time order.
    #[must_use]
    pub fn readings(&self) -> &[MeterReading] {
        &self.readings
    }

    /// The first reading.
    #[must_use]
    pub fn first(&self) -> &MeterReading {
        // `new` refuses an empty series.
        &self.readings[0]
    }

    /// The last reading.
    #[must_use]
    pub fn last(&self) -> &MeterReading {
        &self.readings[self.readings.len() - 1]
    }

    /// The total energy across the whole series.
    ///
    /// # Errors
    ///
    /// [`MeterError::RegisterRanBackwards`] cannot occur — `new` already
    /// rejected it — but the subtraction is fallible in the type system, so the
    /// error is surfaced rather than unwrapped.
    pub fn total(&self) -> Result<Energy, MeterError> {
        self.last()
            .register
            .difference_from(self.first().register)
            .map_err(|_| MeterError::RegisterRanBackwards {
                at: self.last().at,
                from: self.first().register,
                to: self.last().register,
            })
    }

    /// How many readings land on a clock boundary.
    ///
    /// The number that decides whether a quarter-hour settlement is measured or
    /// interpolated, so it is worth being able to ask for directly — an
    /// operator's answer to "why is this session's allocation marked
    /// interpolated?" usually starts here.
    #[must_use]
    pub fn clock_aligned_count(&self) -> usize {
        self.readings
            .iter()
            .filter(|r| r.context.is_clock_aligned())
            .count()
    }

    /// Whether every reading is backed by a signed record.
    #[must_use]
    pub fn fully_signed(&self) -> bool {
        self.readings.iter().all(|r| r.signed)
    }
}

/// What can be wrong with a series of readings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MeterError {
    /// No readings at all.
    #[error("a meter series needs at least one reading")]
    Empty,

    /// A reading counts the other direction.
    #[error("a {expected} series must not contain a {found} reading: the two never net")]
    MixedDirections {
        /// The series' direction.
        expected: Direction,
        /// The reading's direction.
        found: Direction,
    },

    /// The register decreased between two readings.
    #[error("the register ran backwards at {at}: {from} then {to}")]
    RegisterRanBackwards {
        /// When.
        at: time::OffsetDateTime,
        /// The earlier value.
        from: Energy,
        /// The later, smaller value.
        to: Energy,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use time::macros::datetime;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn reading(minute: u8, value: &str, context: ReadingContext) -> MeterReading {
        MeterReading::new(
            datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(i64::from(minute)),
            kwh(value),
            Direction::Import,
            context,
        )
    }

    #[test]
    fn readings_are_sorted_and_totalled() {
        let series = MeterSeries::new(
            Direction::Import,
            vec![
                reading(30, "30.000", ReadingContext::TransactionEnd),
                reading(0, "10.000", ReadingContext::TransactionBegin),
                reading(15, "20.000", ReadingContext::SampleClock),
            ],
        )
        .unwrap();

        assert_eq!(series.readings().len(), 3);
        assert_eq!(series.first().register, kwh("10.000"));
        assert_eq!(series.last().register, kwh("30.000"));
        assert_eq!(series.total().unwrap().to_string(), "20.000 kWh");
    }

    #[test]
    fn a_series_refuses_the_other_direction() {
        let mut export = reading(0, "1", ReadingContext::TransactionBegin);
        export.direction = Direction::Export;
        let err = MeterSeries::new(
            Direction::Import,
            vec![reading(0, "1", ReadingContext::TransactionBegin), export],
        )
        .unwrap_err();
        assert!(matches!(err, MeterError::MixedDirections { .. }));
        assert!(err.to_string().contains("never net"));
    }

    #[test]
    fn a_backwards_register_is_a_fault() {
        let err = MeterSeries::new(
            Direction::Import,
            vec![
                reading(0, "30.000", ReadingContext::TransactionBegin),
                reading(15, "10.000", ReadingContext::TransactionEnd),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, MeterError::RegisterRanBackwards { .. }));
    }

    #[test]
    fn an_empty_series_is_refused() {
        assert!(matches!(
            MeterSeries::new(Direction::Import, vec![]),
            Err(MeterError::Empty)
        ));
    }

    #[test]
    fn only_sample_clock_counts_as_aligned() {
        let series = MeterSeries::new(
            Direction::Import,
            vec![
                reading(0, "10", ReadingContext::TransactionBegin),
                reading(15, "20", ReadingContext::SampleClock),
                // Falls on a boundary by coincidence, which is not the same
                // thing as being aligned to one.
                reading(30, "25", ReadingContext::SamplePeriodic),
                reading(45, "30", ReadingContext::TransactionEnd),
            ],
        )
        .unwrap();
        assert_eq!(series.clock_aligned_count(), 1);
    }

    #[test]
    fn signing_is_tracked_per_reading() {
        let series = MeterSeries::new(
            Direction::Import,
            vec![
                reading(0, "10", ReadingContext::TransactionBegin).signed(),
                reading(15, "20", ReadingContext::TransactionEnd),
            ],
        )
        .unwrap();
        assert!(!series.fully_signed());
    }
}
