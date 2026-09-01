//! A reference day: a fleet, its sessions, and what the chain made of them.
//!
//! # The demo that cannot lie
//!
//! A hundred posts charge cars all day. Some of the sessions go wrong in the
//! ways sessions actually go wrong. At the end of it exactly one thing has to
//! be true, and it is not "everything billed":
//!
//! > **Every kilowatt-hour a meter moved either reached a settled record or was
//! > refused with a reason. Nothing is unaccounted for.**
//!
//! That is the metering identity `Σ allocated + residual = total` the rest of
//! this workspace is built around, applied to a whole day rather than to one
//! session — and it is the only assertion a fleet run can make that a silent
//! failure cannot satisfy. A run that asserted "no errors" would pass by
//! throwing sessions away.

use emob_cdr::{Acceptance, Cdr, CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::{Currency, Direction, Energy, EvseId, PartyId, SessionId};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_ocpp::Transaction;
use emob_session::Authorization;
use emob_tariff::{
    Dimension, PriceComponent, Restrictions, Tariff, TariffElement, TariffKind, TaxIncluded,
    check_afir,
};
use rust_decimal::Decimal;

use crate::fault::{Fault, FaultPlan};
use crate::rng::Rng;
use crate::station::{SessionPlan, VirtualStation};

/// The operator every simulated post belongs to.
fn operator() -> PartyId {
    PartyId::new("DE", "SIM").expect("a valid party id")
}

/// The two tariff shapes a simulated fleet offers.
///
/// One prices energy only; the other adds the occupancy fee `[AFIR Art. 5(4)]`
/// permits at 50 kW and above. The difference is not decoration: an occupancy
/// fee prices a **duration**, so half the fleet runs into the clock rules and
/// half does not, and a run in which every tariff is the same shape exercises
/// one gate out of two.
///
/// The occupancy fee is `6.00` an hour, which is `0.10` a minute exactly — the
/// unit the article asks the station to show it in.
///
/// # Both shapes carry a night rate
///
/// So the fleet prices real sessions across a tariff boundary rather than always
/// against a single element. It does **not** reach the rating engine's
/// *subdivision*: every period here is a quarter-hour slot and `22:00` and
/// `06:00` are on the quarter-hour grid, so a period never spans one. That is
/// the split working — `[PTB-A 50.7 §3.1.7.2]` wants a price change on a
/// measurement-period boundary anyway — and the subdivision is unit-tested in
/// `emob_tariff::rating`, where a caller's periods can be any shape.
///
/// The night element is listed first because the unrestricted one shadows
/// anything behind it.
fn tariff(prices_occupancy: bool) -> Tariff {
    let components = |energy_price: &str| {
        let mut components = vec![
            PriceComponent::new(
                Dimension::Energy,
                Decimal::from_str_exact(energy_price).unwrap(),
            )
            .with_vat(Decimal::from(19)),
        ];
        if prices_occupancy {
            components.push(
                PriceComponent::new(
                    Dimension::ParkingTime,
                    Decimal::from_str_exact("6.00").unwrap(),
                )
                .with_vat(Decimal::from(19)),
            );
        }
        components
    };

    Tariff {
        id: if prices_occupancy {
            "sim-ad-hoc-dc".parse().expect("a valid tariff id")
        } else {
            "sim-ad-hoc-ac".parse().expect("a valid tariff id")
        },
        currency: Currency::EUR,
        kind: TariffKind::AdHoc,
        tax_included: TaxIncluded::Yes,
        elements: vec![
            TariffElement {
                components: components("0.39"),
                restrictions: Restrictions {
                    start_time: Some(time::macros::time!(22:00)),
                    end_time: Some(time::macros::time!(06:00)),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(components("0.49")),
        ],
        min_price: None,
        max_price: None,
        valid_from: None,
        valid_until: None,
    }
}

/// The tariff `Fault::UnlawfulTariff` swaps in: an occupancy fee and no price
/// per kWh.
///
/// Perfectly ordinary on a 22 kW post and unlawful on the 150 kW charger beside
/// it, which is the whole point — the fault is a property of the *pairing*, not
/// of the tariff, so a fleet of one power could never exercise it.
fn per_minute_only() -> Tariff {
    Tariff::simple(
        "sim-ad-hoc-minute".parse().expect("a valid tariff id"),
        Currency::EUR,
        TariffKind::AdHoc,
        vec![
            PriceComponent::new(
                Dimension::ParkingTime,
                Decimal::from_str_exact("6.00").unwrap(),
            )
            .with_vat(Decimal::from(19)),
        ],
    )
}

/// A day's worth of a simulated fleet.
#[derive(Debug, Clone)]
pub struct ReferenceDay {
    seed: u64,
    stations: u32,
    sessions_per_station: u32,
    day_start: time::OffsetDateTime,
    faults: FaultPlan,
}

impl ReferenceDay {
    /// Start describing a day.
    #[must_use]
    pub fn builder() -> ReferenceDayBuilder {
        ReferenceDayBuilder::default()
    }

    /// How many posts.
    #[must_use]
    pub const fn stations(&self) -> u32 {
        self.stations
    }

    /// How many sessions the day will attempt in total.
    #[must_use]
    pub const fn planned_sessions(&self) -> u32 {
        self.stations * self.sessions_per_station
    }

    /// Run the day through the whole chain.
    ///
    /// Meter → signed OCMF → evidence → session → quarter-hour split → CDR →
    /// ledger, using the same code an operator's own backend would.
    ///
    /// # Panics
    ///
    /// Never, for a day this builder can produce: the identifiers are
    /// assembled from indices and the simulated meter series never runs
    /// backwards. A panic here is a bug in the generator rather than a fact
    /// about the fleet, which is why it is an assertion rather than an error
    /// variant callers would have to handle and could not act on.
    #[must_use]
    pub fn run(&self) -> DayOutcome {
        let mut station_rng = Rng::stream(self.seed, "stations");
        let mut session_rng = Rng::stream(self.seed, "sessions");
        let mut fault_rng = Rng::stream(self.seed, "faults");

        let mut fleet: Vec<VirtualStation> = (0..self.stations)
            .map(|index| VirtualStation::new(index, &mut station_rng))
            .collect();

        // The registry a provisioning run would have written. One station's
        // entry is withheld per `Fault::UnregisteredStation`, which is decided
        // per session so the same post can be reachable and then not — a
        // provisioning gap that opens mid-day is the realistic version.
        let mut registry = KeyRegistry::new();
        for station in &fleet {
            registry.insert(station.component(), station.registered_key());
        }
        let empty_registry = KeyRegistry::new();

        let mut outcome = DayOutcome::default();

        for round in 0..self.sessions_per_station {
            for (index, station) in fleet.iter_mut().enumerate() {
                let faults = self.faults.draw(&mut fault_rng);
                let plan = SessionPlan::draw(self.day_start, &mut session_rng);
                let charged = station.charge(&plan, &faults);

                let moved = charged
                    .series
                    .total()
                    .expect("a simulated series never runs backwards");
                outcome.metered += moved;
                outcome.attempted += 1;

                let session_id: SessionId = format!("sim-{index:05}-{round:03}")
                    .parse()
                    .expect("a valid session id");
                let refusal = |reasons: Vec<String>| Refused {
                    evse_id: station.evse_id.clone(),
                    session_id: session_id.clone(),
                    energy: moved,
                    faults: faults.clone(),
                    reasons,
                };

                // ── The chain, exactly as a backend would run it ────────────
                //
                // Through the **OCPP seam**, not around it: what a CSMS holds
                // is a stream of transaction events, and the session it bills
                // is assembled from the signed values inside them. Nothing here
                // touches a numeric meter value, because `emob-ocpp`'s input
                // vocabulary has none.
                let transaction = Transaction {
                    id: session_id.clone(),
                    evse_id: station.evse_id.clone(),
                    authorization: Authorization::ad_hoc(),
                    events: charged.events.clone(),
                };
                let assembled = match transaction.assemble(Direction::Import) {
                    Ok(assembled) => assembled,
                    Err(error) => {
                        outcome.refuse(refusal(vec![error.to_string()]));
                        continue;
                    }
                };
                let (session, records) = (assembled.session, assembled.records);

                let evidence = Evidence::assemble(
                    &records,
                    if faults.contains(&Fault::UnregisteredStation) {
                        &empty_registry
                    } else {
                        &registry
                    },
                    charged.started_at,
                );

                // A tariff the operator may not offer at this power is refused
                // before it prices anything. The energy is measured, every
                // signature holds and the record must still not be built:
                // `[AFIR Art. 5(4)]` is a rule about the *pairing* of a tariff
                // and a charge point, and a backend that rates first and checks
                // later has already produced the number it may not charge.
                let offered = if faults.contains(&Fault::UnlawfulTariff) {
                    per_minute_only()
                } else {
                    tariff(station.prices_occupancy())
                };
                let conformance = check_afir(&offered, station.rated_power_kw);
                if !conformance.is_lawful() {
                    outcome.refuse(refusal(conformance.reasons().collect()));
                    continue;
                }

                let cdr = CdrBuilder::from_session(&session, Direction::Import).and_then(|b| {
                    b.key(
                        operator(),
                        format!("sim-{index:05}-{round:03}")
                            .parse()
                            .expect("a valid CDR id"),
                    )
                    .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
                    .rated_with(&offered)
                    .build()
                });

                match cdr {
                    Ok(cdr) => {
                        // A settled record is one the ledger accepted, not one
                        // the builder returned: the ledger is where a
                        // retransmission stops being a second invoice line.
                        match outcome.ledger.accept(cdr.clone()) {
                            Acceptance::Stored => outcome.settle(cdr),
                            other => outcome.refuse(refusal(vec![format!(
                                "the ledger did not store it: {other:?}"
                            )])),
                        }
                    }
                    Err(error) => {
                        let mut reasons = vec![error.to_string()];
                        reasons.extend(evidence.reasons());
                        outcome.refuse(refusal(reasons));
                    }
                }
            }
        }

        outcome
    }
}

/// Describes a [`ReferenceDay`].
#[derive(Debug, Clone)]
pub struct ReferenceDayBuilder {
    seed: u64,
    stations: u32,
    sessions_per_station: u32,
    day_start: time::OffsetDateTime,
    faults: FaultPlan,
}

impl Default for ReferenceDayBuilder {
    fn default() -> Self {
        Self {
            seed: 0x00E4_0B15,
            stations: 100,
            sessions_per_station: 4,
            // A fixed day, because a simulation that reads a clock is a
            // simulation whose failures cannot be replayed.
            day_start: time::macros::datetime!(2026-01-02 00:00 +1),
            faults: FaultPlan::none(),
        }
    }
}

impl ReferenceDayBuilder {
    /// The seed the whole day is a function of.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// How many posts the fleet has.
    #[must_use]
    pub const fn stations(mut self, stations: u32) -> Self {
        self.stations = stations;
        self
    }

    /// How many sessions each post runs.
    #[must_use]
    pub const fn sessions_per_station(mut self, sessions: u32) -> Self {
        self.sessions_per_station = sessions;
        self
    }

    /// When the day begins.
    #[must_use]
    pub const fn day_start(mut self, at: time::OffsetDateTime) -> Self {
        self.day_start = at;
        self
    }

    /// Which faults to inject, and how often.
    #[must_use]
    pub fn faults(mut self, faults: FaultPlan) -> Self {
        self.faults = faults;
        self
    }

    /// Build it.
    #[must_use]
    pub fn build(self) -> ReferenceDay {
        ReferenceDay {
            seed: self.seed,
            stations: self.stations,
            sessions_per_station: self.sessions_per_station,
            day_start: self.day_start,
            faults: self.faults,
        }
    }
}

/// A session that did not reach a settled record, and why.
#[derive(Debug, Clone)]
pub struct Refused {
    /// Which post.
    pub evse_id: EvseId,
    /// Which session.
    pub session_id: SessionId,
    /// How much energy its meter moved anyway — the number that has to be
    /// accounted for even though nobody may bill it.
    pub energy: Energy,
    /// What was injected.
    pub faults: Vec<Fault>,
    /// What the chain said, in the order it said it.
    pub reasons: Vec<String>,
}

/// What a day came to.
#[derive(Debug, Default)]
pub struct DayOutcome {
    /// The records that settled, in the order they were accepted.
    pub settled: Vec<Cdr>,
    /// The sessions that did not, each with its reasons.
    pub refused: Vec<Refused>,
    /// The ledger the settled records went into.
    pub ledger: CdrLedger,
    /// How many sessions were attempted.
    pub attempted: u32,
    /// Everything the meters moved, billable or not.
    pub metered: Energy,
    billed: Energy,
    unbilled: Energy,
}

impl DayOutcome {
    fn settle(&mut self, cdr: Cdr) {
        self.billed += cdr.total_energy;
        self.settled.push(cdr);
    }

    fn refuse(&mut self, refused: Refused) {
        self.unbilled += refused.energy;
        self.refused.push(refused);
    }

    /// The energy that reached a settled record.
    #[must_use]
    pub const fn billed(&self) -> Energy {
        self.billed
    }

    /// The energy a meter moved that no record billed.
    ///
    /// Not lost — *accounted for*. Every kilowatt-hour here belongs to a
    /// [`Refused`] that names its reasons, which is the difference between a
    /// residual and a leak.
    #[must_use]
    pub const fn unbilled(&self) -> Energy {
        self.unbilled
    }

    /// **The identity the whole run exists to assert.**
    ///
    /// `billed + unbilled == metered`, exactly. It is `Σ allocated + residual =
    /// total` over a day rather than a session, and it is the only assertion a
    /// fleet run can make that a silent failure cannot satisfy: a run that
    /// asserted "no errors" would pass by throwing sessions away.
    #[must_use]
    pub fn reconciles(&self) -> bool {
        self.billed + self.unbilled == self.metered
    }

    /// Whether every session reached one outcome or the other.
    #[must_use]
    pub fn every_session_is_accounted_for(&self) -> bool {
        let settled = u32::try_from(self.settled.len()).unwrap_or(u32::MAX);
        let refused = u32::try_from(self.refused.len()).unwrap_or(u32::MAX);
        settled + refused == self.attempted
    }

    /// Whether every refusal says why.
    ///
    /// A session that vanishes without a reason is the failure this whole crate
    /// is shaped to make impossible: it looks like a clean run and is a lost
    /// invoice.
    #[must_use]
    pub fn every_refusal_has_a_reason(&self) -> bool {
        self.refused
            .iter()
            .all(|r| r.reasons.iter().any(|reason| !reason.trim().is_empty()))
    }

    /// Whether every settled record's own periods sum to its total.
    #[must_use]
    pub fn every_record_conserves(&self) -> bool {
        self.settled.iter().all(Cdr::conserves)
    }

    /// How many sessions each fault was injected into.
    #[must_use]
    pub fn faults_seen(&self) -> Vec<(Fault, usize)> {
        Fault::ALL
            .iter()
            .map(|fault| {
                (
                    *fault,
                    self.refused
                        .iter()
                        .filter(|r| r.faults.contains(fault))
                        .count(),
                )
            })
            .collect()
    }

    /// A one-line summary for a run log.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} sessions: {} settled ({}), {} refused ({}), metered {}",
            self.attempted,
            self.settled.len(),
            self.billed,
            self.refused.len(),
            self.unbilled,
            self.metered,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_day_settles_everything() {
        let day = ReferenceDay::builder()
            .stations(20)
            .sessions_per_station(3)
            .build();
        let outcome = day.run();

        assert_eq!(outcome.attempted, 60);
        assert!(
            outcome.refused.is_empty(),
            "{:?}",
            outcome.refused.first().map(|r| &r.reasons)
        );
        assert_eq!(outcome.settled.len(), 60);
        assert!(outcome.reconciles());
        assert_eq!(outcome.billed(), outcome.metered);
        assert!(outcome.every_record_conserves());
    }

    #[test]
    fn a_day_is_a_function_of_its_seed() {
        // A fleet run that fails once a month for reasons nobody can recreate
        // is worse than no fleet run at all.
        let build = |seed: u64| {
            ReferenceDay::builder()
                .seed(seed)
                .stations(8)
                .sessions_per_station(2)
                .faults(FaultPlan::everything(crate::Rate::one_in(4)))
                .build()
                .run()
        };
        let one = build(7);
        let two = build(7);
        assert_eq!(one.summary(), two.summary());
        assert_eq!(one.billed(), two.billed());
        assert_ne!(one.summary(), build(8).summary());
    }

    #[test]
    fn a_faulty_day_still_accounts_for_every_kilowatt_hour() {
        // The assertion the whole crate exists for. Not "everything billed" —
        // *nothing is unaccounted for*.
        let day = ReferenceDay::builder()
            .stations(25)
            .sessions_per_station(4)
            .faults(FaultPlan::everything(crate::Rate::one_in(6)))
            .build();
        let outcome = day.run();

        assert!(!outcome.refused.is_empty(), "the faults have to bite");
        assert!(!outcome.settled.is_empty(), "…and not bite everything");
        assert!(outcome.reconciles(), "{}", outcome.summary());
        assert!(outcome.every_session_is_accounted_for());
        assert!(outcome.every_refusal_has_a_reason());
        assert!(outcome.every_record_conserves());
    }

    #[test]
    fn the_ledger_holds_exactly_the_settled_records() {
        let outcome = ReferenceDay::builder()
            .stations(10)
            .sessions_per_station(3)
            .faults(FaultPlan::everything(crate::Rate::one_in(5)))
            .build()
            .run();

        assert_eq!(outcome.ledger.len(), outcome.settled.len());
        let ledger_energy: Energy = outcome.ledger.iter().map(|c| c.total_energy).sum();
        assert_eq!(ledger_energy, outcome.billed());
    }

    #[test]
    fn a_day_with_no_sessions_is_a_day_and_not_a_panic() {
        let outcome = ReferenceDay::builder()
            .stations(0)
            .sessions_per_station(0)
            .build()
            .run();
        assert_eq!(outcome.attempted, 0);
        assert!(outcome.reconciles());
        assert_eq!(outcome.metered, Energy::ZERO);
    }
}
