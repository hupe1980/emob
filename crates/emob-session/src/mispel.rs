//! The `MiSpeL` month: how much of a bidirectional charging site's grid draw is
//! relieved of levies, under the Bundesnetzagentur's Festlegung 618-25-02 of
//! 01.10.2026.
//!
//! # What it decides
//!
//! A bidirectional point draws energy from the grid and feeds some of it back.
//! The part fed back out of a "store" — and a bidirectional point is one,
//! `[MiSpeL A1 3.2.5]` — is *saldierungsfähig*: the levies on that much of the
//! site's grid draw fall to zero `[MiSpeL Tenor 1]`. Everything else drawn
//! stays levied, including what the cars drove away with. Get it too large and
//! the site keeps money the levies were owed; get it too small and the site
//! pays twice.
//!
//! | Option | Period | Here |
//! |---|---|---|
//! | **Abgrenzungsoption**, Anlage 1 | calendar month or Rumpfmonat | [`month`]: Basisfall A2 (Z1 + Z2, formulas (1)–(21)) or Sonderfall A11 (Z1 only) |
//! | **Pauschaloption**, Anlage 2 | calendar year or Rumpfjahr | [`pauschal_year`]: Basisfall P2, factor (P2)P2 = 0,2 |
//!
//! Only the **levy** side. The förderfähige Netzeinspeisung, formulas
//! (23)–(33) and (P12)–(P15), is the market premium of an EE plant's operator,
//! and without a premium-supported plant behind the connection the Festlegung
//! leaves it "unberücksichtigt" `[MiSpeL A1 10.1.2]`. A stationary store
//! (Basisfälle A1, A3, A4) is not modelled here.
//!
//! Citations are `[MiSpeL A1 (n)]` for formula n of Anlage 1 (*Abgrenzungsoption*),
//! `[MiSpeL A2 (Pn)]` for Anlage 2 (*Pauschaloption*), sections as
//! `[MiSpeL A1 3.2.5]`, and `[MiSpeL Tenor n]` for a Tenorziffer (Amtsblatt
//! 19/2026, Vfg. Nr. 73/2026). The corpus is the sibling `hems` workspace's.
//!
//! # Where the numbers come from
//!
//! - **Z1** — the grid meter at the connection, `Z1NB¼` drawn and `Z1NE¼` fed
//!   in. An input: the Messstellenbetreiber's values, never derived from
//!   sessions. It must hold **one value for every quarter hour** of the
//!   governed window, or the month is refused — a missing quarter hour of draw
//!   is levied draw that vanishes.
//! - **Z2** — the meter behind the bidirectional points. Several points at the
//!   place of one are metered as one `[MiSpeL A1 3.2.6]`, so `Z2V¼` and `Z2E¼`
//!   are the sums of the covered points' [`SessionSplit`] slots by
//!   [`Direction`]. Both directions stay apart until the formula takes the
//!   minimum: nothing here nets a draw against a discharge before the
//!   Festlegung does.
//!
//! A unidirectional point is sonstiger Verbrauch and stays out of Z2
//! `[MiSpeL A1 3.2.5]`. A discharge at a point the declared Messkonzept does
//! not cover is **refused by name** — the Formelsatz must capture all of the
//! site's quantities, and a discharge it cannot see is one it cannot net.
//!
//! # When the rules govern
//!
//! [`governed`] turns the Tenor into the one window a settlement may cover:
//! effect with the Bekanntgabe on 21.10.2026 `[MiSpeL Tenor 8]`; until
//! 30.09.2027 only with the grid and metering operators' consent, which is an
//! explicit, declared input `[MiSpeL Tenor 9]` a); the Pauschaloption only from
//! the first day of the month after the European Commission's state-aid
//! approval, and not at all while there is none `[MiSpeL Tenor 9]` b). An
//! installation already assigned to a Veräußerungsform switches only on the
//! first of a month; a first assignment may start mid-month, and the rest of
//! that month is a **Rumpfmonat**, which "tritt … an die Stelle des
//! Kalendermonats" `[MiSpeL A1 11]`.
//!
//! The calendar is German legal time: a month's quarter hours are those that
//! start on a Europe/Berlin day of the window, so the 92- and 100-quarter days
//! of a clock change are counted, not assumed.

use std::collections::{BTreeMap, BTreeSet};

use emob_core::{Direction, EvseId, Formelsatz, QuarterHour, TimeZone};
use rust_decimal::Decimal;
use time::macros::date;
use time::{Date, Month as CalendarMonth};

use crate::split::{Provenance, SessionSplit};

/// The day the Festlegung takes effect: its Bekanntgabe, two weeks after the
/// Amtsblatt 19/2026 of 07.10.2026 `[MiSpeL Tenor 8]`.
pub const IN_EFFECT: Date = date!(2026 - 10 - 21);

/// The last day on which the rules apply only with the consent of the grid
/// and metering operators `[MiSpeL Tenor 9]` a).
pub const CONSENT_REQUIRED_UNTIL: Date = date!(2027 - 09 - 30);

/// The storage efficiency presumed for a charge point, for want of a meter
/// that could measure one: 85 % `[MiSpeL A1 (14)]` (A2, A3, A4).
pub const CHARGE_POINT_EFFICIENCY: Decimal = Decimal::from_parts(85, 0, 0, false, 2);

/// The Rechengröße of the Pauschaloption for a charge point without a store:
/// 0,2 `[MiSpeL A2 (P2)]` (P2). The working draft of 05.08.2026 said 0,5.
pub const PAUSCHAL_FACTOR_CHARGE_POINT: Decimal = Decimal::from_parts(2, 0, 0, false, 1);

/// Feed-in per kilowatt of installed solar power the Pauschaloption treats as
/// supportable each year: 500 kWh/kW `[MiSpeL A2 (P1)]`.
pub const PAUSCHAL_KWH_PER_KWP: Decimal = Decimal::from_parts(500, 0, 0, false, 0);

/// The largest solar installation the Pauschaloption is open to, kWp
/// `[MiSpeL A2 4.1.2]`.
pub const PAUSCHAL_MAX_KWP: Decimal = Decimal::from_parts(30, 0, 0, false, 0);

/// Days of the summer period (April–September) in every year `[MiSpeL A2 (P17)]`.
const SUMMER_DAYS: u16 = 183;

/// Which option a period is settled under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Anlage 1 — per calendar month.
    Abgrenzung,
    /// Anlage 2 — per calendar year, from the state-aid month.
    Pauschal,
}

/// What the operator declares rather than measures.
///
/// Each one changes what the rules allow, so each is an argument: nothing here
/// assumes consent, an approval or an assignment that was not stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conditions {
    /// The first day the installation is assigned on the option's basis
    /// `[MiSpeL Tenor 5]`.
    pub assigned_from: Date,
    /// Whether this is the **first** assignment of a newly commissioned
    /// installation — the only one that may start mid-month `[MiSpeL A1 11]`.
    pub first_assignment: bool,
    /// Whether the grid and metering operators consented `[MiSpeL Tenor 9]` a).
    pub operator_consent: bool,
    /// The day the European Commission approved § 19 Abs. 3c EEG under state-aid
    /// law, if it has `[MiSpeL Tenor 9]` b).
    pub state_aid_approved_on: Option<Date>,
}

/// The part of a period the rules govern, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Governed {
    /// The first governed day.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub first: Date,
    /// The last governed day.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub last: Date,
    /// Whether the window starts inside the period — a Rumpfmonat
    /// `[MiSpeL A1 11]` or Rumpfjahr `[MiSpeL A2 9]`.
    pub rumpf: bool,
}

/// Why a period is not netted. Each names the rule it rests on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refusal {
    /// The period ends before the Festlegung took effect.
    #[error("the period ends before the Festlegung took effect on {IN_EFFECT} (Tenorziffer 8)")]
    BeforeTheRules,
    /// The period ends before the installation was assigned.
    #[error("the period ends before the installation was assigned on {assigned_from}")]
    BeforeTheAssignment {
        /// The assignment date.
        assigned_from: Date,
    },
    /// A switch of an installation already assigned, not on the first of a month.
    #[error(
        "an installation already assigned to a Veräußerungsform switches only on the first day of \
         a month (Anlage 1 § 11, § 21b Abs. 1 S. 2 EEG), not on {assigned_from}"
    )]
    MidMonthSwitch {
        /// The assignment date.
        assigned_from: Date,
    },
    /// Before 01.10.2027 without the operators' consent.
    #[error(
        "until {CONSENT_REQUIRED_UNTIL} the Festlegung applies only with the consent of the grid \
         and metering operators (Tenorziffer 9 a), and none is declared"
    )]
    NoConsent,
    /// The Pauschaloption with no state-aid approval.
    #[error(
        "the Pauschaloption applies only from the month after the European Commission's state-aid \
         approval of § 19 Abs. 3c EEG (Tenorziffer 9 b), and no approval date is known"
    )]
    NoStateAid,
    /// The Pauschaloption before the month after approval.
    #[error("the Pauschaloption applies from {applicable_from} (Tenorziffer 9 b)")]
    BeforeStateAid {
        /// The first applicable day.
        applicable_from: Date,
    },
    /// A discharge at a point the declared Messkonzept does not cover.
    #[error(
        "{point} fed back in the quarter hour {quarter}, and the declared Messkonzept does not \
         cover it (Anlage 1 § 3.2.5): a discharge no approved Formelsatz sees is not netted"
    )]
    UncoveredDischarge {
        /// The point.
        point: EvseId,
        /// The quarter hour.
        quarter: QuarterHour,
    },
    /// A covered slot whose energy was interpolated, not measured.
    #[error(
        "{point}'s energy in the quarter hour {quarter} was interpolated, and the Festlegung nets \
         quarter-hour measurements (Anlage 1 § 3.2.1)"
    )]
    NotMeasured {
        /// The point.
        point: EvseId,
        /// The quarter hour.
        quarter: QuarterHour,
    },
    /// The grid series holds a quarter hour twice.
    #[error("the grid meter series holds the quarter hour {0} twice")]
    GridDuplicate(QuarterHour),
    /// The grid series does not cover the governed window.
    #[error(
        "the grid meter series holds {received} of the {expected} quarter hours from {first} to \
         {last}; a missing quarter hour of draw is levied draw that vanishes"
    )]
    GridIncomplete {
        /// Quarter hours the window has.
        expected: usize,
        /// Quarter hours the series supplied for it.
        received: usize,
        /// The window's first day.
        first: Date,
        /// The window's last day.
        last: Date,
    },
    /// The Pauschaloption without a solar plant.
    #[error(
        "the Pauschaloption needs a solar installation behind the connection (Anlage 2 § 2.1.1)"
    )]
    NoSolar,
    /// The Pauschaloption over 30 kWp.
    #[error(
        "the Pauschaloption is open to solar of at most 30 kWp, not {0} kWp (Anlage 2 § 4.1.2)"
    )]
    SolarTooLarge(Decimal),
}

/// The part of `[first, last]` the rules govern for one installation.
///
/// # Errors
///
/// The [`Refusal`] naming the condition that leaves nothing of the period
/// governed, or the switch the text does not permit.
pub fn governed(
    choice: Choice,
    conditions: &Conditions,
    first: Date,
    last: Date,
) -> Result<Governed, Refusal> {
    if IN_EFFECT > last {
        return Err(Refusal::BeforeTheRules);
    }
    let assigned_from = conditions.assigned_from;
    if !conditions.first_assignment && assigned_from.day() != 1 {
        return Err(Refusal::MidMonthSwitch { assigned_from });
    }
    if assigned_from > last {
        return Err(Refusal::BeforeTheAssignment { assigned_from });
    }
    let mut start = first.max(IN_EFFECT).max(assigned_from);

    if !conditions.operator_consent {
        if CONSENT_REQUIRED_UNTIL >= last {
            return Err(Refusal::NoConsent);
        }
        start = start.max(CONSENT_REQUIRED_UNTIL.next_day().unwrap_or(last));
    }

    if choice == Choice::Pauschal {
        let approved = conditions
            .state_aid_approved_on
            .ok_or(Refusal::NoStateAid)?;
        let applicable_from = first_of_next_month(approved);
        if applicable_from > last {
            return Err(Refusal::BeforeStateAid { applicable_from });
        }
        start = start.max(applicable_from);
    }

    Ok(Governed {
        first: start,
        last,
        rumpf: start > first,
    })
}

/// One quarter hour of the grid meter Z1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridQuarter {
    /// The quarter hour, named by its start.
    pub quarter: QuarterHour,
    /// `Z1NB¼` — drawn from the grid, kWh, non-negative.
    pub draw: emob_core::Energy,
    /// `Z1NE¼` — fed into the grid, kWh, non-negative.
    pub feed_in: emob_core::Energy,
    /// Whether the spot price `SP¼` was negative — read by the Pauschaloption
    /// only `[MiSpeL A2 (P5)]`.
    pub spot_negative: bool,
}

/// The site as its Messkonzept declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The Formelsatz with its Messkonzept.
    pub formelsatz: Formelsatz,
    /// The bidirectional points the Messkonzept covers — behind Z2 under A2,
    /// behind Z1 under A11.
    pub covered: BTreeSet<EvseId>,
}

/// The figures Basisfall A2 adds to a month, numbered as in Anlage 1.
///
/// (17)–(19) are absent on purpose: storage losses are not privilegeable with
/// a charge point, so (19)A2 = 0 `[MiSpeL A1 (19)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct A2Figures {
    /// `(5)` consumption at the points, kWh.
    pub device_consumption: Decimal,
    /// `(6)` generation at the points, kWh.
    pub device_generation: Decimal,
    /// `(9)` grid electricity the points took in the same quarter hour.
    pub grid_charged: Decimal,
    /// `(10)` electricity from behind the connection the points took.
    pub plant_charged: Decimal,
    /// `(11)` feed-in out of the points, quarter hour by quarter hour.
    pub device_feed_in_base: Decimal,
    /// `(12)` **Fremdtankstrom** — energy a car brought from elsewhere.
    pub foreign_charge: Decimal,
    /// `(13)` feed-in out of the points that may be considered.
    pub device_feed_in_considered: Decimal,
    /// `(14)` the presumed efficiency, 0,85.
    pub efficiency: Decimal,
    /// `(15)` the part of the points' output that came from behind the
    /// connection rather than from the grid.
    pub renewable_storage_output: Decimal,
}

/// One month — or Rumpfmonat — under the Abgrenzungsoption.
///
/// Every figure is kept, numbered as in the Festlegung, because a Nachweis is
/// checked by somebody re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Month {
    /// The window netted.
    pub governed: Governed,
    /// The Formelsatz it was netted under.
    pub formelsatz: Formelsatz,
    /// `(3)` total grid draw, kWh.
    pub grid_draw: Decimal,
    /// `(4)` total grid feed-in, kWh.
    pub grid_feed_in: Decimal,
    /// The A2 intermediates; `None` under A11, which has no Z2.
    pub a2: Option<A2Figures>,
    /// `(16)` / `(16)A11` — **saldierungsfähige Netzeinspeisung**.
    pub settleable_feed_in: Decimal,
    /// `(20)` / `(20)A11` — the quantity whose levies fall to zero.
    pub levy_reducing: Decimal,
    /// `(21)` / `(21)A11` — **umlagebelasteter Netzbezug**, still levied.
    pub levied_grid_draw: Decimal,
}

/// Net one calendar month of a site under the Abgrenzungsoption.
///
/// `grid` must hold every quarter hour of the governed window exactly once;
/// quarter hours outside it are ignored, so a Rumpfmonat can be handed the
/// whole month. `points` are the sessions of the site's points, each with the
/// point it ran at: covered points make Z2, uncovered ones may only draw.
///
/// # Errors
///
/// A [`Refusal`] from [`governed`], or for an uncovered discharge, an
/// interpolated covered slot (A2), or a grid series that is not complete.
#[allow(
    clippy::similar_names,
    reason = "z1nb, z1ne, z2v, z2e are the Festlegung's own meter names"
)]
pub fn month(
    year: i32,
    month: CalendarMonth,
    site: &Site,
    conditions: &Conditions,
    grid: &[GridQuarter],
    points: &[(EvseId, SessionSplit)],
) -> Result<Month, Refusal> {
    let first = Date::from_calendar_date(year, month, 1).map_err(|_| Refusal::BeforeTheRules)?;
    let last = first_of_next_month(first).previous_day().unwrap_or(first);
    let governed = governed(Choice::Abgrenzung, conditions, first, last)?;
    let window = Window::of(&governed);
    let z1 = window.grid(grid)?;

    // Z2, by quarter hour and by direction — never netted before the formula.
    let mut z2: BTreeMap<QuarterHour, (Decimal, Decimal)> = BTreeMap::new();
    for (point, split) in points {
        let covered = site.covered.contains(point);
        for slot in &split.slots {
            if slot.energy.is_zero() || !window.contains(slot.quarter_hour) {
                continue;
            }
            if !covered {
                if slot.direction == Direction::Export {
                    return Err(Refusal::UncoveredDischarge {
                        point: point.clone(),
                        quarter: slot.quarter_hour,
                    });
                }
                // A unidirectional point's draw is sonstiger Verbrauch, already in Z1.
                continue;
            }
            if site.formelsatz == Formelsatz::A11 {
                continue;
            }
            if slot.provenance == Provenance::Interpolated {
                return Err(Refusal::NotMeasured {
                    point: point.clone(),
                    quarter: slot.quarter_hour,
                });
            }
            let entry = z2.entry(slot.quarter_hour).or_default();
            match slot.direction {
                Direction::Import => entry.0 += slot.energy.kwh(),
                Direction::Export => entry.1 += slot.energy.kwh(),
            }
        }
    }

    // (3), (4) `[MiSpeL A1 (3)]`, `[MiSpeL A1 (4)]`.
    let grid_draw: Decimal = z1.values().map(|q| q.draw.kwh()).sum();
    let grid_feed_in: Decimal = z1.values().map(|q| q.feed_in.kwh()).sum();

    let (a2, settleable_feed_in) = match site.formelsatz {
        // `[MiSpeL A1 (16)]` A11: with no other generation behind the
        // connection, all feed-in came out of the points and all they took came
        // from the grid — so all of it is saldierungsfähig.
        Formelsatz::A11 => (None, grid_feed_in),
        Formelsatz::A2 => {
            let (mut f5, mut f6, mut f9, mut f11) =
                (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
            for (quarter, &(z2v, z2e)) in &z2 {
                // Every covered quarter hour is inside the window, and the window's
                // grid series is complete, so the grid value exists.
                let (z1nb, z1ne) = z1.get(quarter).map_or((Decimal::ZERO, Decimal::ZERO), |q| {
                    (q.draw.kwh(), q.feed_in.kwh())
                });
                f5 += z2v; // (5)
                f6 += z2e; // (6)
                // `[MiSpeL A1 (1)]` — the Speichervorrang: grid draw goes to
                // the points before anything else in the quarter hour.
                f9 += z1nb.min(z2v);
                // `[MiSpeL A1 (2)]` — feed-in comes out of the points first.
                f11 += z1ne.min(z2e);
            }
            let f10 = f5 - f9; // `[MiSpeL A1 (10)]`
            let f12 = (f6 - f5).max(Decimal::ZERO); // `[MiSpeL A1 (12)]` Fremdtankstrom
            let f13 = (f11 - f12).max(Decimal::ZERO); // `[MiSpeL A1 (13)]`
            let f14 = CHARGE_POINT_EFFICIENCY; // `[MiSpeL A1 (14)]` A2
            let f15 = f14 * f10; // `[MiSpeL A1 (15)]`
            let f16 = (f13 - f15).max(Decimal::ZERO); // `[MiSpeL A1 (16)]`
            (
                Some(A2Figures {
                    device_consumption: f5,
                    device_generation: f6,
                    grid_charged: f9,
                    plant_charged: f10,
                    device_feed_in_base: f11,
                    foreign_charge: f12,
                    device_feed_in_considered: f13,
                    efficiency: f14,
                    renewable_storage_output: f15,
                }),
                f16,
            )
        }
    };

    // `[MiSpeL A1 (20)]` with (19)A2 = 0, and `[MiSpeL A1 (21)]`: never more
    // relief than there was draw.
    let levy_reducing = settleable_feed_in.min(grid_draw);
    Ok(Month {
        governed,
        formelsatz: site.formelsatz,
        grid_draw,
        grid_feed_in,
        a2,
        settleable_feed_in,
        levy_reducing,
        levied_grid_draw: grid_draw - levy_reducing,
    })
}

/// The Pauschaloption's two limits for a calendar year or Rumpfjahr,
/// numbered as in Anlage 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Limits {
    /// `(P22)R` — days of the period; a whole year is its own Rumpfjahr.
    pub days: u16,
    /// `(P19)R` — of them, days April to September.
    pub summer_days: u16,
    /// `(P1)` / `(P1)R` — Pauschalgrenze der Förderfähigkeit, kWh.
    pub support_cap: Decimal,
    /// `(P2)P2` — 0,2.
    pub factor: Decimal,
    /// `(P3)` / `(P3)R` — the Indifferenzbereich, kWh.
    pub indifference_band: Decimal,
    /// `(P4)` / `(P4)R` — Pauschalgrenze der Saldierungsfähigkeit, kWh.
    pub settlement_threshold: Decimal,
}

/// The limits of Basisfall P2 for the days `first..=last` of one calendar year.
///
/// The Rumpfjahr formulas apply to a whole year unchanged — 183 of 183 summer
/// days, 365 of 365 days — so there is one path. Each product is taken before
/// its quotient, which is the same formula with one rounding at the 28th digit
/// instead of two.
///
/// # Errors
///
/// [`Refusal::NoSolar`] or [`Refusal::SolarTooLarge`]: Anlage 2 has no case
/// without a solar plant, and none over 30 kWp.
pub fn pauschal_limits(solar_kwp: Decimal, first: Date, last: Date) -> Result<Limits, Refusal> {
    if solar_kwp <= Decimal::ZERO {
        return Err(Refusal::NoSolar);
    }
    if solar_kwp > PAUSCHAL_MAX_KWP {
        return Err(Refusal::SolarTooLarge(solar_kwp));
    }
    let year_days = if time::util::is_leap_year(first.year()) {
        366
    } else {
        365
    }; // `[MiSpeL A2 (P20)]`
    let mut days = 0u16;
    let mut summer_days = 0u16;
    let mut day = first;
    while day <= last {
        days += 1; // (P22)R
        if (4..=9).contains(&u8::from(day.month())) {
            summer_days += 1; // (P19)R
        }
        let Some(next) = day.next_day() else { break };
        day = next;
    }
    let p1 = solar_kwp * PAUSCHAL_KWH_PER_KWP; // `[MiSpeL A2 (P1)]`
    let p3 = PAUSCHAL_FACTOR_CHARGE_POINT * p1; // `[MiSpeL A2 (P3)]`
    // `[MiSpeL A2 (P18)]` and (P1)R = (P19)R · (P18).
    let support_cap = p1 * Decimal::from(summer_days) / Decimal::from(SUMMER_DAYS);
    // `[MiSpeL A2 (P21)]` and (P3)R = (P22)R · (P21).
    let indifference_band = p3 * Decimal::from(days) / Decimal::from(year_days);
    Ok(Limits {
        days,
        summer_days,
        support_cap,
        factor: PAUSCHAL_FACTOR_CHARGE_POINT,
        indifference_band,
        settlement_threshold: support_cap + indifference_band, // (P4)R
    })
}

/// One calendar year — or Rumpfjahr — under the Pauschaloption, Basisfall P2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Year {
    /// The window settled.
    pub governed: Governed,
    /// The limits for that window.
    pub limits: Limits,
    /// `(P7)` feed-in in quarter hours with a non-negative spot price, kWh.
    pub feed_in_at_non_negative_prices: Decimal,
    /// `(P8)` of it, the part above (P4).
    pub above_threshold: Decimal,
    /// `(P9)` total grid draw.
    pub grid_draw: Decimal,
    /// `(P10)` **saldierungsfähige Netzeinspeisung**.
    pub settleable_feed_in: Decimal,
    /// `(P11)` **umlagebelasteter Netzbezug**.
    pub levied_grid_draw: Decimal,
}

/// Settle one calendar year of a site with one bidirectional point and one
/// solar plant of `solar_kwp` under the Pauschaloption (Basisfall P2).
///
/// # Errors
///
/// A [`Refusal`] from [`governed`] — [`Refusal::NoStateAid`] above all, while
/// the Commission has not approved § 19 Abs. 3c EEG — from [`pauschal_limits`],
/// or for an incomplete grid series.
pub fn pauschal_year(
    year: i32,
    solar_kwp: Decimal,
    conditions: &Conditions,
    grid: &[GridQuarter],
) -> Result<Year, Refusal> {
    let first = Date::from_calendar_date(year, CalendarMonth::January, 1)
        .map_err(|_| Refusal::BeforeTheRules)?;
    let last = Date::from_calendar_date(year, CalendarMonth::December, 31)
        .map_err(|_| Refusal::BeforeTheRules)?;
    let governed = governed(Choice::Pauschal, conditions, first, last)?;
    let limits = pauschal_limits(solar_kwp, governed.first, governed.last)?;
    let z1 = Window::of(&governed).grid(grid)?;

    // `[MiSpeL A2 (P5)]`–`[MiSpeL A2 (P7)]`.
    let p7: Decimal = z1
        .values()
        .filter(|q| !q.spot_negative)
        .map(|q| q.feed_in.kwh())
        .sum();
    let p8 = (p7 - limits.settlement_threshold).max(Decimal::ZERO); // `[MiSpeL A2 (P8)]`
    let p9: Decimal = z1.values().map(|q| q.draw.kwh()).sum(); // `[MiSpeL A2 (P9)]`
    let p10 = p8.min(p9); // `[MiSpeL A2 (P10)]`
    Ok(Year {
        governed,
        limits,
        feed_in_at_non_negative_prices: p7,
        above_threshold: p8,
        grid_draw: p9,
        settleable_feed_in: p10,
        levied_grid_draw: p9 - p10, // `[MiSpeL A2 (P11)]`
    })
}

/// A governed window as instants: Europe/Berlin midnight to midnight.
struct Window {
    governed: Governed,
    from: time::OffsetDateTime,
    to: time::OffsetDateTime,
}

impl Window {
    fn of(governed: &Governed) -> Self {
        let zone = berlin();
        let midnight = |day: Date| {
            zone.instant_at(day, time::Time::MIDNIGHT)
                .unwrap_or_else(|| day.midnight().assume_utc())
        };
        Self {
            governed: *governed,
            from: midnight(governed.first),
            to: midnight(governed.last.next_day().unwrap_or(governed.last)),
        }
    }

    fn contains(&self, quarter: QuarterHour) -> bool {
        self.from <= quarter.start() && quarter.start() < self.to
    }

    /// The grid series inside the window, keyed by quarter hour — refused unless
    /// every quarter hour of the window is there exactly once.
    fn grid<'a>(
        &self,
        grid: &'a [GridQuarter],
    ) -> Result<BTreeMap<QuarterHour, &'a GridQuarter>, Refusal> {
        let mut inside = BTreeMap::new();
        for q in grid.iter().filter(|q| self.contains(q.quarter)) {
            if inside.insert(q.quarter, q).is_some() {
                return Err(Refusal::GridDuplicate(q.quarter));
            }
        }
        let expected =
            usize::try_from((self.to - self.from).whole_seconds() / QuarterHour::SECONDS)
                .unwrap_or(0);
        if inside.len() != expected {
            return Err(Refusal::GridIncomplete {
                expected,
                received: inside.len(),
                first: self.governed.first,
                last: self.governed.last,
            });
        }
        Ok(inside)
    }
}

/// The zone the Festlegung's calendar months are in.
fn berlin() -> TimeZone {
    // The database is compiled in (`tzdb-bundle-always`), so the name always
    // resolves; UTC would move a month's edges by an hour and is never right.
    TimeZone::new("Europe/Berlin").unwrap_or_else(|_| TimeZone::utc())
}

fn first_of_next_month(day: Date) -> Date {
    let (year, month) = match day.month() {
        CalendarMonth::December => (day.year() + 1, CalendarMonth::January),
        m => (day.year(), m.next()),
    };
    Date::from_calendar_date(year, month, 1).unwrap_or(day)
}
