//! The things that go wrong, and how often.
//!
//! # Why a simulator that only succeeds is not a simulator
//!
//! A fleet run in which every session bills proves that the happy path
//! compiles. What a platform is actually judged on is the other days: the
//! meter that substituted a value, the record that never arrived, the clock
//! nobody synchronised, the cabinet wired to the wrong register. Each of those
//! has a rule somewhere in this workspace that is supposed to catch it, and a
//! rule nothing exercises is a rule that quietly stops holding.
//!
//! So a reference day carries **seeded faults**, and the run asserts not that
//! everything billed but that **everything either billed or was refused with a
//! reason** — which is the property that actually matters, and the one a
//! silent failure breaks.

use crate::rng::Rng;

/// Something that goes wrong between a meter and an invoice.
///
/// Each variant names a rule this workspace enforces somewhere, so a reference
/// day that injects it is exercising that rule against a genuinely signed
/// session rather than a hand-built fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Fault {
    /// A reading reports `ST=S`: the meter invented a value it could not
    /// measure `[OCMF Tab. 10]`.
    SubstituteReading,
    /// A record never reaches the backend, leaving a hole in the pagination
    /// that every remaining signature still verifies across.
    DroppedRecord,
    /// A payload byte changes after signing.
    TamperedValue,
    /// The station's clock is unsynchronised `[OCMF Tab. 19]`, so the energy
    /// bills and the duration does not.
    UnsynchronisedClock,
    /// The register the station signs is an **export** one `[OCMF Tab. 25]`
    /// while the session claims a draw.
    WrongDirectionRegister,
    /// A reading marks `TX=X`: time and energy are unusable from there on
    /// `[OCMF Tab. 7, TX]`.
    ExceptionDuringCharging,
    /// The station is not in the key registry — a provisioning gap, which is
    /// the most common cause of an unbillable session in the field.
    UnregisteredStation,
    /// The post is offered a tariff `[AFIR Art. 5(4)]` does not permit at its
    /// power: an occupancy fee per minute and no price per kWh, on a charger of
    /// 50 kW or more.
    ///
    /// The one fault in the catalogue that is nothing to do with the meter. The
    /// energy is measured perfectly, every signature holds, and the session must
    /// still not be priced — because the tariff is one the operator may not
    /// offer. A fleet that only ever injects metering faults never runs the
    /// shape gate, and a gate nothing exercises is a gate that quietly stops
    /// holding.
    UnlawfulTariff,
}

impl Fault {
    /// Every fault, for a day that wants the whole catalogue.
    pub const ALL: &'static [Self] = &[
        Self::SubstituteReading,
        Self::DroppedRecord,
        Self::TamperedValue,
        Self::UnsynchronisedClock,
        Self::WrongDirectionRegister,
        Self::ExceptionDuringCharging,
        Self::UnregisteredStation,
        Self::UnlawfulTariff,
    ];

    /// Whether this fault stops the session's **energy** from billing.
    ///
    /// The distinction the whole Eichrecht chain is built on: an
    /// unsynchronised clock leaves a register an invoice may use, and a
    /// substitute value does not.
    #[must_use]
    pub const fn blocks_energy(self) -> bool {
        !matches!(self, Self::UnsynchronisedClock)
    }

    /// A short name for a report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubstituteReading => "substitute reading",
            Self::DroppedRecord => "dropped record",
            Self::TamperedValue => "tampered value",
            Self::UnsynchronisedClock => "unsynchronised clock",
            Self::WrongDirectionRegister => "wrong-direction register",
            Self::ExceptionDuringCharging => "exception during charging",
            Self::UnregisteredStation => "unregistered station",
            Self::UnlawfulTariff => "unlawful tariff",
        }
    }
}

impl core::fmt::Display for Fault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How often a fault is injected: one session in `n`.
///
/// `n == 0` never fires, which is how a day is built fault-free without a
/// second code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rate(pub u64);

impl Rate {
    /// Never.
    pub const NEVER: Self = Self(0);
    /// Every session.
    pub const ALWAYS: Self = Self(1);

    /// One session in `n`.
    #[must_use]
    pub const fn one_in(n: u64) -> Self {
        Self(n)
    }

    /// Whether this fault fires for the session the stream is currently on.
    pub fn fires(self, rng: &mut Rng) -> bool {
        rng.one_in(self.0)
    }
}

/// Which faults a reference day injects, and how often.
#[derive(Debug, Clone, Default)]
pub struct FaultPlan {
    rates: Vec<(Fault, Rate)>,
}

impl FaultPlan {
    /// A day in which nothing goes wrong.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A day carrying the whole catalogue at one rate each.
    ///
    /// The default a fleet run should use: it is the only setting under which
    /// *every* rule in the chain is exercised, and a run that exercises only
    /// the rules somebody remembered to list is a run that drifts.
    #[must_use]
    pub fn everything(rate: Rate) -> Self {
        Self {
            rates: Fault::ALL.iter().map(|f| (*f, rate)).collect(),
        }
    }

    /// Add or replace one fault's rate.
    #[must_use]
    pub fn with(mut self, fault: Fault, rate: Rate) -> Self {
        self.rates.retain(|(f, _)| *f != fault);
        self.rates.push((fault, rate));
        self.rates.sort_unstable();
        self
    }

    /// The faults this session draws.
    ///
    /// Drawn in a fixed order so that adding a fault to the catalogue does not
    /// reshuffle the ones already there.
    #[must_use]
    pub fn draw(&self, rng: &mut Rng) -> Vec<Fault> {
        self.rates
            .iter()
            .filter(|(_, rate)| rate.fires(rng))
            .map(|(fault, _)| *fault)
            .collect()
    }

    /// Whether any fault can fire at all.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.rates.iter().all(|(_, rate)| rate.0 == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_plan_draws_nothing() {
        let plan = FaultPlan::none();
        let mut rng = Rng::new(1);
        assert!(plan.is_quiet());
        assert!(plan.draw(&mut rng).is_empty());

        let never = FaultPlan::everything(Rate::NEVER);
        assert!(never.is_quiet());
        assert!(never.draw(&mut rng).is_empty());
    }

    #[test]
    fn every_fault_fires_at_rate_one() {
        let plan = FaultPlan::everything(Rate::ALWAYS);
        let mut rng = Rng::new(1);
        assert_eq!(plan.draw(&mut rng).len(), Fault::ALL.len());
    }

    #[test]
    fn a_rate_replaces_rather_than_stacking() {
        let plan = FaultPlan::none()
            .with(Fault::TamperedValue, Rate::ALWAYS)
            .with(Fault::TamperedValue, Rate::NEVER);
        let mut rng = Rng::new(1);
        assert!(plan.draw(&mut rng).is_empty());
    }

    #[test]
    fn only_the_clock_fault_leaves_the_energy_billable() {
        // The distinction the whole chain is built on, asserted over the
        // catalogue so a new fault has to declare which side it is on.
        for fault in Fault::ALL {
            assert_eq!(
                fault.blocks_energy(),
                *fault != Fault::UnsynchronisedClock,
                "{fault}"
            );
        }
    }

    #[test]
    fn a_draw_is_reproducible_from_its_seed() {
        let plan = FaultPlan::everything(Rate::one_in(3));
        let one = plan.draw(&mut Rng::new(1234));
        let two = plan.draw(&mut Rng::new(1234));
        assert_eq!(one, two);
    }
}
