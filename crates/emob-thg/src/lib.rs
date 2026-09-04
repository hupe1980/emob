//! **The German greenhouse-gas quota for public charging, as executable law.**
//!
//! A charge point operator's kilowatt-hours are worth money twice: once from
//! the driver, and once from a fuel supplier that has to reduce the emissions
//! of what it sells `[38k §5]`. The second is the *THG-Quote*, and it is
//! settled from a notification the operator — or a third party it designates —
//! files for the obligation year.
//!
//! This crate builds that notification. It is the last domain crate in the
//! workspace, and it is the shortest, because everything it needs already
//! exists: `emob-eichrecht` decided which kilowatt-hours a meter actually
//! signed, `emob-cdr` settled them into records, and `emob-core`'s obligation
//! calendar already carries the eligibility rule.
//!
//! # What this crate refuses
//!
//! `[38k §6(3)]` states **four** cumulative conditions, and the interesting
//! property of all four is that failing them is invisible: the point charges
//! cars, the sessions bill, the money arrives, and the quota simply does not.
//! An operator finds out a year later, from the competent authority, about
//! energy it can no longer go back and measure differently.
//!
//! So a point that fails any of them is a **refusal that names the remedy**
//! rather than a line quietly missing from a file:
//!
//! ```text
//! DE*ABC*E00042 is not eligible: publish the register entry or consent to its
//! publication, sign the conformity declaration the authority provides, and
//! obtain an operator identification code — or forgo the quota
//! ```
//!
//! Three more refusals sit beside it. A record with no signed evidence cannot
//! show `[38k §6(3) Nr. 2]` and is refused rather than summed. A point outside
//! the German electricity-tax territory is not a withdrawal `[38k §5(1)]`
//! counts. And a point whose operator has not designated this filer in
//! Textform `[38k §5(2)]` does not belong in this filer's notification at all.
//!
//! # The three factors, and why none of them is a constant here
//!
//! `[38k §5(3)]` multiplies four things: the energetic quantity, a counting
//! factor that steps down in 2035 and again in 2036, the average emissions per
//! energy unit of German electricity, and Anlage 3's adjustment factor for
//! drive efficiency — `0.4` for a battery-electric drive.
//!
//! The emissions value is **announced annually in the Bundesanzeiger, by 31
//! October, for the following obligation year** `[38k §5(4)]`. A crate holding
//! it as a constant would be right for one year and wrong for every other, so
//! it is an argument, carried with the announcement it came from — because the
//! first question anyone asks of a two-year-old notification is which notice a
//! figure came out of.
//!
//! ```no_run
//! # use emob_thg::{Attribution, ClaimBuilder, DriveEfficiency, EmissionsBasis};
//! # use rust_decimal::Decimal;
//! # fn demo(profile: &emob_core::station::ChargePointProfile, ledger: &emob_cdr::CdrLedger)
//! # -> Result<(), Box<dyn std::error::Error>> {
//! let basis = EmissionsBasis::grid_average(Decimal::from(96), "BAnz AT 31.10.2025 B5")?;
//! let mut claim = ClaimBuilder::new(
//!     2026,
//!     Attribution::own("ABC"),
//!     basis,
//!     DriveEfficiency::BatteryElectric,
//! )?;
//! claim.point(profile, "Musterstraße 1, 10115 Berlin", ledger)?;
//!
//! let filed = claim.build()?;
//! println!("{} MWh", filed.value.megawatt_hours());
//! println!("{} kg CO2e", filed.value.emissions_kg_co2e());
//! for reason in filed.reasons() {
//!     println!("{reason}");  // what did not reach the file, and why
//! }
//! # Ok(()) }
//! ```
//!
//! # Renewable electricity is a conjunction, and it has a date inside it
//!
//! `[38k §5(5)]` lets a claim use the value for one renewable source instead
//! of the grid average — but only when the electricity is drawn **directly
//! from a plant behind the same grid connection point** rather than from the
//! grid, proved by the metering point operator's quarter-hourly measurements
//! of simultaneous consumption. Both conditions, not either.
//!
//! And the list of sources carries its own date in the middle of it: wind and
//! sun count now, biomass, landfill gas, sewage gas and biogas only **from
//! obligation year 2028**. [`RenewableSource::countable_from`] is where that
//! lives, and a claim on a source that does not count yet is refused.
//!
//! When the proof is incomplete the paragraph's own remedy is the grid
//! average, so [`EmissionsBasis::renewable`] returns the missing condition by
//! name and the caller reaches for [`EmissionsBasis::grid_average`]. The
//! fallback is stated rather than performed silently, because a notification
//! calculated on the wrong basis is one the authority recalculates and the
//! operator finds out about from the difference.
//!
//! # Where it stops
//!
//! At the figure the Verordnung defines. What an operator *sells* is the
//! difference against the reference value in § 37a of the
//! Bundes-Immissionsschutzgesetz, and that is the competent authority's
//! arithmetic over a fuel supplier's whole balance — not a fact about a charge
//! point. A crate that computed a price here would be inventing the half of
//! the calculation it cannot see.
//!
//! # It reads no clock
//!
//! The obligation year is an argument, the announcement is an argument, and
//! the eligibility of every point is judged on 31 December of the year being
//! filed for. A notification rebuilt three years later, when the register has
//! moved on and half the points are decommissioned, is the same file.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

pub mod claim;
pub mod error;
pub mod estimate;
pub mod factors;

pub use claim::{Attribution, Claim, ClaimBuilder, PointRecord, Route, Window};
pub use error::ThgError;
pub use estimate::{
    Estimate, EstimateClaim, EstimateClaimBuilder, RegistrationEvidence, VehicleRecord,
};
pub use factors::{
    BasisKind, DirectSupply, DriveEfficiency, EmissionsBasis, FIRST_COUNTED_YEAR, MJ_PER_KWH,
    RenewableSource, VehicleClass, counting_factor,
};
