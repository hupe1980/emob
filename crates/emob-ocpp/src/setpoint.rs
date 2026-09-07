//! The § 14a allocation, carried to the stations that have to obey it.
//!
//! # The other end of a wire whose middle is a rule
//!
//! `emob_smart::allocate` decides what each point of a site may draw under a
//! grid operator's setpoint. That decision is pure and it is law: `[EnWG §14a]`
//! and `[BK6-22-300]`, with a ceiling `Allocation` has no constructor outside
//! of. What it is not is a message. A charge point learns its share over OCPP,
//! and this is the crossing.
//!
//! It lives here for the reason the tariff crossing does — the seam is the only
//! place both vocabularies are in scope, and the crate that decides the rule
//! links no protocol implementation. `emob-smart` has never heard of OCPP; the
//! edge points this way and not the other.
//!
//! # `ChargingStationExternalConstraints`, and why it is not a preference
//!
//! `[BK6-22-300 Ziff. 4.6]` puts the grid operator's reduction **above**
//! *marktliche Laststeuerung* — a market plan, a tariff optimiser, a fleet
//! scheduler. `emob-smart` makes that structural by taking the market plan as
//! an input to a function whose ceiling is the setpoint, so no plan can ask for
//! more than the grid allows.
//!
//! On the wire, the same rule has to survive a profile stack that another actor
//! also writes into. OCPP resolves overlapping profiles by purpose and stack
//! level, and `ChargingStationExternalConstraints` is the purpose reserved for
//! *"an external actor"* — which is what a distribution grid operator is. A
//! setpoint sent as a `TxDefaultProfile` would sit in the same tier as the
//! optimiser it is supposed to outrank, and the station would resolve the two
//! by stack level: a number an operator typed, deciding a limit a regulator
//! set.
//!
//! So the purpose is fixed by this crate and is not a parameter. There is no
//! argument for which a § 14a setpoint is a transaction profile.
//!
//! # Watts, exactly
//!
//! `ChargingSchedulePeriod::limit` is an exact decimal on both sides of this
//! seam, so a kilowatt figure becomes a watt figure by multiplication and
//! nothing else happens to it. That is not a nicety: `[BK6-22-300 Ziff. 4.5]`'s
//! Mindestleistung is 4.2 kW, three-phase 6 A is 4.14 kW, and the difference
//! between them is sixty watts — a figure a rounding would eat and a compliance
//! question turns on.
//!
//! # What the wire cannot carry, and is therefore said
//!
//! An [`Allocation`] holds three things OCPP has no field for, and each is
//! reported through [`emob_core::Crossing`] rather than dropped:
//!
//! * the **ceiling** itself. OCPP addresses EVSEs; there is no message that
//!   says *"this site is held at 22 kW"*. The seats add up to it and the fact
//!   that they do is a property of the allocation, not of the profiles.
//! * the **reason**. `UnderFailsafe` and `RunningAutonomously` are two
//!   different sites at the same number `[LPC §2.3.2]`, and a profile is a
//!   number.
//! * the **headroom**, which tells a grid operator whether it is curtailing a
//!   site or watching an empty car park.

use emob_core::{Crossing, EvseId};
use emob_smart::{Allocation, SmartNote};
use ocpp_kit::v2_1::{
    ChargingProfile, ChargingProfileKind, ChargingProfilePurpose, ChargingRateUnit,
    ChargingSchedule, ChargingSchedulePeriod,
};
use rust_decimal::Decimal;

/// Watts in a kilowatt. The whole of the unit conversion, and it is exact.
const W_PER_KW: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);

/// The profile stack level a § 14a setpoint is installed at.
///
/// Zero, and deliberately the lowest. Level is how OCPP resolves two profiles
/// *of the same purpose*, and there is only ever one external constraint here —
/// the grid operator's. What puts this above a market plan is the **purpose**,
/// not the level, and a crate that reached for a high level instead would be
/// competing in the wrong tier.
const STACK_LEVEL: i32 = 0;

/// One point's share of a site's allocation, as a station is told it.
#[derive(Debug, Clone, PartialEq)]
pub struct Setpoint {
    /// Which point, in this workspace's own grammar.
    ///
    /// Not OCPP's integer `evseId`: that is a number a station was
    /// commissioned with, and which integer belongs to which
    /// [`EvseId`] is a fact about a deployment's provisioning
    /// records rather than about the allocation. The daemon that holds those
    /// records routes; this crate says what each point may draw.
    pub evse_id: EvseId,
    /// What that point may draw, in watts.
    pub watts: Decimal,
    /// The profile itself.
    pub profile: ChargingProfile,
}

/// Carry a site allocation onto OCPP 2.1 charging profiles, one per seated
/// point.
///
/// `profile_id` is the identity the station stores the profile under and a later
/// `ClearChargingProfile` addresses it by; it is the caller's because it has to
/// be unique on that station and this crate holds no station.
///
/// # A point with no seat gets no profile, and that is not the same as zero
///
/// `emob_smart::SmartNote::HeldAtZero` is a point that may not draw, and it is
/// sent a profile saying exactly that. `SmartNote::NotOnThisSite` is a point the
/// site does not control at all, and sending it anything would be this platform
/// limiting a device it has no relationship with. Only seated points appear
/// here.
///
/// ```
/// # use emob_ocpp::setpoint::to_charging_profiles;
/// # use emob_smart::{Control, Demand, GridLimit, Site, SteuVe, allocate};
/// # use emob_core::station::{Accessibility, ChargePointProfile};
/// # use rust_decimal::Decimal;
/// # use time::macros::datetime;
/// # let mut depot = ChargePointProfile::bare("DE*ABC*E00001".parse()?, datetime!(2026-01-01 0:00 UTC).date());
/// # depot.accessibility = Accessibility::Private;
/// let box_one = SteuVe::of(&depot, Control::Ems)?;
/// let site = Site { connection_kw: Decimal::from(22), devices: vec![box_one.clone()] };
/// let plan = [Demand { evse_id: box_one.evse_id().clone(), wanted_kw: Decimal::from(11) }];
/// let limit = GridLimit::curtailed(Decimal::from(5), datetime!(2026-06-01 18:00 +2));
///
/// let carried = to_charging_profiles(&allocate(&site, &plan, &limit), 1);
/// assert_eq!(carried.value.len(), 1);
/// // 5 kW, in watts, exactly.
/// assert_eq!(carried.value[0].watts, Decimal::from(5000));
/// // …and the site ceiling itself has no field on the wire, so it is said.
/// assert!(!carried.is_lossless());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn to_charging_profiles(allocation: &Allocation, profile_id: i32) -> Crossing<Vec<Setpoint>> {
    let mut setpoints = Vec::with_capacity(allocation.seats().len());
    let mut crossing = Crossing::lossless(());

    for (evse_id, kw) in allocation.seats() {
        let watts = *kw * W_PER_KW;
        // Through the same conversion a price crosses by. `ocpp-kit`'s decimal
        // is a 64-bit mantissa at a scale of at most eighteen and this
        // workspace's is 96 bits at twenty-eight, so a figure can be outside
        // it — and a limit that arrived rounded is a limit the station will
        // obey rather than the one the grid operator set.
        let Some((limit, carried)) = crate::tariff::to_wire(watts) else {
            crossing.note(
                format!("/{evse_id}/chargingSchedulePeriod/0/limit"),
                format!(
                    "{watts} W is not a figure OCPP's decimal can carry, so this point is sent \
                     no profile at all: a limit rounded on the way out is one the station \
                     obeys and the grid operator did not set [BK6-22-300 Ziff. 4.6]"
                ),
            );
            continue;
        };
        if carried != watts {
            crossing.note(
                format!("/{evse_id}/chargingSchedulePeriod/0/limit"),
                format!(
                    "{watts} W crosses as {carried} W: OCPP's decimal is narrower than this \
                     workspace's, and the difference is against the site rather than for it"
                ),
            );
        }
        setpoints.push(Setpoint {
            evse_id: evse_id.clone(),
            watts: carried,
            profile: profile_for(profile_id, limit),
        });
    }

    // The three facts an allocation holds and a profile has no field for. Each
    // is reported by JSON Pointer into the document the station will read, the
    // way every other crossing in this workspace accounts for what it could not
    // carry.
    crossing.note(
        "/chargingProfile",
        format!(
            "the site is held at {} kW and OCPP addresses EVSEs: there is no message that states \
             a site ceiling, so that the seats add up to it is a property of the allocation and \
             not of these profiles [BK6-22-300 Ziff. 4.6]",
            allocation.ceiling_kw()
        ),
    );
    if allocation.headroom_kw() > Decimal::ZERO {
        crossing.note(
            "/chargingProfile",
            format!(
                "{} kW of the ceiling went unused: a grid operator reading these profiles is \
                 reading a site that is not being curtailed by them",
                allocation.headroom_kw()
            ),
        );
    }
    for note in allocation.notes() {
        // The two states `[LPC §2.3.2]` tells apart and a number cannot: a site
        // at 4.2 kW under a failsafe and one at 4.2 kW under a setpoint are the
        // same profile and different problems, and only one of them is a
        // control box to go and replace.
        if matches!(
            note,
            SmartNote::UnderFailsafe { .. } | SmartNote::RunningAutonomously { .. }
        ) {
            crossing.note("/chargingProfile", note.to_string());
        }
    }

    crossing.map(|()| setpoints)
}

/// One profile, at one limit, valid until something replaces it.
fn profile_for(id: i32, limit: ocpp_kit::types::Decimal) -> ChargingProfile {
    // Through the kit's own constructors rather than a struct literal. A 2.1
    // `ChargingSchedulePeriod` has twenty-two optional fields, and a literal
    // here would be this crate deciding, silently, what each of them is — and
    // would keep compiling with a new one defaulted to whatever a release
    // chose.
    let period = ChargingSchedulePeriod::new(0).with_limit(limit);

    let schedule = ChargingSchedule::new(
        id,
        // Watts, because § 14a is about power at a connection point and amperes
        // are about a conductor. A site whose points are on different phase
        // counts cannot be held to one ampere figure.
        ChargingRateUnit::W,
        vec![period],
    );

    ChargingProfile::new(
        id,
        STACK_LEVEL,
        // See the module documentation. Not a parameter, and not a preference.
        ChargingProfilePurpose::ChargingStationExternalConstraints,
        // `Absolute` with no `startSchedule`: the limit applies from the moment
        // the station has it and until it is replaced, which is what a grid
        // operator's setpoint is. A `Recurring` profile would be a schedule,
        // and § 14a's instruction has no period — it holds until the next one.
        ChargingProfileKind::Absolute,
        vec![schedule],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_core::station::{Accessibility, ChargePointProfile};
    use emob_smart::{Control, Demand, GridLimit, Site, SteuVe, allocate};
    use time::macros::{date, datetime};

    fn depot(n: usize, connection_kw: Decimal) -> SteuVe {
        let mut point = ChargePointProfile::bare(
            format!("DE*ABC*E{n:05}").parse().expect("a valid EVSE id"),
            date!(2026 - 01 - 01),
        );
        point.accessibility = Accessibility::Private;
        point.grid.capacity_kw = connection_kw;
        SteuVe::of(&point, Control::Ems).expect("a private point over the threshold")
    }

    fn at() -> time::OffsetDateTime {
        datetime!(2026-06-01 18:00 +2)
    }

    #[test]
    fn a_setpoint_crosses_as_a_constraint_an_optimiser_cannot_outrank() {
        // `[BK6-22-300 Ziff. 4.6]` puts the grid operator's reduction above
        // *marktliche Laststeuerung*. On the wire that is the **purpose**: a
        // `TxDefaultProfile` would sit in the same tier as the optimiser it
        // outranks, and the station would resolve the two by a stack level an
        // operator typed.
        let device = depot(1, Decimal::from(11));
        let site = Site {
            connection_kw: Decimal::from(22),
            devices: vec![device.clone()],
        };
        let plan = [Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        }];
        let allocation = allocate(&site, &plan, &GridLimit::curtailed(Decimal::from(5), at()));

        let carried = to_charging_profiles(&allocation, 42);
        let setpoints = &carried.value;
        assert_eq!(setpoints.len(), 1);
        assert_eq!(
            setpoints[0].profile.charging_profile_purpose,
            ChargingProfilePurpose::ChargingStationExternalConstraints
        );
        assert_eq!(
            setpoints[0].profile.charging_profile_kind,
            ChargingProfileKind::Absolute
        );
    }

    #[test]
    fn the_kilowatt_figure_reaches_the_station_in_watts_and_exactly() {
        // Sixty watts is the distance between `[BK6-22-300 Ziff. 4.5]`'s 4.2 kW
        // Mindestleistung and three-phase 6 A, which is 4.14 kW. A rounding
        // eats it and a compliance question turns on it.
        let device = depot(1, Decimal::from(11));
        let site = Site {
            connection_kw: Decimal::from(22),
            devices: vec![device.clone()],
        };
        let plan = [Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        }];
        let limit = GridLimit::curtailed(Decimal::from_str_exact("4.14").expect("a decimal"), at());
        let allocation = allocate(&site, &plan, &limit);

        let carried = to_charging_profiles(&allocation, 1);
        let setpoints = &carried.value;
        // …raised to the Mindestleistung, because the Anspruch holds — and
        // carried as watts to the last one.
        assert_eq!(setpoints[0].watts, Decimal::from(4200));
        let schedule = &setpoints[0].profile.charging_schedule[0];
        assert_eq!(schedule.charging_rate_unit, ChargingRateUnit::W);
        // `4200.00` rather than `4200`: `to_wire` walks the scale **down**
        // from what the value carries and never below it, because a scale is a
        // statement the sender made and a conversion has no business weakening
        // one it did not have to. The figure is the same 4200 W either way.
        assert_eq!(
            schedule.charging_schedule_period[0]
                .limit
                .expect("a limit")
                .to_string(),
            "4200.00"
        );
    }

    #[test]
    fn the_three_things_a_profile_has_no_field_for_are_said_rather_than_dropped() {
        let device = depot(1, Decimal::from(11));
        let site = Site {
            connection_kw: Decimal::from(22),
            devices: vec![device.clone()],
        };
        // Nobody plugged in: the demand asks for less than the ceiling, so the
        // site is not being curtailed at all — which is the difference between
        // a network problem and a Tuesday.
        let plan = [Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(4),
        }];
        let allocation = allocate(&site, &plan, &GridLimit::curtailed(Decimal::from(11), at()));

        let carried = to_charging_profiles(&allocation, 1);
        assert!(!carried.is_lossless(), "a profile is a number");
        let reasons: Vec<String> = carried.reasons().collect();
        assert!(
            reasons.iter().any(|r| r.contains("no message that states")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|r| r.contains("went unused")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_site_running_on_its_own_looks_like_a_curtailed_one_and_says_which_it_is() {
        // `[LPC §2.3.2]` gives a site five states; a `ChargingSchedulePeriod`
        // gives it one number. A depot at full power because its grid operator
        // allows it and one at full power because the control box died four
        // hours ago are the same profile and different problems.
        let device = depot(1, Decimal::from(11));
        let site = Site {
            connection_kw: Decimal::from(22),
            devices: vec![device.clone()],
        };
        let plan = [Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        }];
        let allocation = allocate(
            &site,
            &plan,
            &GridLimit::Autonomous {
                silent_since: at() - time::Duration::hours(4),
            },
        );

        let reasons: Vec<String> = to_charging_profiles(&allocation, 1).reasons().collect();
        assert!(
            reasons.iter().any(|r| r.contains("[LPC-906]")),
            "the dangerous quiet is on the record: {reasons:?}"
        );
    }
}
