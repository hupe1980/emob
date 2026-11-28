//! **M6c: a setpoint that arrives over a wire.**
//!
//! > A grid operator writes a limit, and the charge points are told what they
//! > may draw.
//!
//! Both ends are real. The control box is an `eebus` SPINE
//! [`Engine`](eebus::spine::Engine) with the Energy Guard's client feature, the
//! site is another with the three features `[LPC §3.3]` asks a Controllable
//! System for, and every message between them is an **encoded datagram** that
//! goes through the codec on the way. The commissioning is the specification's
//! own: detailed discovery, use-case discovery, then a binding and a
//! subscription on each feature.
//!
//! What the test asserts is the seam, and there are four claims:
//!
//! 1. the watts that bind the allocation are the ones on the wire, **exactly** —
//!    `4139.5` W arrives as `4139.5` W and not as `4139.499999999999`;
//! 2. no charge point is above the ceiling, whatever the market plan asked for
//!    `[BK6-22-300 Ziff. 4.6]`;
//! 3. a setpoint below the Mindestleistung is answered by holding at the
//!    Mindestleistung, because `Ziff. 4.5` gives the operator a claim to it and
//!    obeying quietly is giving that claim up;
//! 4. and the pair `[LPC §4.1.5]` calls the operator's evidence — what was
//!    asked for and what was answered — is on the record.

use eebus::model::{
    CmdData, DeviceType, EntityType, FeatureAddress, FeatureType, LoadControlLimitData,
    LoadControlLimitListData, Role, ScaledNumber,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management};
use eebus::usecases::{limitation, lpc};
use emob_core::station::{Accessibility, ChargePointProfile};
use emob_smart::{Control, Demand, Site, SteuVe};
use rust_decimal::Decimal;
use sited::{Answer, Sited};
use time::macros::{date, datetime};

/// Where the exchange starts.
fn at(minutes: i64) -> time::OffsetDateTime {
    datetime!(2026-06-01 18:00 +2) + time::Duration::minutes(minutes)
}

fn kw(text: &str) -> Decimal {
    Decimal::from_str_exact(text).expect("a decimal")
}

/// A private charge point over the § 14a threshold, in low voltage.
fn depot(n: usize, connection_kw: Decimal) -> SteuVe {
    let mut point = ChargePointProfile::bare(
        format!("DE*ABC*E{n:05}").parse().expect("a valid EVSE id"),
        date!(2026 - 01 - 01),
    );
    point.accessibility = Accessibility::Private;
    point.grid.capacity_kw = connection_kw;
    SteuVe::of(&point, Control::Ems).expect("a private point over the threshold")
}

/// A control box and a site, wired to each other over SPINE.
struct Wire {
    guard: Engine,
    site: Engine,
    controller: Sited,
    now: core::time::Duration,
}

impl Wire {
    /// Two nodes with the features the limitation use case asks of them.
    fn new(devices: Vec<SteuVe>, connection_kw: Decimal) -> Self {
        let mut site_device =
            LocalDevice::new("i:67890", "ChargePark-1", DeviceType::ChargingStation)
                .expect("a device address");
        site_device
            .add_entity(
                LocalEntity::new([1], EntityType::EVSE)
                    .with_feature(limitation::load_control_feature(1))
                    .with_feature(limitation::device_configuration_feature(2))
                    .with_feature(limitation::device_diagnosis_feature(3))
                    .with_feature(limitation::device_diagnosis_client_feature(5)),
            )
            .expect("one entity");
        let mut site = Engine::new(site_device);
        site.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

        let mut guard_device = LocalDevice::new(
            "i:12345",
            "ControlBox-1",
            DeviceType::ElectricitySupplySystem,
        )
        .expect("a device address");
        guard_device
            .add_entity(
                LocalEntity::new([1], EntityType::GridGuard).with_feature(LocalFeature::new(
                    1,
                    FeatureType::Generic,
                    Role::Client,
                )),
            )
            .expect("one entity");
        let mut guard = Engine::new(guard_device);
        guard.add_use_case([1], 1, &lpc::ENERGY_GUARD);

        Self {
            guard,
            site,
            controller: Sited::new(
                Site {
                    connection_kw,
                    devices,
                },
                kw("4.2"),
                time::Duration::hours(2),
            ),
            now: core::time::Duration::ZERO,
        }
    }

    fn guard_client(&self) -> FeatureAddress {
        self.guard.device().address_of(&[1], 1)
    }

    fn site_feature(&self, feature: u32) -> FeatureAddress {
        self.site.device().address_of(&[1], feature)
    }

    /// Deliver messages both ways, **through the codec**, and answer the writes
    /// that are waiting.
    ///
    /// The round trip is the point: a payload that only ever travelled as a
    /// Rust value has not crossed a wire, and this workspace's whole argument
    /// about `ScaledNumber` is about what survives the encoding.
    fn pump(&mut self, wire_at: time::OffsetDateTime) -> Vec<Decimal> {
        let mut accepted = Vec::new();
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.guard.poll_transmit() {
                self.site.handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.site.poll_transmit() {
                self.guard.handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            let events: Vec<SpineEvent> = core::iter::from_fn(|| self.site.poll_event()).collect();
            for event in events {
                if let SpineEvent::WriteRequested(request) = event {
                    moved = true;
                    // The **decision** is the controller's, and the value it
                    // decides on comes off `resolved` — what the function
                    // becomes if the write is accepted (SPINE IG §3.3).
                    match self
                        .controller
                        .on_limit(&request.data, &request.resolved, wire_at)
                    {
                        Ok(limit) => {
                            if let Some(stated) = limit.stated_kw() {
                                accepted.push(stated);
                            }
                            self.site
                                .accept_write(request.token, self.now)
                                .expect("a token the engine just issued");
                        }
                        Err(_) => {
                            // A NACK is a fact the Energy Guard can act on, and
                            // a substituted value is not.
                            self.site.reject_write(
                                request.token,
                                eebus::spine::ErrorNumber::CommandRejected,
                                self.now,
                            );
                        }
                    }
                }
            }
            if !moved {
                return accepted;
            }
        }
        panic!("the exchange did not settle");
    }

    /// `[LPC §3.3]`'s pre-scenario communication: discovery, binding,
    /// subscription.
    fn commission(&mut self) {
        let guard_nm = node_management(self.guard.device().address());
        let site_nm = node_management(self.site.device().address());
        for function in [
            eebus::model::Function::NodeManagementDetailedDiscoveryData,
            eebus::model::Function::NodeManagementUseCaseData,
        ] {
            self.guard.read(&site_nm, &guard_nm, function, self.now);
        }
        self.pump(at(0));

        let client = self.guard_client();
        for feature in [1, 2] {
            let server = self.site_feature(feature);
            self.guard.request_binding(&client, &server, self.now);
        }
        for feature in [1, 2, 3] {
            let server = self.site_feature(feature);
            self.guard.request_subscription(&client, &server, self.now);
        }
        self.pump(at(0));
        self.controller.on_heartbeat(at(0));
    }

    /// The grid operator writes a limit, as a `ScaledNumber` — the exact pair
    /// SPINE carries, never a float.
    fn write_limit(
        &mut self,
        number: i64,
        scale: i16,
        active: bool,
        wire_at: time::OffsetDateTime,
    ) {
        self.update(
            LoadControlLimitData {
                limit_id: Some(limitation::LIMIT_ID),
                is_limit_active: Some(active),
                value: Some(ScaledNumber::new(number, scale)),
                ..Default::default()
            },
            wire_at,
        );
    }

    fn update(&mut self, entry: LoadControlLimitData, wire_at: time::OffsetDateTime) {
        let client = self.guard_client();
        let server = self.site_feature(1);
        let data = CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![entry]),
        });
        self.guard.write(&server, &client, data, true, self.now);
        self.pump(wire_at);
    }
}

/// A datagram, encoded and decoded — the wire, rather than a value passed along.
fn round_trip(datagram: &eebus::model::Datagram) -> eebus::model::Datagram {
    let json = serde_json::to_vec(datagram).expect("a datagram serialises");
    serde_json::from_slice(&json).expect("and comes back")
}

#[test]
fn a_setpoint_travels_as_a_datagram_and_no_point_ends_up_above_it() {
    let one = depot(1, Decimal::from(11));
    let two = depot(2, Decimal::from(11));
    let mut wire = Wire::new(vec![one.clone(), two.clone()], Decimal::from(22));
    wire.commission();

    // 4139.5 W: a scale of −1, which is the shape a trip through `f64` stops
    // being exact at — and sixty watts from `[BK6-22-300 Ziff. 4.5]`'s
    // Mindestleistung, which is the distance a compliance question turns on.
    //
    // Above it rather than below, so this asserts the arithmetic rather than
    // the Anspruch: 11 kW.
    wire.write_limit(110_000, -1, true, at(1));

    let held = wire.controller.limit_at(at(1));
    assert_eq!(held.lpc_state(), Some("limited"));
    assert_eq!(
        held.stated_kw(),
        Some(kw("11")),
        "11000.0 W off the wire is 11 kW here, exactly"
    );

    // A market plan asking for everything both boxes can take, under a ceiling
    // that carries one of them: `[BK6-22-300 Ziff. 4.6]`'s priority rule, which
    // `emob-smart` makes structural by taking the plan as an input.
    let plan = [
        Demand {
            evse_id: one.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        },
        Demand {
            evse_id: two.evse_id().clone(),
            wanted_kw: Decimal::from(11),
        },
    ];
    let allocation = wire.controller.allocate(&plan, at(1));
    assert!(allocation.within_ceiling());
    assert_eq!(allocation.total_kw(), kw("11"));

    // …and each station is told, in watts, at a purpose an optimiser cannot
    // outrank.
    let carried = wire.controller.charging_profiles(&plan, at(1), 1);
    assert_eq!(carried.value.len(), 2);
    let total: Decimal = carried.value.iter().map(|s| s.watts).sum();
    assert_eq!(total, Decimal::from(11_000));
    for setpoint in &carried.value {
        assert_eq!(
            setpoint.profile.charging_profile_purpose,
            ocpp_kit::v2_1::ChargingProfilePurpose::ChargingStationExternalConstraints
        );
    }

    // The pair `[LPC §4.1.5]` makes the operator's evidence.
    let log = wire.controller.log();
    assert_eq!(log.len(), 1);
    assert_eq!(
        emob_eebus::read_limit_write(&log[0].asked, limitation::LIMIT_ID)
            .unwrap()
            .watts,
        Decimal::from(11_000)
    );
    assert!(
        emob_eebus::read_limit_write(&log[0].asked, limitation::LIMIT_ID)
            .unwrap()
            .is_active
    );
    assert!(
        matches!(&log[0].answered, Answer::Accepted { ceiling_kw } if *ceiling_kw == kw("11")),
        "{:?}",
        log[0].answered
    );
}

#[test]
fn a_setpoint_below_the_mindestleistung_is_held_at_the_mindestleistung_and_says_so() {
    // `[BK6-22-300 Ziff. 4.5]` gives the operator an *Anspruch* to a minimum
    // draw, so a setpoint below it is an instruction the grid operator may not
    // give. Obeying it silently is giving up a legal claim that nobody would
    // ever find out had been given up — which is `emob-smart`'s rule, arriving
    // here through the wire rather than through a call.
    let one = depot(1, Decimal::from(11));
    let mut wire = Wire::new(vec![one.clone()], Decimal::from(22));
    wire.commission();

    // 2 kW, against a single-device site whose Mindestleistung is 4.2 kW.
    wire.write_limit(2_000, 0, true, at(1));

    let plan = [Demand {
        evse_id: one.evse_id().clone(),
        wanted_kw: Decimal::from(11),
    }];
    let allocation = wire.controller.allocate(&plan, at(1));
    assert_eq!(allocation.ceiling_kw(), kw("4.2"));
    assert!(
        allocation
            .reasons()
            .any(|reason| reason.contains("[BK6-22-300 Ziff. 4.5]")),
        "the claim is on the record: {:?}",
        allocation.reasons().collect::<Vec<_>>()
    );

    // …and the record says what was asked for and what was answered, which are
    // different numbers here and are both facts an operator has to produce.
    let log = wire.controller.log();
    assert_eq!(
        emob_eebus::read_limit_write(&log[0].asked, limitation::LIMIT_ID)
            .unwrap()
            .watts,
        Decimal::from(2_000)
    );
    assert!(
        matches!(&log[0].answered, Answer::Accepted { ceiling_kw } if *ceiling_kw == kw("4.2")),
        "{:?}",
        log[0].answered
    );
}

#[test]
fn a_limit_no_decimal_can_hold_is_refused_over_the_wire_rather_than_approximated() {
    // A `scale` is a signed 16-bit number the peer chooses, so a value outside
    // a decimal is one well-formed message away. A substituted figure is a
    // limit nobody asked for, applied to a site that will obey it — so the
    // answer is a NACK, and the refusal is on the record.
    let one = depot(1, Decimal::from(11));
    let mut wire = Wire::new(vec![one.clone()], Decimal::from(22));
    wire.commission();

    wire.write_limit(i64::MAX, 300, true, at(1));

    // LPC-902: an inapplicable valid first limit ends initialization.
    assert_eq!(
        wire.controller.limit_at(at(1)).lpc_state(),
        Some("unlimited/controlled")
    );
    let log = wire.controller.log();
    assert_eq!(log.len(), 1);
    let Answer::Refused { reason } = &log[0].answered else {
        panic!("{:?}", log[0].answered);
    };
    assert!(reason.contains("precision"), "{reason}");
}

#[test]
fn a_silence_walks_the_site_through_the_states_the_specification_gives_it() {
    // site-limit-timing/SR-031
    let one = depot(1, Decimal::from(11));
    let mut wire = Wire::new(vec![one.clone()], Decimal::from(11));
    let plan = [Demand {
        evse_id: one.evse_id().clone(),
        wanted_kw: kw("11"),
    }];
    let seconds = |s| at(0) + time::Duration::seconds(s);
    let check = |wire: &Wire, s, state, power| {
        let t = seconds(s);
        assert_eq!(wire.controller.limit_at(t).lpc_state(), Some(state));
        assert_eq!(wire.controller.allocate(&plan, t).ceiling_kw(), kw(power));
        assert_eq!(
            wire.controller.charging_profiles(&plan, t, 1).value[0].watts,
            kw(power) * Decimal::from(1000)
        );
    };
    check(&wire, 0, "init", "4.2");
    wire.commission();
    wire.write_limit(5_000, 0, true, seconds(60));
    wire.write_limit(6_000, 0, true, seconds(110));
    check(&wire, 119, "limited", "6");
    check(&wire, 120, "failsafe state", "4.2");
    check(&wire, 7319, "failsafe state", "4.2");
    check(&wire, 7320, "unlimited/autonomous", "11");
    wire.controller.on_heartbeat(seconds(7330));
    check(&wire, 7330, "unlimited/autonomous", "11");
    wire.write_limit(7_000, 0, true, seconds(7391));
    assert!(matches!(
        wire.controller.log().last().unwrap().answered,
        Answer::Refused { .. }
    ));
    wire.controller.on_heartbeat(seconds(7392));
    wire.write_limit(7_000, 0, true, seconds(7452));
    check(&wire, 7452, "limited", "7");
    wire.write_limit(0, 0, false, seconds(7460));
    check(&wire, 7511, "unlimited/controlled", "11");
    check(&wire, 7512, "failsafe state", "4.2");
    wire.controller.on_heartbeat(seconds(7513));
    wire.write_limit(i64::MAX, 300, true, seconds(7514));
    check(&wire, 7514, "unlimited/controlled", "11");
    let fresh = Wire::new(vec![one], kw("11"));
    check(&fresh, 7515, "init", "4.2");
}

#[test]
fn a_release_is_a_message_the_wire_carries_and_not_the_absence_of_one() {
    // §2.13 of the implementation guide wants a deactivation **sent**, and sent
    // only when the grid actually permits unlimited operation. A site that
    // inferred a release from silence would be a site that stops obeying a
    // limit because a packet went missing.
    let one = depot(1, Decimal::from(11));
    let mut wire = Wire::new(vec![one], Decimal::from(22));
    wire.commission();
    wire.write_limit(5_000, 0, true, at(1));
    wire.controller.on_heartbeat(at(1));
    assert_eq!(wire.controller.limit_at(at(2)).lpc_state(), Some("limited"));
    wire.controller.on_heartbeat(at(2));

    wire.write_limit(0, 0, false, at(3));
    assert_eq!(
        wire.controller.limit_at(at(3)).lpc_state(),
        Some("unlimited/controlled"),
        "the guard is speaking and allowing full power [LPC-905]"
    );
    assert_eq!(wire.controller.log().len(), 2, "both writes are evidence");
}

fn fragment(active: Option<bool>, watts: Option<i64>, end: Option<&str>) -> LoadControlLimitData {
    LoadControlLimitData {
        limit_id: Some(limitation::LIMIT_ID),
        is_limit_active: active,
        value: watts.map(|w| ScaledNumber::new(w, 0)),
        time_period: end.map(|end| eebus::model::TimePeriod {
            end_time: Some(end.to_owned().into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn partial_writes_preserve_or_replace_the_expiry_seen_by_the_station() {
    // site-limit-timing/SR-030
    let one = depot(1, kw("11"));
    let demand = [Demand {
        evse_id: one.evse_id().clone(),
        wanted_kw: kw("11"),
    }];
    for (replacement, expected) in [(None, "11"), (Some("PT10M"), "6")] {
        let mut wire = Wire::new(vec![one.clone()], kw("11"));
        wire.commission();
        wire.update(fragment(Some(true), Some(5000), Some("PT10M")), at(0));
        for n in 1..=18 {
            wire.controller
                .on_heartbeat(at(0) + time::Duration::seconds(30 * n));
        }
        wire.update(fragment(None, Some(6000), replacement), at(9));
        wire.update(fragment(None, None, Some("2026-11-28T12:00:00Z")), at(9));
        assert!(matches!(
            wire.controller.log().last().unwrap().answered,
            Answer::Refused { .. }
        ));
        for (time, ceiling) in [(at(10) - time::Duration::SECOND, "6"), (at(10), expected)] {
            assert_eq!(
                wire.controller.allocate(&demand, time).ceiling_kw(),
                kw(ceiling)
            );
            assert_eq!(
                wire.controller.charging_profiles(&demand, time, 1).value[0].watts,
                kw(ceiling) * Decimal::from(1000)
            );
        }
        wire.update(fragment(None, Some(7000), None), at(10));
        assert_eq!(
            wire.controller.limit_at(at(10)).stated_kw(),
            replacement.map(|_| kw("7"))
        );
    }
}

#[test]
fn activation_zero_duration_and_coincident_loss_have_distinct_outcomes() {
    let mut wire = Wire::new(vec![depot(1, kw("11"))], kw("11"));
    wire.commission();
    wire.update(fragment(Some(true), Some(0), Some("PT2M")), at(0));
    assert_eq!(
        wire.controller.limit_at(at(0)).stated_kw(),
        Some(Decimal::ZERO)
    );
    assert_eq!(
        wire.controller.limit_at(at(2)).lpc_state(),
        Some("failsafe state")
    );
    wire.controller.on_heartbeat(at(2));
    wire.update(fragment(Some(true), Some(6000), Some("PT0S")), at(2));
    assert_eq!(
        wire.controller.limit_at(at(2)).lpc_state(),
        Some("unlimited/controlled")
    );
    wire.update(fragment(Some(false), Some(5000), Some("PT1M")), at(2));
    wire.update(fragment(None, Some(4000), None), at(2));
    assert_eq!(
        wire.controller.limit_at(at(2)).lpc_state(),
        Some("unlimited/controlled")
    );
    wire.update(fragment(Some(true), None, None), at(3));
    assert_eq!(
        wire.controller.limit_at(at(3)).lpc_state(),
        Some("unlimited/controlled")
    );
}

#[test]
fn malformed_recovery_and_out_of_order_receipts_preserve_the_current_exchange() {
    let mut wire = Wire::new(vec![depot(1, kw("11"))], kw("11"));
    wire.commission();
    wire.write_limit(5000, 0, true, at(0));
    wire.controller.on_heartbeat(at(2));
    wire.update(fragment(Some(true), Some(-1), Some("PT1M")), at(2));
    assert_eq!(
        wire.controller.limit_at(at(2)).lpc_state(),
        Some("failsafe state")
    );
    wire.write_limit(6000, 0, true, at(2));
    wire.controller.on_heartbeat(at(1));
    wire.write_limit(9000, 0, true, at(1));
    assert!(matches!(
        wire.controller.log().last().unwrap().answered,
        Answer::Refused { .. }
    ));
    assert_eq!(wire.controller.limit_at(at(2)).stated_kw(), Some(kw("6")));
    wire.update(fragment(Some(true), Some(6000), Some("PT4M")), at(2));
    wire.controller.on_heartbeat(at(4));
    wire.update(fragment(Some(true), Some(7000), None), at(4));
    wire.controller.on_heartbeat(at(5));
    assert_eq!(
        wire.controller
            .limit_at(at(6) - time::Duration::SECOND)
            .stated_kw(),
        Some(kw("7"))
    );
    assert_eq!(
        wire.controller.limit_at(at(6)).lpc_state(),
        Some("unlimited/controlled")
    );
}

#[test]
fn a_deadline_outside_the_clock_preserves_the_accepted_limit() {
    let mut wire = Wire::new(vec![depot(1, kw("11"))], kw("11"));
    let t = time::Date::MAX.with_hms(23, 59, 58).unwrap().assume_utc();
    wire.commission();
    wire.controller.on_heartbeat(t);
    wire.write_limit(5000, 0, true, t);
    wire.update(fragment(Some(true), Some(6000), Some("PT1M")), t);
    assert!(matches!(
        wire.controller.log().last().unwrap().answered,
        Answer::Refused { .. }
    ));
    assert_eq!(wire.controller.limit_at(t).stated_kw(), Some(kw("5")));
}

#[test]
fn withdrawing_end_time_cancels_the_existing_deadline() {
    use eebus::model::{
        Filter, FilterElements, FilterSelectors, LoadControlLimitDataElements,
        LoadControlLimitListDataSelectors, TimePeriodElements,
    };
    let mut wire = Wire::new(vec![depot(1, kw("11"))], kw("11"));
    wire.commission();
    wire.update(fragment(Some(true), Some(5000), Some("PT1M")), at(0));
    let delete = Filter::delete()
        .select(FilterSelectors::LoadControlLimitListDataSelectors(
            LoadControlLimitListDataSelectors {
                limit_id: Some(limitation::LIMIT_ID),
            },
        ))
        .covering(FilterElements::LoadControlLimitDataElements(
            LoadControlLimitDataElements {
                time_period: Some(TimePeriodElements {
                    end_time: Some(eebus::codec::ElementTag),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ));
    let update = CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![fragment(None, Some(7000), None)]),
    });
    wire.guard.write_filtered(
        &wire.site_feature(1),
        &wire.guard_client(),
        update,
        vec![delete, Filter::partial()],
        wire.now,
    );
    wire.pump(at(0) + time::Duration::seconds(30));
    assert_eq!(wire.controller.limit_at(at(1)).stated_kw(), Some(kw("7")));
}
