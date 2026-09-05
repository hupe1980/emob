//! The three numbers `[38k §5(3)]` multiplies, and the years they change in.
//!
//! None of them is a constant this crate may hold. The counting factor steps
//! down twice on dates the Verordnung states; the emissions value is announced
//! in the Bundesanzeiger by 31 October **for the following obligation year**
//! `[38k §5(4)]`; and which value applies at all depends on where the
//! electricity came from `[38k §5(5)]`. A crate that baked any of them in
//! would produce a defensible answer for one year and a wrong one for every
//! other.

use crate::error::ThgError;
use rust_decimal::Decimal;

/// The first obligation year `[38k §5(3)]` states a counting factor for.
///
/// Earlier years had their own factors under earlier versions of the
/// Verordnung. Reading them out of the current text would be reading a number
/// that is not there, so this crate refuses them by name instead.
pub const FIRST_COUNTED_YEAR: i32 = 2024;

/// Megajoules in a kilowatt-hour. Exact, and the only unit conversion here.
///
/// The announced emissions value is per megajoule and a charge point measures
/// kilowatt-hours, so one of the two has to move. Three point six is exact in
/// decimal, which is why the whole calculation stays exact.
pub const MJ_PER_KWH: Decimal = Decimal::from_parts(36, 0, 0, false, 1);

/// The factor the energetic quantity is multiplied by `[38k §5(3) S. 1]`.
///
/// Three from 2024, two from 2035, one from 2036 — a schedule that is written
/// down once here rather than at each call site that needs it.
///
/// `None` before [`FIRST_COUNTED_YEAR`].
///
/// # There is a second schedule, and it is not this one
///
/// Since the *Zweites Gesetz zur Weiterentwicklung der Treibhausgasminderungs-
/// Quote* (in force 07.06.2026, applying from obligation year 2026) the
/// Verordnung states **two** factor schedules, and a function named for the
/// factor has to say which one it is.
///
/// `[38k §7(6)]` gives purely battery-electric vehicles of classes **M3 and
/// N3** — buses and heavy goods vehicles — a factor of 4 from 2027, stepping
/// down 3.5 (2035), 3 (2036), 2.5 (2037), 2 (2038), 1.5 (2039), 1 (2040). At
/// its widest that is a third more counted energy than this function returns,
/// which is not a rounding difference.
///
/// **It belongs to a different route, and this crate files the other one.**
/// `[38k §6]` is *"Energetische Menge des elektrischen Stroms aus öffentlich
/// zugänglichen Ladepunkten"* — energy a meter at a public point measured, and
/// the operator of that point is the claimant. `[38k §7]` is *"in anderen
/// Fällen"*: non-public charging, where nothing measures per session, the
/// quantity is a published *Schätzwert* rather than a reading, and the claimant
/// is the person the vehicle is registered to.
///
/// So the elevated factor sits exactly where the vehicle class is a fact
/// somebody holds a Zulassungsbescheinigung for. A public charge point does not
/// know what it is charging, which is why `[38k §5(3)]` gives it one factor for
/// everything — and why a `vehicle_class` argument here would be a field a
/// caller fills in rather than a fact the estate can evidence, which is the one
/// shape this workspace refuses everywhere else.
///
/// [`crate::ThgError::NotPublic`] names § 7 as the route a non-public point
/// has, so the refusal points somewhere rather than reading as "worthless".
#[must_use]
pub fn counting_factor(year: i32) -> Option<Decimal> {
    match year {
        y if y >= 2036 => Some(Decimal::ONE),
        2035 => Some(Decimal::TWO),
        y if y >= FIRST_COUNTED_YEAR => Some(Decimal::from(3)),
        _ => None,
    }
}

/// A vehicle class, for the factor `[38k §7(6)]` gives two of them.
///
/// The distinction exists in exactly one place in the Verordnung and it is worth
/// a third of the revenue: a bus depot's kilowatt-hours count four times from
/// 2027 where a car's count three, and the schedules do not converge until 2040.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum VehicleClass {
    /// Every class `[38k §5(3)]`'s single schedule covers — a passenger car,
    /// a van, a light truck.
    #[default]
    Other,
    /// **M3 and N3** — buses and heavy goods vehicles, as
    /// `[38k §7(6)]` names them by reference to Anlage XXIX Abschnitt 1
    /// Nummer 1 und 2 of the Straßenverkehrs-Zulassungs-Ordnung.
    HeavyM3OrN3,
}

impl VehicleClass {
    /// The Verordnung's own word for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Other => "andere Klasse",
            Self::HeavyM3OrN3 => "Klasse M3 oder N3",
        }
    }

    /// The factor `[38k §5(3) S. 1]` or `[38k §7(6)]` multiplies by in `year`.
    ///
    /// # Two schedules, and the second is a whole paragraph rather than a row
    ///
    /// `[38k §7(6)]` opens *"Abweichend von § 5 Absatz 3 Satz 1"* and states
    /// **seven** steps against § 5(3)'s three:
    ///
    /// | from | M3 / N3 | everything else |
    /// |---|---|---|
    /// | 2024 | — | 3 |
    /// | 2027 | 4 | 3 |
    /// | 2035 | 3.5 | 2 |
    /// | 2036 | 3 | 1 |
    /// | 2037 | 2.5 | 1 |
    /// | 2038 | 2 | 1 |
    /// | 2039 | 1.5 | 1 |
    /// | 2040 | 1 | 1 |
    ///
    /// Before 2027 the paragraph states nothing, so § 5(3)'s schedule is the
    /// one that applies — the deviation begins where the deviation says it
    /// begins, and reading `4` back into 2026 would be reading a number that is
    /// not there.
    ///
    /// `None` before [`FIRST_COUNTED_YEAR`], for both classes.
    #[must_use]
    pub fn factor(self, year: i32) -> Option<Decimal> {
        // A half-step, written at the scale the Verordnung prints: `3.5`, not
        // `3.50`. Every value here is a small whole number of tenths.
        let tenths = |value: u32| Decimal::from_parts(value, 0, 0, false, 1);
        match self {
            Self::Other => counting_factor(year),
            Self::HeavyM3OrN3 => match year {
                y if y >= 2040 => Some(Decimal::ONE),
                2039 => Some(tenths(15)),
                2038 => Some(Decimal::TWO),
                2037 => Some(tenths(25)),
                2036 => Some(Decimal::from(3)),
                2035 => Some(tenths(35)),
                y if y >= 2027 => Some(Decimal::from(4)),
                // "ab dem Kalenderjahr 2027": before it, the deviation has not
                // begun and § 5(3) is the schedule.
                other => counting_factor(other),
            },
        }
    }
}

/// The adjustment factor for drive efficiency, from Anlage 3 `[38k §5(3)]`.
///
/// The whole table rather than the one row a charge point needs: the
/// combustion row is what the electric rows are a discount against, and a
/// reviewer checking `0.4` against the Verordnung is looking at a table with
/// three rows in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum DriveEfficiency {
    /// `Verbrennungsmotor` — 1.
    CombustionEngine,
    /// `Batteriegestützter Elektroantrieb` — 0.4. What a charge point supplies.
    #[default]
    BatteryElectric,
    /// `Wasserstoffzellengestützter Elektroantrieb` — 0.4.
    HydrogenFuelCell,
}

impl DriveEfficiency {
    /// The Anlage 3 factor.
    #[must_use]
    pub fn factor(self) -> Decimal {
        match self {
            Self::CombustionEngine => Decimal::ONE,
            // 0.4, written with the scale the table prints.
            Self::BatteryElectric | Self::HydrogenFuelCell => {
                Decimal::from_parts(4, 0, 0, false, 1)
            }
        }
    }
}

/// A renewable source `[38k §5(5) S. 1 Nr. 1]` names, and the year it starts
/// counting in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum RenewableSource {
    /// Wind — letter a.
    Wind,
    /// Sun — letter b.
    Solar,
    /// Biomass — letter c, from 2028.
    Biomass,
    /// Landfill gas — letter c, from 2028.
    LandfillGas,
    /// Sewage gas — letter c, from 2028.
    SewageGas,
    /// Biogas — letter c, from 2028.
    Biogas,
}

impl RenewableSource {
    /// The first obligation year this source counts in.
    ///
    /// Letter c carries its own date — "ab dem Verpflichtungsjahr 2028" — and
    /// it sits inside the list rather than beside it, which is exactly the
    /// shape a reader skims past.
    #[must_use]
    pub const fn countable_from(self) -> i32 {
        match self {
            Self::Wind | Self::Solar => FIRST_COUNTED_YEAR,
            Self::Biomass | Self::LandfillGas | Self::SewageGas | Self::Biogas => 2028,
        }
    }

    /// The Verordnung's own word for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wind => "Wind",
            Self::Solar => "Sonne",
            Self::Biomass => "Biomasse",
            Self::LandfillGas => "Deponiegas",
            Self::SewageGas => "Klärgas",
            Self::Biogas => "Biogas",
        }
    }
}

/// What `[38k §5(5) S. 1 Nr. 2]` asks about how the electricity arrived, and
/// what proves it.
///
/// Every field is a fact somebody has to hold a document for. They are
/// separate because the paragraph's fallback is per-condition: a claim missing
/// any one of them is calculated on the grid average, and an operator that
/// knows *which* one is missing knows what to go and get.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DirectSupply {
    /// Not drawn from the grid, but directly from a generating plant behind
    /// the same grid connection point `[38k §5(5) S. 1 Nr. 2]`.
    pub behind_same_connection_point: bool,
    /// The metering point operator's measurements of simultaneous consumption
    /// **per quarter hour** `[38k §5(5) S. 2]`.
    ///
    /// The quarter hour is the same grid this workspace already rates and
    /// splits on, which is why a platform that can produce this at all can
    /// produce it from what it already has.
    pub quarter_hourly_simultaneity_proved: bool,
    /// The authority's duty-of-care declaration is attached `[38k §5(5)]`.
    pub duty_of_care_declared: bool,
}

impl DirectSupply {
    /// Everything the paragraph asks for, held.
    #[must_use]
    pub const fn complete() -> Self {
        Self {
            behind_same_connection_point: true,
            quarter_hourly_simultaneity_proved: true,
            duty_of_care_declared: true,
        }
    }

    /// The first condition that is not met, in the paragraph's order.
    #[must_use]
    pub const fn missing(self) -> Option<&'static str> {
        if !self.behind_same_connection_point {
            Some(
                "the electricity is drawn from the grid rather than from a plant behind the same connection point",
            )
        } else if !self.quarter_hourly_simultaneity_proved {
            Some("no quarter-hourly simultaneity measurements from the metering point operator")
        } else if !self.duty_of_care_declared {
            Some("the duty-of-care declaration is not attached")
        } else {
            None
        }
    }
}

/// Which paragraph an emissions value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum BasisKind {
    /// The German grid average `[38k §5(4)]`.
    GridAverage,
    /// One renewable source, supplied directly `[38k §5(5)]`.
    Renewable(RenewableSource),
}

/// The average greenhouse-gas emissions per energy unit that `[38k §5(3)]`
/// multiplies by, and the announcement it came from.
///
/// The announcement is carried with the number because a notification is a
/// document somebody checks two years later, and *which* Bundesanzeiger notice
/// a figure came from is the first thing they will ask.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EmissionsBasis {
    grams_co2e_per_mj: Decimal,
    kind: BasisKind,
    announcement: String,
}

/// Read back through the same check the constructors run, because a negative
/// emissions value is a filing that claims a saving from having emitted.
///
/// The renewable *proof* is not re-run: `[38k §5(5)]`'s conditions are facts
/// about a supply arrangement rather than about this value, and
/// [`Self::renewable`] is where they are established. What survives the wire is
/// the figure and the announcement it came from, and the figure is checked
/// (D264).
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for EmissionsBasis {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        #[derive(serde::Deserialize)]
        struct AsSent {
            grams_co2e_per_mj: Decimal,
            kind: BasisKind,
            announcement: String,
        }

        let sent = AsSent::deserialize(deserializer)?;
        Self::checked(sent.grams_co2e_per_mj, sent.kind, sent.announcement)
            .map_err(D::Error::custom)
    }
}

impl EmissionsBasis {
    /// The grid average announced for the obligation year `[38k §5(4)]`.
    ///
    /// # Errors
    ///
    /// [`ThgError::Negative`] for a negative value.
    pub fn grid_average(
        grams_co2e_per_mj: Decimal,
        announcement: impl Into<String>,
    ) -> Result<Self, ThgError> {
        Self::checked(grams_co2e_per_mj, BasisKind::GridAverage, announcement)
    }

    /// The value for one renewable source `[38k §5(5)]`, when the whole of the
    /// paragraph holds.
    ///
    /// # Errors
    ///
    /// [`ThgError::ProofIncomplete`] naming the first condition that is not
    /// met — which is the fallback to [`Self::grid_average`] that the
    /// paragraph's own sentence prescribes, stated rather than performed
    /// quietly. [`ThgError::Negative`] for a negative value.
    pub fn renewable(
        source: RenewableSource,
        grams_co2e_per_mj: Decimal,
        announcement: impl Into<String>,
        supply: DirectSupply,
    ) -> Result<Self, ThgError> {
        if let Some(missing) = supply.missing() {
            return Err(ThgError::ProofIncomplete {
                missing: missing.to_string(),
            });
        }
        Self::checked(
            grams_co2e_per_mj,
            BasisKind::Renewable(source),
            announcement,
        )
    }

    fn checked(
        grams_co2e_per_mj: Decimal,
        kind: BasisKind,
        announcement: impl Into<String>,
    ) -> Result<Self, ThgError> {
        if grams_co2e_per_mj.is_sign_negative() {
            return Err(ThgError::Negative {
                what: "emissions per energy unit",
                value: grams_co2e_per_mj.to_string(),
            });
        }
        Ok(Self {
            grams_co2e_per_mj,
            kind,
            announcement: announcement.into(),
        })
    }

    /// Grams of CO₂ equivalent per megajoule.
    #[must_use]
    pub const fn grams_co2e_per_mj(&self) -> Decimal {
        self.grams_co2e_per_mj
    }

    /// Which paragraph it came from.
    #[must_use]
    pub const fn kind(&self) -> BasisKind {
        self.kind
    }

    /// The Bundesanzeiger announcement.
    #[must_use]
    pub fn announcement(&self) -> &str {
        &self.announcement
    }

    /// Whether this basis may be used in the obligation year.
    ///
    /// # Errors
    ///
    /// [`ThgError::SourceNotYetCountable`] for a letter-c source before 2028.
    pub const fn usable_in(&self, year: i32) -> Result<(), ThgError> {
        if let BasisKind::Renewable(source) = self.kind {
            let from = source.countable_from();
            if year < from {
                return Err(ThgError::SourceNotYetCountable {
                    energy_source: source.as_str(),
                    from,
                    year,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[cfg(feature = "serde")]
    #[test]
    fn an_announced_basis_arrives_through_the_check_its_constructors_run() {
        // A negative emissions value is a filing that claims a saving from
        // having emitted, and it multiplies into `[38k §5(3)]`'s reference
        // value. A derived `Deserialize` restored it from a store without
        // asking (D264).
        let negative = r#"{"grams_co2e_per_mj":"-1","kind":"grid_average","announcement":"BAnz"}"#;
        assert!(serde_json::from_str::<EmissionsBasis>(negative).is_err());

        let announced = EmissionsBasis::grid_average(Decimal::from(96), "BAnz").unwrap();
        let json = serde_json::to_string(&announced).unwrap();
        assert_eq!(
            serde_json::from_str::<EmissionsBasis>(&json).unwrap(),
            announced
        );
    }

    #[test]
    fn the_counting_factor_steps_down_on_the_verordnungs_own_dates() {
        assert_eq!(counting_factor(2023), None);
        assert_eq!(counting_factor(2024), Some(dec("3")));
        assert_eq!(counting_factor(2034), Some(dec("3")));
        assert_eq!(counting_factor(2035), Some(dec("2")));
        assert_eq!(counting_factor(2036), Some(dec("1")));
        assert_eq!(counting_factor(2050), Some(dec("1")));
    }

    #[test]
    fn anlage_3_is_the_whole_table() {
        assert_eq!(DriveEfficiency::CombustionEngine.factor(), dec("1"));
        assert_eq!(DriveEfficiency::BatteryElectric.factor(), dec("0.4"));
        assert_eq!(DriveEfficiency::HydrogenFuelCell.factor(), dec("0.4"));
    }

    #[test]
    fn letter_c_carries_its_own_date_inside_the_list() {
        assert_eq!(RenewableSource::Wind.countable_from(), FIRST_COUNTED_YEAR);
        assert_eq!(RenewableSource::Solar.countable_from(), FIRST_COUNTED_YEAR);
        for late in [
            RenewableSource::Biomass,
            RenewableSource::LandfillGas,
            RenewableSource::SewageGas,
            RenewableSource::Biogas,
        ] {
            assert_eq!(late.countable_from(), 2028);
        }
    }

    #[test]
    fn a_source_that_does_not_count_yet_is_refused_by_year() {
        let basis = EmissionsBasis::renewable(
            RenewableSource::Biogas,
            dec("20"),
            "BAnz",
            DirectSupply::complete(),
        )
        .unwrap();
        assert!(matches!(
            basis.usable_in(2026),
            Err(ThgError::SourceNotYetCountable { from: 2028, .. })
        ));
        assert!(basis.usable_in(2028).is_ok());
        // The grid average has no source, so no year can exclude it.
        assert!(
            EmissionsBasis::grid_average(dec("96"), "BAnz")
                .unwrap()
                .usable_in(2024)
                .is_ok()
        );
    }

    #[test]
    fn the_renewable_basis_needs_the_whole_of_the_paragraph() {
        // Nothing held: the message names the first missing condition, which is
        // the caller's instruction to fall back to `[38k §5(4)]`.
        assert!(matches!(
            EmissionsBasis::renewable(
                RenewableSource::Wind,
                dec("13"),
                "BAnz",
                DirectSupply::default()
            ),
            Err(ThgError::ProofIncomplete { .. })
        ));

        // Behind the meter, but no quarter-hourly simultaneity.
        let err = EmissionsBasis::renewable(
            RenewableSource::Wind,
            dec("13"),
            "BAnz",
            DirectSupply {
                behind_same_connection_point: true,
                ..DirectSupply::default()
            },
        )
        .unwrap_err();
        let ThgError::ProofIncomplete { missing } = &err else {
            panic!("expected ProofIncomplete, got {err}");
        };
        assert!(missing.contains("quarter-hourly"), "{missing}");

        assert!(
            EmissionsBasis::renewable(
                RenewableSource::Wind,
                dec("13"),
                "BAnz",
                DirectSupply::complete()
            )
            .is_ok()
        );
    }

    #[test]
    fn an_emissions_value_cannot_be_negative() {
        assert!(matches!(
            EmissionsBasis::grid_average(dec("-1"), "BAnz"),
            Err(ThgError::Negative { .. })
        ));
    }
}
