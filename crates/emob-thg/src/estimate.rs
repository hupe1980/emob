//! `[38k §7]` — the other route, *„in anderen Fällen"*.
//!
//! # A different claim, not a smaller one
//!
//! [`crate::claim`] files `[38k §6]`: kilowatt-hours a meter at a **publicly
//! accessible** point measured, claimed by that point's operator or the third
//! party it designated. This module files the paragraph beside it, and every
//! load-bearing fact is different.
//!
//! | | `[38k §6]` | `[38k §7]` |
//! |---|---|---|
//! | who the *Ladepunktbetreiber* is | the operator of the point | *"die Person, auf die … das reine Batterieelektrofahrzeug zugelassen ist"* |
//! | what the quantity is | a mess- und eichrechtskonform measured value | a published **Schätzwert**, once per vehicle |
//! | what evidence stands behind it | a signed meter record | a Zulassungsbescheinigung Teil I |
//! | when it is filed | 28 February of the **following** year | 15 November **inside** the obligation year |
//! | what the counting factor is | `[38k §5(3)]` — one schedule | `[38k §7(6)]` — two, and M3/N3 reach 4 |
//!
//! Nothing a charge point holds appears in that column, which is why this is a
//! second claim type rather than a flag on the first (D213). A depot operator is
//! routinely **both** parties at once — its posts are not publicly accessible,
//! so § 6 refuses them, and its buses are registered to it, so § 7 counts them —
//! and that is the case worth building for.
//!
//! # What this crate will not do
//!
//! It does not decide whether a vehicle is a *reines Batterieelektrofahrzeug*,
//! whether a registration is current, or what the Schätzwert is. Those are facts
//! about a vehicle and a Bundesanzeiger notice, and they arrive here already
//! established — the same rule [`crate::claim`] keeps for the emissions value.

use std::collections::BTreeMap;

use emob_core::{Crossing, Note};
use rust_decimal::Decimal;
use time::Date;

use crate::claim::{Attribution, Route};
use crate::error::ThgError;
use crate::factors::{DriveEfficiency, EmissionsBasis, MJ_PER_KWH, VehicleClass};

/// The estimate `[38k §7(3)]` publishes, and the notice it came from.
///
/// > Das Bundesministerium für Umwelt, Naturschutz und nukleare Sicherheit gibt
/// > die Schätzwerte der anrechenbaren energetischen Mengen elektrischen Stroms
/// > für reine Batterieelektrofahrzeuge im Bundesanzeiger bekannt.
///
/// An argument rather than a constant, for the reason every figure in this crate
/// is one: it is announced annually, and a claim replayed two years later has to
/// reproduce the value that was in force rather than today's. The notice travels
/// with the number because *which* announcement a figure came from is the first
/// thing an auditor asks.
///
/// It is **per vehicle class**, because `[38k §7(1) S. 3]` contemplates classes
/// with their own value — *"wenn für die entsprechende Fahrzeugklasse ein
/// eigener Schätzwert nach Absatz 3 bekannt gegeben wurde"* — and a bus does not
/// consume what a car does.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Estimate {
    kwh: Decimal,
    class: VehicleClass,
    announcement: String,
}

impl Estimate {
    /// The published estimate for a vehicle class, in kilowatt-hours.
    ///
    /// # Errors
    ///
    /// [`ThgError::Negative`] for a negative value.
    pub fn announced(
        kwh: Decimal,
        class: VehicleClass,
        announcement: impl Into<String>,
    ) -> Result<Self, ThgError> {
        if kwh.is_sign_negative() {
            return Err(ThgError::Negative {
                what: "the published estimate",
                value: kwh.to_string(),
            });
        }
        Ok(Self {
            kwh,
            class,
            announcement: announcement.into(),
        })
    }

    /// The estimate, in kilowatt-hours.
    #[must_use]
    pub const fn kwh(&self) -> Decimal {
        self.kwh
    }

    /// Which class it was published for.
    #[must_use]
    pub const fn class(&self) -> VehicleClass {
        self.class
    }

    /// The Bundesanzeiger notice it came from.
    #[must_use]
    pub fn announcement(&self) -> &str {
        &self.announcement
    }
}

/// What `[38k §7(2)]` asks the filer to hold about one vehicle.
///
/// Every field is a document rather than an assertion, and they are separate
/// because the refusal names which one is missing: a filer that knows *which*
/// piece of paper it lacks knows what to go and get.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RegistrationEvidence {
    /// A copy of the Zulassungsbescheinigung Teil I is on file
    /// `[38k §7(2) S. 2]` — or, for a vehicle exempt from registration under
    /// `§ 3(3) FZV`, the Übereinstimmungsbescheinigung `[38k §7(2) S. 5]`.
    pub certificate_on_file: bool,
    /// …and it is **current**.
    ///
    /// `[38k §7(2) S. 3]`: *"Spätestens nach Ablauf eines Jahres ist eine Kopie
    /// der aktuellen Zulassungsbescheinigung Teil I als Nachweis
    /// erforderlich."* A copy taken once and never refreshed stops being
    /// evidence after a year, which is the quiet way a fleet's claim decays.
    pub certificate_current: bool,
    /// The vehicle is a *reines Batterieelektrofahrzeug* `[38k §7(1) S. 1]`.
    ///
    /// Not a plug-in hybrid. The paragraph counts only the pure battery
    /// electric vehicle, and it says so in the sentence that opens the route.
    pub battery_electric_only: bool,
}

impl RegistrationEvidence {
    /// Everything the paragraph asks for, held.
    #[must_use]
    pub const fn complete() -> Self {
        Self {
            certificate_on_file: true,
            certificate_current: true,
            battery_electric_only: true,
        }
    }

    /// The first condition that is not met, in the paragraph's own order.
    #[must_use]
    pub const fn missing(self) -> Option<&'static str> {
        if !self.battery_electric_only {
            Some("`[38k §7(1)]` counts only a reines Batterieelektrofahrzeug, and this is not one")
        } else if !self.certificate_on_file {
            Some("no copy of the Zulassungsbescheinigung Teil I is on file `[38k §7(2) S. 2]`")
        } else if !self.certificate_current {
            Some(
                "the Zulassungsbescheinigung Teil I on file is not the current one `[38k §7(2) S. 3]`",
            )
        } else {
            None
        }
    }
}

/// One vehicle counted in one obligation year.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VehicleRecord {
    /// The filer's own reference for the vehicle.
    ///
    /// **Not** a registration plate and not a vehicle identification number.
    /// `[38k §7(4) S. 2]` requires a vehicle to be counted once per obligation
    /// year, which needs an identifier that is stable and unique *within one
    /// filer's records* — and nothing more. A plate is a lifelong identifier of
    /// a thing a person drives, and putting one in a file that leaves the
    /// building builds a movement profile the Verordnung never asks for. The
    /// same reasoning as `emob_session::TokenRef`, one document along.
    pub reference: String,
    /// Which class, which decides the counting factor `[38k §7(6)]`.
    pub class: VehicleClass,
    /// What the filer holds for it `[38k §7(2)]`.
    pub evidence: RegistrationEvidence,
    /// The estimate that was counted, in kilowatt-hours.
    pub estimate_kwh: Decimal,
}

/// A `[38k §7]` notification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EstimateClaim {
    /// The obligation year.
    pub year: i32,
    /// Who is filing, and for whom.
    pub attribution: Attribution,
    /// One line per vehicle, ordered by reference.
    pub vehicles: Vec<VehicleRecord>,
    /// The emissions value and the announcement it came from.
    pub basis: EmissionsBasis,
    /// Anlage 3's adjustment factor.
    pub efficiency: DriveEfficiency,
}

impl EstimateClaim {
    /// The route this claim is filed under — always `[38k §7]`.
    pub const ROUTE: Route = Route::EstimatedPerVehicle;

    /// The last day this notification may be filed `[38k §8(1) S. 1 Nr. 2]`.
    ///
    /// The 15th of November **inside** the obligation year, which is the trap: a
    /// filer holding the § 6 date in their head misses by three and a half
    /// months in the direction that cannot be recovered.
    #[must_use]
    pub fn deadline(&self) -> Date {
        Self::ROUTE.deadline(self.year)
    }

    /// Whether filing on `on` is in time.
    #[must_use]
    pub fn in_time(&self, on: Date) -> bool {
        Self::ROUTE.in_time(self.year, on)
    }

    /// The energetic quantity across every vehicle, in megawatt-hours
    /// `[38k §7(4) S. 1]`.
    ///
    /// > … durch die Multiplikation der Zahl der reinen
    /// > Batterieelektrofahrzeuge … mit dem Schätzwert.
    #[must_use]
    pub fn megawatt_hours(&self) -> Decimal {
        self.vehicles
            .iter()
            .map(|v| v.estimate_kwh / Decimal::from(1000))
            .sum()
    }

    /// The quantity after the counting factor, **per class**.
    ///
    /// The one place the two schedules meet. A mixed fleet — a depot's buses
    /// beside its vans — is counted at two factors in one notification, so the
    /// multiplication happens per vehicle and not on the total.
    ///
    /// # Panics
    ///
    /// If [`Self::year`] states a year `[38k §5(3)]` counts nothing in — before
    /// [`crate::factors::FIRST_COUNTED_YEAR`]. [`EstimateClaimBuilder`] refuses
    /// one, and the two classes state a factor for exactly the same years, so
    /// no claim this crate assembles can reach it. The fields are public, so a
    /// claim deserialised from a store or written by hand can: that is a
    /// document nobody may file, and it is better to say so than to return a
    /// number for a year the Verordnung has none for.
    #[must_use]
    pub fn counted_megawatt_hours(&self) -> Decimal {
        self.vehicles
            .iter()
            .map(|v| {
                let factor = v
                    .class
                    .factor(self.year)
                    .expect("the year was checked at construction");
                v.estimate_kwh / Decimal::from(1000) * factor
            })
            .sum()
    }

    /// The greenhouse-gas emissions of the counted electricity, in kilograms of
    /// CO₂ equivalent `[38k §7(6) S. 2]` → `[38k §5(3) S. 2]`.
    ///
    /// *"§ 5 Absatz 3 Satz 2 gilt entsprechend"*, so the arithmetic is the one
    /// the other route already uses: counted energy × the announced emissions
    /// value × Anlage 3's efficiency factor, with the value stated per megajoule
    /// and the energy in kilowatt-hours.
    #[must_use]
    pub fn emissions_kg_co2e(&self) -> Decimal {
        let mj = self.counted_megawatt_hours() * Decimal::from(1000) * MJ_PER_KWH;
        mj * self.basis.grams_co2e_per_mj() * self.efficiency.factor() / Decimal::from(1000)
    }
}

/// Builds a `[38k §7]` notification, refusing every vehicle the paragraph does.
#[derive(Debug)]
pub struct EstimateClaimBuilder {
    year: i32,
    attribution: Attribution,
    basis: EmissionsBasis,
    efficiency: DriveEfficiency,
    vehicles: BTreeMap<String, VehicleRecord>,
    notes: Vec<Note>,
}

impl EstimateClaimBuilder {
    /// A notification for one obligation year, on one announced basis.
    ///
    /// # Errors
    ///
    /// [`ThgError::YearNotCounted`] for a year no schedule states a factor for,
    /// and [`ThgError::SourceNotYetCountable`] for a renewable basis whose
    /// source does not count yet.
    pub fn new(
        year: i32,
        attribution: Attribution,
        basis: EmissionsBasis,
        efficiency: DriveEfficiency,
    ) -> Result<Self, ThgError> {
        if VehicleClass::Other.factor(year).is_none() {
            return Err(ThgError::YearNotCounted {
                year,
                first: crate::factors::FIRST_COUNTED_YEAR,
            });
        }
        basis.usable_in(year)?;
        Ok(Self {
            year,
            attribution,
            basis,
            efficiency,
            vehicles: BTreeMap::new(),
            notes: Vec::new(),
        })
    }

    /// Count one vehicle.
    ///
    /// # Errors
    ///
    /// [`ThgError::NoAgreement`] when the filer holds no `[38k §5(2)]`
    /// designation covering this keeper, [`ThgError::RegistrationNotEvidenced`]
    /// for a vehicle `[38k §7(1)–(2)]` refuses, [`ThgError::NoEstimateForClass`]
    /// when the estimate was published for a different class, and
    /// [`ThgError::VehicleAlreadyCounted`] for a second line naming the same
    /// vehicle `[38k §7(4) S. 2]`.
    pub fn vehicle(
        &mut self,
        reference: impl Into<String>,
        keeper: &str,
        class: VehicleClass,
        evidence: RegistrationEvidence,
        estimate: &Estimate,
    ) -> Result<(), ThgError> {
        let reference = reference.into();

        // `[38k §5(2)]`: a third party files only for keepers that designated
        // it. The same gate the other route applies to an operator, asked of
        // the person the vehicle is registered to — who *is* the
        // Ladepunktbetreiber here `[38k §7(1) S. 2]`.
        if !self.attribution.covers(keeper) {
            return Err(ThgError::NoAgreement {
                third_party: self.attribution.third_party.clone(),
                operator: keeper.to_owned(),
                evse_id: reference,
            });
        }

        if let Some(missing) = evidence.missing() {
            return Err(ThgError::RegistrationNotEvidenced {
                reference,
                missing: missing.to_owned(),
            });
        }

        // A Schätzwert is published *for a class*, and counting a bus at a
        // car's estimate is inventing a quantity nobody announced.
        if estimate.class() != class {
            return Err(ThgError::NoEstimateForClass {
                reference,
                expected: class.as_str(),
                announced_for: estimate.class().as_str(),
            });
        }

        // `[38k §7(4) S. 2]`: *"Die Anrechnung … kann pro reinem
        // Batterieelektrofahrzeug und pro Verpflichtungsjahr nur einmal
        // erfolgen."* A second line for one vehicle is the paragraph's own
        // refusal, and a silent replacement would hide it.
        if self.vehicles.contains_key(&reference) {
            return Err(ThgError::VehicleAlreadyCounted { reference });
        }

        // `[38k §7(6)]`'s schedule does not begin until 2027, so a heavy
        // vehicle counted before it is counted at § 5(3)'s factor. Said rather
        // than silently applied: an operator planning a depot wants to know
        // that the same bus is worth a third more next year.
        if class == VehicleClass::HeavyM3OrN3
            && class.factor(self.year) == VehicleClass::Other.factor(self.year)
        {
            self.notes.push(Note::new(
                format!("/vehicles/{reference}"),
                format!(
                    "this is a {} and `[38k §7(6)]`'s schedule begins in 2027, so for {} it counts at `[38k §5(3)]`'s factor like any other vehicle",
                    class.as_str(),
                    self.year
                ),
            ));
        }

        self.vehicles.insert(
            reference.clone(),
            VehicleRecord {
                reference,
                class,
                evidence,
                estimate_kwh: estimate.kwh(),
            },
        );
        Ok(())
    }

    /// The finished notification, with everything the build had to say about it.
    ///
    /// # Errors
    ///
    /// [`ThgError::NothingToReport`] when no vehicle was counted.
    pub fn build(self) -> Result<Crossing<EstimateClaim>, ThgError> {
        if self.vehicles.is_empty() {
            return Err(ThgError::NothingToReport);
        }
        let claim = EstimateClaim {
            year: self.year,
            attribution: self.attribution,
            vehicles: self.vehicles.into_values().collect(),
            basis: self.basis,
            efficiency: self.efficiency,
        };
        let mut crossing = Crossing::lossless(claim);
        crossing.absorb_notes("", self.notes);
        Ok(crossing)
    }
}
