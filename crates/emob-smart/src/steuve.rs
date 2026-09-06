//! Which charge points `[EnWG §14a]` binds, and what their operator is owed
//! back.
//!
//! # The classification is a derivation, not a flag
//!
//! `[BK6-22-300 Ziff. 2.4.1]` states three conditions and every one of them is
//! a fact this workspace already holds about a point: whether it is publicly
//! accessible, what it is rated at, and which grid level it hangs off. So
//! [`SteuVe::of`] reads them rather than taking a boolean beside them — the same
//! reasoning that makes `ProviderProfile`'s four disclosures derived rather than
//! ticked, and for the same reason: a flag is a claim somebody can set, and this
//! one decides whether a grid operator may switch a charge park down.
//!
//! # …and the first condition is the one every summary drops
//!
//! > ein Ladepunkt für Elektromobile, **der kein öffentlich zugänglicher
//! > Ladepunkt im Sinne des § 2 Nr. 5 der Ladesäulenverordnung (LSV) ist**
//!
//! A **publicly accessible** charge point is not a steuerbare
//! `Verbrauchseinrichtung` at all. The fast charger on the forecourt is outside
//! `[EnWG §14a]`; the depot behind the fence, the workplace car park and the fleet yard
//! are inside it. Reading *"a charge park is a `SteuVe`"* the other way round —
//! which is the summary everybody writes — puts a public site under a regime it
//! is not in, and leaves the operator of a private one believing it may refuse a
//! control command it is obliged to obey.
//!
//! # …and charge points are counted one by one
//!
//! `[BK6-22-300 Ziff. 2.4.2]` lets several installations behind one connection
//! be treated as **one** `SteuVE` — and it names only the heat-pump and the
//! space-cooling groups, `2.4.1.b` and `2.4.1.c`. Charge points are not in that
//! sentence and neither is storage. So a yard with nine wall boxes is nine
//! steuerbare Verbrauchseinrichtungen, not one, and `n` in the Mindestleistung
//! formula is nine — which is worth a third of the power a site is owed.
//!
//! # Why 4.2 kW is 4.14 kW rounded up
//!
//! The threshold and the floor are the same number and it comes from a product
//! norm rather than from the grid. `DIN EN 61851-1` signals the charging current
//! over the pilot's PWM duty cycle and its lowest signallable value is **6 A**,
//! so a three-phase point cannot be told to draw less than
//! `3 × 230 V × 6 A = 4.14 kW`. The Bundesnetzagentur's first draft said 3.7 kW;
//! VDE FNN's consultation reply was that a point told to draw 3.7 kW would
//! **abort the session** and could not restart it by itself, and the figure was
//! raised to 4.2 kW for that reason and about charge points specifically.
//!
//! So [`SteuVe::technical_floor_kw`] is the same fact from the other side: what
//! this particular point can actually signal. A point that can drop to a single
//! phase reaches ~1.38 kW and a point that cannot stops at 4.14 kW — which is
//! `[BK6-22-300 Ziff. 4.6]`'s *"Reduzierung auf den nächstgeringeren Wert, der
//! technisch möglich ist"* with a number in it.

use emob_core::EvseId;
use emob_core::station::{ChargePointProfile, NotSteuVe};
use rust_decimal::Decimal;

/// The lowest current `DIN EN 61851-1` can signal over the control pilot, in
/// amperes.
///
/// Below this the duty cycle is not a charging current at all, so a point told
/// to draw less does not draw less — it stops.
const PILOT_FLOOR_AMPS: Decimal = Decimal::from_parts(6, 0, 0, false, 0);

/// The nominal phase voltage a European low-voltage connection is rated at.
///
/// A constant rather than a field, and the reason is that every figure in
/// `[BK6-22-300]` was computed with it: the 4.14 kW that became 4.2 kW is
/// `3 × 230 × 6`. A site on a different nominal voltage would be reasoning
/// against a different regulation.
const PHASE_VOLTS: Decimal = Decimal::from_parts(230, 0, 0, false, 0);

/// Watts in a kilowatt.
const W_PER_KW: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);

/// The connection capacity a point has to be over to be controllable, and the
/// Mindestleistung a device is owed — one number, and that is not a coincidence.
///
/// `emob_core::station::STEUVE_THRESHOLD_KW` is where it lives, because the
/// *conditions* belong beside the profile the obligation calendar judges and the
/// *consequences* belong here. Re-exported so a reader of this crate does not
/// have to discover which one holds it.
pub use emob_core::station::STEUVE_THRESHOLD_KW as THRESHOLD_KW;

/// A charge point `[EnWG §14a]` binds, and what it can physically be told.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SteuVe {
    evse_id: EvseId,
    connection_kw: Decimal,
    phases: u8,
    switches_to_single_phase: bool,
    control: Control,
}

/// Read back through [`SteuVe::of`] is not possible — the classification needs
/// a profile — so the fields are restored and re-checked against each other
/// instead.
///
/// The invariant that has to survive a store: a `SteuVe` with a connection at or
/// below the threshold is not one, and a phase count of zero has no floor at
/// all. Both would deserialise happily through a derive and then answer a
/// Mindestleistung question about a device the Festlegung does not reach.
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SteuVe {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        #[derive(serde::Deserialize)]
        struct AsStored {
            evse_id: EvseId,
            connection_kw: Decimal,
            phases: u8,
            switches_to_single_phase: bool,
            control: Control,
        }

        let stored = AsStored::deserialize(deserializer)?;
        if stored.connection_kw <= THRESHOLD_KW {
            return Err(D::Error::custom(NotSteuVe::AtOrBelowThreshold {
                evse_id: stored.evse_id.to_string(),
                capacity_kw: stored.connection_kw,
            }));
        }
        if stored.phases == 0 || stored.phases > 3 {
            return Err(D::Error::custom(format!(
                "{} states {} phases, and a charge point has one, two or three",
                stored.evse_id, stored.phases
            )));
        }
        Ok(Self {
            evse_id: stored.evse_id,
            connection_kw: stored.connection_kw,
            phases: stored.phases,
            switches_to_single_phase: stored.switches_to_single_phase,
            control: stored.control,
        })
    }
}

impl SteuVe {
    /// Classify a point, and remember how the grid operator reaches it.
    ///
    /// The three conditions are `emob_core`'s — they are read off the profile
    /// the obligation calendar judges, so a duty and a load manager cannot
    /// disagree about whether `[EnWG §14a]` binds a point. What this adds is
    /// `control`, which `[BK6-22-300 Ziff. 4.4]` makes a decision the operator
    /// states **per device** and which nothing about the point itself carries.
    ///
    /// # Errors
    ///
    /// [`NotSteuVe`] with the condition that failed — three answers rather than
    /// one, because they are three different situations.
    pub fn of(point: &ChargePointProfile, control: Control) -> Result<Self, NotSteuVe> {
        point.steuve()?;
        Ok(Self {
            evse_id: point.evse_id.clone(),
            connection_kw: point.grid.capacity_kw,
            phases: 3,
            switches_to_single_phase: false,
            control,
        })
    }

    /// The same point, wired on a stated number of phases.
    ///
    /// Three is the default because a point over 4.2 kW in Germany is three-phase
    /// unless somebody says otherwise, and because the figure the Festlegung
    /// rests on — 4.14 kW — is the three-phase one.
    #[must_use]
    pub const fn on_phases(mut self, phases: u8) -> Self {
        self.phases = phases;
        self
    }

    /// The same point, able to drop to a single phase mid-session.
    ///
    /// The option the revision of `DIN EN 61851-1` adds, and the one that moves
    /// a point's own floor from 4.14 kW to about 1.38 kW. A fact about the
    /// hardware, stated rather than assumed, because assuming it would have a
    /// site promise a grid operator a reduction its boxes cannot perform.
    #[must_use]
    pub const fn switching_to_single_phase(mut self) -> Self {
        self.switches_to_single_phase = true;
        self
    }

    /// Which point.
    #[must_use]
    pub const fn evse_id(&self) -> &EvseId {
        &self.evse_id
    }

    /// What it is connected at — the Netzanschlussleistung.
    #[must_use]
    pub const fn connection_kw(&self) -> Decimal {
        self.connection_kw
    }

    /// How the grid operator reaches it `[BK6-22-300 Ziff. 4.4]`.
    #[must_use]
    pub const fn control(&self) -> Control {
        self.control
    }

    /// The lowest power this point can be told to draw and still charge, in kW.
    ///
    /// `DIN EN 61851-1`'s pilot floor is 6 A, so the answer is
    /// `phases × 230 V × 6 A`, and a point that can drop to one phase reaches
    /// the single-phase figure whatever it is wired on. Below it there is no
    /// duty cycle that means *"charge slowly"* — the session stops, which is
    /// what `[BK6-22-300 Ziff. 4.6]` calls the next lower technically possible
    /// value and is why the allocation holds a point at **zero** rather than at
    /// a number it cannot signal.
    #[must_use]
    pub fn technical_floor_kw(&self) -> Decimal {
        let phases = if self.switches_to_single_phase {
            Decimal::ONE
        } else {
            Decimal::from(self.phases)
        };
        phases * PHASE_VOLTS * PILOT_FLOOR_AMPS / W_PER_KW
    }
}

/// How the grid operator's setpoint reaches a device `[BK6-22-300 Ziff. 4.4]`.
///
/// A decision the operator states **per** steuerbare Verbrauchseinrichtung, not
/// once per site: the Festlegung says *"Für jede steuerbare Verbrauchseinrichtung
/// hinter einem Netzanschluss hat der Betreiber … die Entscheidung zu treffen"*,
/// so a yard can have its two fast boxes wired directly and its six slow ones on
/// the energy-management system.
///
/// Which matters because the two are owed different minima, and because
/// `[BK6-22-300 Ziff. 4.5.2]`'s `n` counts *"alle steuerbaren
/// Verbrauchseinrichtungen, die nach Ziffer 4.4.b angesteuert werden"* — the
/// ones on the EMS, and not every device at the site. Reading it as every device
/// applies a simultaneity factor to a device that is not owed one, and the site
/// ends up claiming less than it may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Control {
    /// `4.4.a` — a setpoint bound to this one device.
    Direct,
    /// `4.4.b` — one setpoint for everything on the energy-management system,
    /// which the operator then spends as it sees fit.
    Ems,
}

/// The Gleichzeitigkeitsfaktor `[BK6-22-300 Ziff. 4.5.2]` tabulates.
///
/// | n | 2 | 3 | 4 | 5 | 6 | 7 | 8 | ≥ 9 |
/// |---|---|---|---|---|---|---|---|---|
/// | GZF | 0.8 | 0.75 | 0.7 | 0.65 | 0.6 | 0.55 | 0.5 | 0.45 |
///
/// `None` below two, where the formula's `(n − 1)` term is zero and the factor
/// is never read — an answer rather than a zero, because a zero would multiply
/// silently if the term ever moved.
///
/// # It is still this table, and that was worth checking
///
/// Ziffer 4.5.2 states the calculation as a presumption *"bis zum Inkrafttreten
/// einer anderweitigen Empfehlung"*, and VDE FNN published exactly such a
/// recommendation on this Tenorziffer in January 2025. It concluded the other
/// way: *"spricht sich VDE FNN dafür aus, ohne das Vorliegen praktischer
/// Erfahrungen, keine generelle Anpassung der Mindestleistung vorzunehmen."* So
/// the table below is the one in force, and the citation is to the Festlegung
/// rather than to the recommendation.
#[must_use]
pub fn simultaneity_factor(devices: usize) -> Option<Decimal> {
    let tenths = |value: u32| Decimal::from_parts(value, 0, 0, false, 2);
    match devices {
        0 | 1 => None,
        2 => Some(tenths(80)),
        3 => Some(tenths(75)),
        4 => Some(tenths(70)),
        5 => Some(tenths(65)),
        6 => Some(tenths(60)),
        7 => Some(tenths(55)),
        8 => Some(tenths(50)),
        _ => Some(tenths(45)),
    }
}

/// The minimum a site is owed, and the rule that produced it.
///
/// Carried together because the figure alone cannot be checked: 7.56 kW is right
/// for two devices on an energy-management system and wrong for anything else,
/// and an operator disputing a setpoint is disputing the rule as much as the
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Minimum {
    /// The Mindestleistung, in kW.
    pub kw: Decimal,
    /// Which sentence of `[BK6-22-300 Ziff. 4.5]` produced it.
    pub rule: MinimumRule,
}

/// Which limb of `[BK6-22-300 Ziff. 4.5]` a minimum came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MinimumRule {
    /// `4.5.1` — one directly controlled device, owed 4.2 kW on its own.
    ///
    /// The sentence's second half scales a device over 11 kW by 0.4, and it
    /// names `2.4.1.b` and `2.4.1.c` — heat pumps and space cooling. A charge
    /// point is `2.4.1.a` and is not in it, so a 22 kW box is owed 4.2 kW and
    /// not 8.8.
    PerDevice,
    /// `4.5.2` — one figure for everything on the energy-management system.
    EmsGroup {
        /// How many devices are on it — `n` in the formula, counting only the
        /// ones controlled that way.
        devices: usize,
        /// The Gleichzeitigkeitsfaktor that applied, or `None` for a group of
        /// one where the term is zero.
        factor: Option<Decimal>,
    },
    /// Nothing at this site is controllable, so nothing is owed and nothing may
    /// be curtailed under this Festlegung either.
    NotControllable,
}

/// The Mindestleistung a set of devices is owed `[BK6-22-300 Ziff. 4.5]`.
///
/// # Two rules, and mixing them is the ordinary case
///
/// A directly controlled device is owed 4.2 kW **on its own**, and the grid
/// operator addresses it on its own; the devices on the energy-management system
/// share one figure the operator then spends as it likes. A site with both is
/// owed the sum, and the two halves are computed by different sentences.
///
/// `n` in the EMS formula counts **only** the devices controlled that way — the
/// Festlegung says so in as many words — so a yard with two direct boxes and six
/// on the EMS has `n = 6`, not eight.
///
/// And the direction that mistake falls in is the one that hurts: treating all
/// eight as one EMS group gives `4.2 + 7 × 0.5 × 4.2 = 18.9 kW`, where the two
/// halves computed properly come to `8.4 + 16.8 = 25.2`. **Six and a third
/// kilowatts under-claimed**, because a directly controlled device is owed its
/// 4.2 kW whole and no simultaneity factor is applied to it. An operator that
/// under-claims accepts a curtailment deeper than the Festlegung permits, and
/// nothing anywhere reports that it did.
#[must_use]
pub fn minimum_for(devices: &[SteuVe]) -> Minimum {
    let direct = devices
        .iter()
        .filter(|device| device.control == Control::Direct)
        .count();
    let on_ems = devices
        .iter()
        .filter(|device| device.control == Control::Ems)
        .count();

    if direct == 0 && on_ems == 0 {
        return Minimum {
            kw: Decimal::ZERO,
            rule: MinimumRule::NotControllable,
        };
    }

    // 4.5.1 — each directly controlled device, on its own.
    let per_device = Decimal::from(u64::try_from(direct).unwrap_or(u64::MAX)) * THRESHOLD_KW;

    // 4.5.2 — `Pmin = 4.2 kW + (n − 1) × GZF × 4.2 kW`. The other limb of the
    // formula adds `Max(0.4 × ΣWP; 0.4 × ΣKlima)` in place of the leading
    // 4.2 kW, and it applies where a heat pump or a cooling installation over
    // 11 kW is part of the same EMS group — neither of which is a charge point,
    // and neither of which this crate models. A site with one has a figure this
    // function under-states, which is the direction an under-claim falls in.
    let factor = simultaneity_factor(on_ems);
    let ems = if on_ems == 0 {
        Decimal::ZERO
    } else {
        let others = Decimal::from(u64::try_from(on_ems - 1).unwrap_or(u64::MAX));
        THRESHOLD_KW + others * factor.unwrap_or(Decimal::ZERO) * THRESHOLD_KW
    };

    Minimum {
        kw: per_device + ems,
        rule: if on_ems == 0 {
            MinimumRule::PerDevice
        } else {
            MinimumRule::EmsGroup {
                devices: on_ems,
                factor,
            }
        },
    }
}
