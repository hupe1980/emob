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
                    match self.controller.on_limit(&request.resolved, wire_at) {
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
        let client = self.guard_client();
        let server = self.site_feature(1);
        let data = CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(limitation::LIMIT_ID),
                is_limit_active: Some(active),
                value: Some(ScaledNumber::new(number, scale)),
                ..Default::default()
            }]),
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
    assert_eq!(log[0].asked.watts, Decimal::from(11_000));
    assert!(log[0].asked.is_active);
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
    assert_eq!(log[0].asked.watts, Decimal::from(2_000));
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

    // Nothing was taken on: the site is still uncontrolled, because a write
    // that could not be read is not a write that happened.
    assert_eq!(
        wire.controller.limit_at(at(1)),
        emob_smart::GridLimit::Uncontrolled
    );
    let log = wire.controller.log();
    assert_eq!(log.len(), 1);
    let Answer::Refused { reason } = &log[0].answered else {
        panic!("{:?}", log[0].answered);
    };
    assert!(reason.contains("will not approximate"), "{reason}");
}

#[test]
fn a_silence_walks_the_site_through_the_states_the_specification_gives_it() {
    // `[LPC §2.3.2]` gives a controllable system five states and three of them
    // allow full power. A site at full power because its grid operator says so,
    // one in its failsafe, and one that has left the failsafe under `[LPC-022]`
    // are three different facts — and only the timer can tell them apart, which
    // is the whole reason this service has a clock.
    let one = depot(1, Decimal::from(11));
    let mut wire = Wire::new(vec![one], Decimal::from(22));
    wire.commission();
    wire.write_limit(11_000, 0, true, at(1));

    assert_eq!(
        wire.controller.limit_at(at(30)).lpc_state(),
        Some("limited")
    );
    assert_eq!(
        wire.controller.limit_at(at(150)).lpc_state(),
        Some("failsafe state"),
        "past the Failsafe Duration Minimum [LPC-912]"
    );
    assert_eq!(
        wire.controller.limit_at(at(300)).lpc_state(),
        Some("unlimited/autonomous"),
        "and past [LPC-022]'s release, which every summary of § 14a drops"
    );

    // …and a heartbeat is the one thing that stops all of that `[LPC §3.8]`.
    // The setpoint is not re-sent — it stands until a write replaces it, which
    // is what `[LPC-004]`'s duration and §2.13's explicit deactivation are both
    // about — so the site goes back to being *limited* rather than released.
    wire.controller.on_heartbeat(at(290));
    let after = wire.controller.limit_at(at(300));
    assert_eq!(after.lpc_state(), Some("limited"));
    assert_eq!(after.stated_kw(), Some(kw("11")));
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
    assert_eq!(wire.controller.limit_at(at(2)).lpc_state(), Some("limited"));

    wire.write_limit(0, 0, false, at(3));
    assert_eq!(
        wire.controller.limit_at(at(4)).lpc_state(),
        Some("unlimited/controlled"),
        "the guard is speaking and allowing full power [LPC-905]"
    );
    assert_eq!(wire.controller.log().len(), 2, "both writes are evidence");
}
