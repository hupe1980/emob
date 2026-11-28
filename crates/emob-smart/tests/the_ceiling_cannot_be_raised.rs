//! **M6b's exit criterion: a § 14a ceiling proven unoverridable.**
//!
//! `[BK6-22-300 Ziff. 4.6]` makes the grid operator's reduction take precedence
//! over *"anderweitige Steuerungsmaßnahmen, insbesondere marktlicher
//! Laststeuerung"*. Every one of those — a dynamic tariff, a fleet's departure
//! schedule, a driver in a hurry — is a reason somebody writes code that reaches
//! past the limit, so the claim worth proving is not that the limit is checked
//! but that there is **no way to be outside it**.
//!
//! That is a property over generated input rather than an example, because an
//! example is a shape somebody already thought of: the sites, the demands and
//! the setpoints below are drawn rather than written, and the assertion is asked
//! of every one of them.

use emob_core::EvseId;
use emob_core::station::{Accessibility, ChargePointProfile};
use emob_smart::{Control, Demand, GridLevel, GridLimit, Site, SmartNote, SteuVe, allocate};
use rust_decimal::Decimal;
use time::macros::{date, datetime};

/// SplitMix64 — the workspace takes no `rand`, and a replayable property test
/// wants a seeded sequence anyway.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }
}

fn dec(units: u64, scale: u32) -> Decimal {
    Decimal::new(i64::try_from(units).unwrap_or(i64::MAX), scale)
}

fn point(id: &str, accessibility: Accessibility, connection_kw: Decimal) -> ChargePointProfile {
    let mut point =
        ChargePointProfile::bare(id.parse().expect("a valid EVSE id"), date!(2026 - 01 - 01));
    point.accessibility = accessibility;
    point.grid.capacity_kw = connection_kw;
    point
}

fn evse(n: usize) -> EvseId {
    format!("DE*ABC*E{n:05}").parse().expect("a valid EVSE id")
}

fn depot(n: usize, connection_kw: Decimal, control: Control) -> SteuVe {
    SteuVe::of(
        &point(
            &format!("DE*ABC*E{n:05}"),
            Accessibility::Private,
            connection_kw,
        ),
        control,
    )
    .expect("a private point over the threshold in low voltage")
}

#[test]
fn no_demand_and_no_market_plan_can_put_a_site_above_its_ceiling() {
    let mut rng = Rng(0x14A_5EED);
    let at = datetime!(2026-06-01 18:00 +2);

    for case in 0..4000_u64 {
        // A yard of one to nine boxes, each connected somewhere over the
        // threshold, some on the energy-management system and some addressed
        // directly — which is the ordinary shape and the one `Ziff. 4.4` makes a
        // per-device decision.
        let count = usize::try_from(rng.between(1, 9)).unwrap_or(1);
        let devices: Vec<SteuVe> = (0..count)
            .map(|n| {
                let connection = dec(rng.between(43, 350), 1);
                let control = if rng.between(0, 3) == 0 {
                    Control::Direct
                } else {
                    Control::Ems
                };
                let device = depot(n, connection, control);
                if rng.between(0, 4) == 0 {
                    device.switching_to_single_phase()
                } else {
                    device
                }
            })
            .collect();
        let site = Site {
            connection_kw: dec(rng.between(40, 900), 1),
            devices: devices.clone(),
        };

        // A market plan that wants everything, which is the competing
        // instruction `[BK6-22-300 Ziff. 4.6]` is about.
        let plan: Vec<Demand> = devices
            .iter()
            .map(|device| Demand {
                evse_id: device.evse_id().clone(),
                wanted_kw: dec(rng.between(0, 400), 1),
            })
            .collect();

        // All five of `[LPC §2.3.2]`'s states, because the ceiling has to hold
        // under every one of them — including the two that state no ceiling for
        // a reason and the one whose ceiling is a communications fault.
        let limit = match rng.between(0, 6) {
            0 => GridLimit::Uncontrolled,
            4 => GridLimit::Initializing {
                limit_kw: dec(rng.between(0, 600), 1),
            },
            1 => GridLimit::InContact { heard_at: at },
            2 => GridLimit::Autonomous { silent_since: at },
            3 => GridLimit::failsafe(dec(rng.between(0, 600), 1), at),
            _ => GridLimit::curtailed(dec(rng.between(0, 600), 1), at),
        };

        let allocation = allocate(&site, &plan, &limit);

        // ── 1. Never above the ceiling ─────────────────────────────────────
        assert!(
            allocation.within_ceiling(),
            "case {case}: {} kW allocated under a ceiling of {} kW",
            allocation.total_kw(),
            allocation.ceiling_kw()
        );

        // ── 2. …and never above what the site is physically connected at ───
        assert!(
            allocation.total_kw() <= site.connection_kw,
            "case {case}: a setpoint cannot raise a transformer"
        );

        // ── 3. A seated point is at or above its own floor ─────────────────
        //
        // `DIN EN 61851-1` has no duty cycle below 6 A, so a number between
        // zero and the floor is not an instruction — it is a session that
        // stops while the allocation reports it running.
        for (id, allocated) in allocation.seats() {
            let device = devices
                .iter()
                .find(|device| device.evse_id() == id)
                .expect("a seat belongs to a device");
            assert!(
                *allocated >= device.technical_floor_kw(),
                "case {case}: {id} seated at {allocated} kW, below its {} kW floor",
                device.technical_floor_kw()
            );
            // …and never above what it asked for, or what it is connected at.
            let wanted = plan
                .iter()
                .find(|want| &want.evse_id == id)
                .map(|want| want.wanted_kw)
                .expect("a seat belongs to a demand");
            assert!(
                *allocated <= wanted.max(device.technical_floor_kw()),
                "case {case}: {id} was given more than it asked for"
            );
            assert!(
                *allocated <= device.connection_kw().max(device.technical_floor_kw()),
                "case {case}: {id} was given more than it is connected at"
            );
        }

        // ── 4. The Anspruch is never given up in silence ───────────────────
        //
        // A ceiling below the Mindestleistung is one the grid operator may not
        // impose, and the site holds at the minimum. Where it does, it says so
        // — and this holds for *any* stated ceiling, so a failsafe cannot take
        // away under a communications fault what a setpoint may not take away
        // in the open.
        if let Some(stated_kw) = limit.stated_kw()
            && stated_kw < site.minimum().kw
        {
            assert!(
                allocation
                    .notes()
                    .iter()
                    .any(|note| matches!(note, SmartNote::SetpointBelowMinimum { .. })),
                "case {case}: a ceiling of {stated_kw} kW under a minimum of {} kW was obeyed \
                 silently, out of {limit:?}",
                site.minimum().kw
            );
        }

        // ── 5. Silence is never permission, and never invisible ────────────
        //
        // The two states that come out of a grid operator not speaking always
        // put their note on the allocation, whatever else the case generated.
        match limit {
            GridLimit::UnderFailsafe(_) => assert!(
                allocation
                    .notes()
                    .iter()
                    .any(|note| matches!(note, SmartNote::UnderFailsafe { .. })),
                "case {case}: a site held at its failsafe reported an ordinary ceiling"
            ),
            GridLimit::Autonomous { .. } => assert!(
                allocation
                    .notes()
                    .iter()
                    .any(|note| matches!(note, SmartNote::RunningAutonomously { .. })),
                "case {case}: a site its grid operator can no longer reach looked ordinary"
            ),
            GridLimit::Initializing { .. }
            | GridLimit::Uncontrolled
            | GridLimit::Curtailed(_)
            | GridLimit::InContact { .. } => {}
        }
    }
}

#[test]
fn a_publicly_accessible_point_is_not_a_steuve_at_all() {
    // The condition every summary of the Festlegung drops, and it is the first
    // one: `[BK6-22-300 Ziff. 2.4.1]` excludes an öffentlich zugänglicher
    // Ladepunkt by name. Reading it the other way round — "a charge park is a
    // SteuVE at its grid connection" — puts a public site under a regime it is
    // not in.
    let forecourt = point("DE*ABC*E00001", Accessibility::Public, Decimal::from(150));
    let refused =
        SteuVe::of(&forecourt, Control::Ems).expect_err("a public fast charger is outside § 14a");
    assert!(
        refused.to_string().contains("publicly accessible"),
        "{refused}"
    );

    // …and the other two conditions are two different situations, which is why
    // they are two answers.
    let small = SteuVe::of(
        &point(
            "DE*ABC*E00002",
            Accessibility::Private,
            emob_smart::THRESHOLD_KW,
        ),
        Control::Ems,
    )
    .expect_err("the Festlegung reaches **more than** 4.2 kW");
    assert!(small.to_string().contains("more than"), "{small}");

    let mut on_medium = point("DE*ABC*E00003", Accessibility::Private, Decimal::from(400));
    on_medium.grid.level = GridLevel::Above;
    let medium = SteuVe::of(&on_medium, Control::Ems)
        .expect_err("a medium-voltage site answers to its connection contract");
    assert!(medium.to_string().contains("low voltage"), "{medium}");
}

#[test]
fn the_minimum_counts_only_the_devices_on_the_energy_management_system() {
    // `[BK6-22-300 Ziff. 4.5.2]`: *"nsteuVE = Anzahl aller steuerbarer
    // Verbrauchseinrichtungen, die nach Ziffer 4.4.b angesteuert werden"* — the
    // ones on the EMS, and not every device at the site.
    //
    // The direction the mistake falls in is the one that hurts. A directly
    // controlled device is owed its 4.2 kW **whole**, with no simultaneity
    // factor applied to it, so folding it into the EMS group **under**-claims —
    // and an operator that under-claims accepts a curtailment deeper than the
    // Festlegung permits, with nothing anywhere reporting that it did.
    let eleven = Decimal::from(11);
    let mixed = vec![
        depot(1, eleven, Control::Direct),
        depot(2, eleven, Control::Direct),
        depot(3, eleven, Control::Ems),
        depot(4, eleven, Control::Ems),
        depot(5, eleven, Control::Ems),
        depot(6, eleven, Control::Ems),
        depot(7, eleven, Control::Ems),
        depot(8, eleven, Control::Ems),
    ];
    let site = Site {
        connection_kw: Decimal::from(88),
        devices: mixed,
    };

    // Two direct at 4.2 each, plus six on the EMS at 4.2 + 5 × 0.6 × 4.2.
    let minimum = site.minimum();
    assert_eq!(minimum.kw, dec(252, 1), "{minimum:?}");
    assert_eq!(
        minimum.rule,
        emob_smart::MinimumRule::EmsGroup {
            devices: 6,
            factor: Some(dec(60, 2)),
        },
        "the factor is the one for six, not for eight"
    );

    // …and folding the two direct boxes into the group gives 4.2 + 7 × 0.5 ×
    // 4.2 = 18.9 kW: six and a third kilowatts the site would have stopped
    // asking for.
    let all_on_ems = Site {
        connection_kw: Decimal::from(88),
        devices: (1..=8).map(|n| depot(n, eleven, Control::Ems)).collect(),
    };
    assert_eq!(all_on_ems.minimum().kw, dec(189, 1));
    assert_eq!(minimum.kw - all_on_ems.minimum().kw, dec(63, 1));
}

#[test]
fn a_directly_controlled_charge_point_over_eleven_kilowatts_is_still_owed_four_point_two() {
    // `[BK6-22-300 Ziff. 4.5.1]`'s second sentence scales a device over 11 kW by
    // 0.4 — and it names `2.4.1.b` and `2.4.1.c`, heat pumps and space cooling.
    // A charge point is `2.4.1.a`. Reading the sentence as though it applied to
    // everything would have a 22 kW box claim 8.8 kW it is not owed.
    let site = Site {
        connection_kw: Decimal::from(22),
        devices: vec![depot(1, Decimal::from(22), Control::Direct)],
    };
    assert_eq!(site.minimum().kw, emob_smart::THRESHOLD_KW);
    assert_eq!(site.minimum().rule, emob_smart::MinimumRule::PerDevice);
}

#[test]
fn a_point_is_at_its_floor_or_at_zero_and_never_between() {
    // The arithmetic a proportional share gets wrong. Three three-phase boxes
    // under an 8 kW ceiling: a proportional split gives each 2.67 kW, which no
    // pilot can signal, and the site draws nothing while reporting that it
    // allocated everything.
    let devices: Vec<SteuVe> = (1..=3)
        .map(|n| depot(n, Decimal::from(11), Control::Ems))
        .collect();
    let site = Site {
        connection_kw: Decimal::from(33),
        devices: devices.clone(),
    };
    let plan: Vec<Demand> = devices
        .iter()
        .map(|device| Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        })
        .collect();

    // The minimum for three on an EMS is 4.2 + 2 × 0.75 × 4.2 = 10.5, so an
    // 8 kW setpoint is one the grid operator may not give and the site holds at
    // 10.5 — which seats two boxes at 4.14 and leaves the third at zero.
    let allocation = allocate(
        &site,
        &plan,
        &GridLimit::curtailed(Decimal::from(8), datetime!(2026-06-01 18:00 +2)),
    );

    let floor = dec(414, 2);
    for (_, allocated) in allocation.seats() {
        assert!(*allocated >= floor, "{allocated} is below a pilot's 6 A");
    }
    assert_eq!(allocation.for_point(&evse(3)), Decimal::ZERO);
    assert!(
        allocation
            .notes()
            .iter()
            .any(|note| matches!(note, SmartNote::HeldAtZero { .. })),
        "{:?}",
        allocation.reasons().collect::<Vec<_>>()
    );
    assert!(allocation.within_ceiling());
}

#[test]
fn a_point_that_can_drop_to_one_phase_reaches_the_lower_floor() {
    // The option the revision of `DIN EN 61851-1` adds, and it is a fact about
    // the hardware rather than about the regulation: a box that can switch to a
    // single phase mid-session signals down to about 1.38 kW, and one that
    // cannot stops at 4.14. `[BK6-22-300 Ziff. 4.6]`'s *"nächstgeringerer Wert,
    // der technisch möglich ist"* is two different numbers for two boxes on one
    // site.
    let three_phase = depot(1, Decimal::from(11), Control::Ems);
    let switching = depot(2, Decimal::from(11), Control::Ems).switching_to_single_phase();
    assert_eq!(three_phase.technical_floor_kw(), dec(414, 2));
    assert_eq!(switching.technical_floor_kw(), dec(138, 2));
}

#[cfg(feature = "serde")]
#[test]
fn a_device_the_festlegung_does_not_reach_cannot_arrive_over_the_wire_either() {
    // Rule 13: a value arrives through a door, and the door is not the
    // constructor you wrote. `SteuVe::of` refuses a point at or below the
    // threshold; a derived `Deserialize` would restore one out of a site
    // controller's own state and then answer a Mindestleistung question about a
    // device § 14a does not bind.
    let stored = serde_json::json!({
        "evse_id": "DE*ABC*E00001",
        "connection_kw": "4.2",
        "phases": 3,
        "switches_to_single_phase": false,
        "control": "ems"
    });
    let refused = serde_json::from_value::<SteuVe>(stored);
    assert!(refused.is_err(), "{refused:?}");

    // …and a phase count no charge point has.
    let phaseless = serde_json::json!({
        "evse_id": "DE*ABC*E00001",
        "connection_kw": "11",
        "phases": 0,
        "switches_to_single_phase": false,
        "control": "ems"
    });
    assert!(serde_json::from_value::<SteuVe>(phaseless).is_err());

    // …and the ordinary one survives the round trip.
    let device = depot(1, Decimal::from(11), Control::Ems);
    let json = serde_json::to_string(&device).expect("it serialises");
    assert_eq!(
        serde_json::from_str::<SteuVe>(&json).expect("it comes back"),
        device
    );
}

#[test]
fn a_single_phase_box_over_the_threshold_has_the_lower_floor_for_a_different_reason() {
    // Two ways to reach 1.38 kW, and they are different facts. A 32 A box wired
    // on **one** phase draws 7.36 kW flat out and can be signalled down to
    // 6 A × 230 V; a three-phase box that can *switch* to one mid-session gets
    // there too. Both are over the 4.2 kW threshold and both are steuerbare
    // Verbrauchseinrichtungen, and a site holding one of each can be curtailed
    // further than a site of plain three-phase boxes.
    let wired = depot(1, dec(736, 2), Control::Ems).on_phases(1);
    let switching = depot(2, Decimal::from(11), Control::Ems).switching_to_single_phase();
    let plain = depot(3, Decimal::from(11), Control::Ems);

    assert_eq!(wired.technical_floor_kw(), dec(138, 2));
    assert_eq!(switching.technical_floor_kw(), dec(138, 2));
    assert_eq!(plain.technical_floor_kw(), dec(414, 2));

    // …and a site of the two low-floor boxes fits under a setpoint that a site
    // of two plain ones does not, which is the whole point of `Ziff. 4.6`'s
    // "next lower technically possible value".
    let low = Site {
        connection_kw: Decimal::from(22),
        devices: vec![wired.clone(), switching.clone()],
    };
    let plan = |devices: &[SteuVe]| -> Vec<Demand> {
        devices
            .iter()
            .map(|device| Demand {
                evse_id: device.evse_id().clone(),
                wanted_kw: Decimal::from(11),
            })
            .collect()
    };
    // Two on an EMS are owed 7.56, so a 4 kW setpoint is held at 7.56 either way
    // — and there the difference shows: two 1.38 floors both seat, two 4.14 ones
    // do not.
    let at = datetime!(2026-06-01 18:00 +2);
    let curtailed = GridLimit::curtailed(Decimal::from(4), at);
    let allocation = allocate(&low, &plan(&[wired, switching]), &curtailed);
    assert_eq!(
        allocation.seats().len(),
        2,
        "{:?}",
        allocation.reasons().collect::<Vec<_>>()
    );

    let high = Site {
        connection_kw: Decimal::from(22),
        devices: vec![plain.clone(), depot(4, Decimal::from(11), Control::Ems)],
    };
    let allocation = allocate(&high, &plan(&high.devices.clone()), &curtailed);
    assert_eq!(
        allocation.seats().len(),
        1,
        "7.56 kW seats one 4.14 floor and not two"
    );
    assert!(
        allocation
            .notes()
            .iter()
            .any(|note| matches!(note, SmartNote::HeldAtZero { .. }))
    );
}

#[test]
fn the_headroom_is_what_a_site_controller_reads_before_it_admits_another_car() {
    // The figure a site answers *"can one more plug in"* with, and the one that
    // tells a grid operator "we are not being held down by you". A ceiling of
    // 22 kW with one 11 kW box on it has eleven kilowatts of it unused, and
    // that is a Tuesday rather than a network problem.
    let device = depot(1, Decimal::from(11), Control::Ems);
    let site = Site {
        connection_kw: Decimal::from(22),
        devices: vec![device.clone()],
    };
    let plan = [Demand {
        evse_id: device.evse_id().clone(),
        wanted_kw: Decimal::from(11),
    }];
    let allocation = allocate(&site, &plan, &GridLimit::Uncontrolled);

    assert_eq!(allocation.total_kw(), Decimal::from(11));
    assert_eq!(allocation.headroom_kw(), Decimal::from(11));
    assert!(
        allocation
            .notes()
            .iter()
            .any(|note| matches!(note, SmartNote::Headroom { .. }))
    );

    // …and a site using all of it has none, which is the other answer.
    let full = Site {
        connection_kw: Decimal::from(11),
        devices: vec![device.clone()],
    };
    assert_eq!(
        allocate(&full, &plan, &GridLimit::Uncontrolled).headroom_kw(),
        Decimal::ZERO
    );
}

/// `[LPC §2.3.2]` gives a site five states and three of them allow full power.
/// They are not one fact: a site drawing everything because its grid operator
/// says it may, and one drawing everything because its control box died four
/// hours ago, are the same number and different problems, and a single "no
/// limit" for all three cannot write the difference down, let alone report it.
#[test]
fn silence_from_the_grid_operator_is_not_permission() {
    let device = depot(1, Decimal::from(22), Control::Direct);
    let site = Site {
        connection_kw: Decimal::from(22),
        devices: vec![device.clone()],
    };
    let plan = [Demand {
        evse_id: device.evse_id().clone(),
        wanted_kw: Decimal::from(22),
    }];
    let silent_since = datetime!(2026-06-01 14:00 +2);

    // 120 seconds after the last heartbeat the site is on its failsafe value
    // `[LPC-912]`, not on its connection — and it says so, because a depot at
    // 4.2 kW under a failsafe and one under a setpoint look identical.
    let under_failsafe = allocate(&site, &plan, &GridLimit::failsafe(dec(42, 1), silent_since));
    assert_eq!(under_failsafe.ceiling_kw(), dec(42, 1));
    let note = under_failsafe
        .notes()
        .iter()
        .find(|note| matches!(note, SmartNote::UnderFailsafe { .. }))
        .expect("a site held at its failsafe says so");
    let reason = note.to_string();
    assert!(reason.contains(&silent_since.to_string()), "{reason}");
    assert!(reason.contains("failsafe limit of 4.2 kW"), "{reason}");

    // …and after the Failsafe Duration Minimum it is released `[LPC-922]`, back
    // on its own connection — which is correct, and is the state a grid
    // operator most needs told about, because it believes it can still curtail
    // this site and it cannot.
    let autonomous = allocate(&site, &plan, &GridLimit::Autonomous { silent_since });
    assert_eq!(autonomous.ceiling_kw(), Decimal::from(22));
    assert!(
        autonomous
            .notes()
            .iter()
            .any(|note| matches!(note, SmartNote::RunningAutonomously { .. })),
        "a site running without its grid operator is not allowed to look ordinary"
    );

    // The other two unlimited states are ordinary and say nothing extra: one
    // grid operator allowing full power, and one site with no control box.
    for quiet in [
        GridLimit::Uncontrolled,
        GridLimit::InContact {
            heard_at: silent_since,
        },
    ] {
        let allocation = allocate(&site, &plan, &quiet);
        assert_eq!(allocation.ceiling_kw(), Decimal::from(22));
        assert!(
            !allocation.notes().iter().any(|note| matches!(
                note,
                SmartNote::UnderFailsafe { .. } | SmartNote::RunningAutonomously { .. }
            )),
            "{quiet:?} is not a fault and must not be reported as one"
        );
    }

    // And every state this crate models names the one LPC gives it, except the
    // site LPC has no state for, because it is running no state machine.
    assert_eq!(GridLimit::Uncontrolled.lpc_state(), None);
    assert_eq!(
        GridLimit::curtailed(Decimal::ONE, silent_since).lpc_state(),
        Some("limited")
    );
    assert_eq!(
        GridLimit::Autonomous { silent_since }.lpc_state(),
        Some("unlimited/autonomous")
    );
}

/// The Mindestleistung is a claim against *the Festlegung's mechanism*, not
/// against a grid operator's good mood. `[BK6-22-300 Ziff. 4.5]` guarantees the
/// figure is a draw the grid can always carry, so a failsafe below it is not
/// the cautious reading — it gives the claim up for a reason the Festlegung
/// does not recognise, at the one moment nobody is watching.
#[test]
fn the_anspruch_holds_against_a_failsafe_as_it_holds_against_a_setpoint() {
    let devices: Vec<SteuVe> = (1..=2)
        .map(|n| depot(n, Decimal::from(11), Control::Ems))
        .collect();
    let site = Site {
        connection_kw: Decimal::from(22),
        devices: devices.clone(),
    };
    let plan: Vec<Demand> = devices
        .iter()
        .map(|device| Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        })
        .collect();

    // Two on one EMS are owed 4.2 + 0.8 × 4.2 = 7.56 kW.
    let owed = dec(756, 2);
    let at = datetime!(2026-06-01 14:00 +2);
    let floor_kw = dec(42, 1);

    for (limit, what) in [
        (GridLimit::curtailed(floor_kw, at), "a setpoint"),
        (GridLimit::failsafe(floor_kw, at), "a failsafe"),
    ] {
        let allocation = allocate(&site, &plan, &limit);
        assert_eq!(allocation.ceiling_kw(), owed, "{what} below the Anspruch");
        assert!(
            allocation
                .notes()
                .iter()
                .any(|note| matches!(note, SmartNote::SetpointBelowMinimum { .. })),
            "{what} below the Anspruch is held at it, and said so"
        );
    }
}

/// The two parties on either side of a setpoint have to agree about what is
/// behind the connection, and only one of them can say when they do not.
#[test]
fn a_demand_for_a_point_this_site_does_not_control_is_named_rather_than_dropped() {
    // A demand for an EVSE that is not one of this site's steuerbare
    // Verbrauchseinrichtungen falls out of the seating loop with nothing to
    // seat it against — correctly, because this crate has no floor, no
    // connection rating and no `Control` for it, and seating it on a guess
    // would put a number under the ceiling that no rule produced.
    //
    // What it must not do is fall out *silently*. The point draws whatever it
    // draws, `within_ceiling` cannot see it, and `[BK6-22-300 Ziff. 4.6]`'s
    // ceiling is then being judged against a smaller site than the one that
    // exists. That is a fact about the **site** rather than about a box, which
    // is why it is a third note beside `HeldAtZero` and `Curtailed`.
    let device = depot(1, Decimal::from(11), Control::Ems);
    let site = Site {
        connection_kw: Decimal::from(22),
        devices: vec![device.clone()],
    };
    let plan = [
        Demand {
            evse_id: device.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        },
        Demand {
            evse_id: evse(99),
            wanted_kw: Decimal::from(11),
        },
    ];

    let allocation = allocate(&site, &plan, &GridLimit::Uncontrolled);
    assert!(
        allocation.notes().iter().any(|note| matches!(
            note,
            SmartNote::NotOnThisSite { evse_id } if evse_id == &evse(99).to_string()
        )),
        "{:?}",
        allocation.notes()
    );
    // …and the point it does not control is given nothing, because there is
    // nothing lawful to give it.
    assert_eq!(allocation.for_point(&evse(99)), Decimal::ZERO);
    assert!(allocation.within_ceiling());

    // The reason reaches an operator as prose, with the paragraph on it.
    assert!(
        allocation
            .reasons()
            .any(|reason| reason.contains("[BK6-22-300 Ziff. 4.6]")
                && reason.contains("disagree about what is behind the connection")),
        "{:?}",
        allocation.reasons().collect::<Vec<_>>()
    );
}
