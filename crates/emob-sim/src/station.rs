//! A virtual charging station that signs for real.
//!
//! # What is virtual and what is not
//!
//! The station is imaginary: it has no socket, no cabinet and no WebSocket. Its
//! **signatures are genuine** — a real ECDSA key, over the real payload bytes,
//! verified through the same code path a record off a real meter goes through.
//! A simulator whose fixtures are hand-written strings proves that the parser
//! accepts what the test author typed; one that signs proves that the chain
//! accepts what a station produces.
//!
//! # The register is the station's, not the session's
//!
//! A real meter counts up over its whole life, and a session is a *difference*
//! between two of its readings `[OCMF Tab. 7]`. So the station carries the
//! register across sessions, and the second session on a post starts where the
//! first left it. That is what makes the fleet's arithmetic worth checking:
//! every kilowatt-hour billed anywhere has to be a difference somebody can
//! point at.

use emob_core::{Direction, Energy, EvseId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_session::{MeterReading, MeterSeries, ReadingContext};
use ocmf::{Curve, PublicKey};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use crate::fault::Fault;
use crate::rng::Rng;

/// The OBIS register a well-behaved station signs its transaction energy
/// against `[OCMF Tab. 25]`: transaction import, measured at the mains.
const IMPORT_REGISTER: &str = "01-00:B2.08.00*FF";
/// …and the one a miswired cabinet signs instead: transaction **export**.
const EXPORT_REGISTER: &str = "01-00:C2.08.00*FF";

/// One charging post in a simulated fleet.
#[derive(Debug, Clone)]
pub struct VirtualStation {
    /// Which point this is.
    pub evse_id: EvseId,
    /// The meter serial the key registry binds a key to.
    pub meter_serial: String,
    /// What the post can deliver, in kW.
    ///
    /// Not decoration, and not cosmetic either: fifty kilowatts is the threshold
    /// `[AFIR Art. 5(4)]` turns on, so a fleet of one power exercises one side
    /// of every rule that mentions it — the tariff shape gate above all. Half
    /// the fleet is a 22 kW AC post and half a 150 kW DC charger.
    pub rated_power_kw: Decimal,
    signing_key: SigningKey,
    register_kwh: Decimal,
}

impl VirtualStation {
    /// Build station number `index`, deterministically.
    ///
    /// # Panics
    ///
    /// Never. The identifier is assembled from the index and the key is drawn
    /// until the scalar is valid, which happens on the first draw for every
    /// value a 256-bit generator can produce short of zero.
    #[must_use]
    pub fn new(index: u32, rng: &mut Rng) -> Self {
        // A real EVSE id: two-letter country, three-character operator, `E`,
        // then the outlet. Padded so a fleet of a hundred sorts readably.
        let evse_id: EvseId = format!("DE*SIM*E{index:05}")
            .parse()
            .expect("the generated EVSE id follows the grammar");

        let signing_key = loop {
            if let Ok(key) = SigningKey::from_bytes(&rng.bytes32().into()) {
                break key;
            }
        };

        Self {
            evse_id,
            meter_serial: format!("SIM-METER-{index:05}"),
            rated_power_kw: Decimal::from(if index % 2 == 1 { 150 } else { 22 }),
            signing_key,
            // Posts do not start life at zero, and a chain whose opening
            // reading is `0.000` hides an off-by-one nobody would notice.
            register_kwh: Decimal::from(rng.between(1_000, 900_000)) / Decimal::from(1000),
        }
    }

    /// The public key a registry has to hold for this station's records to
    /// verify.
    ///
    /// # Panics
    ///
    /// Never. A verifying key encodes to a well-formed uncompressed SEC1 point
    /// by construction, so the conversion cannot fail — it is `expect` rather
    /// than a `Result` because a simulator that could hand back a station with
    /// no key would push the impossibility into every caller.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey::from_sec1(
            Curve::Secp256r1,
            self.signing_key
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        )
        .expect("a verifying key encodes to a well-formed SEC1 point")
    }

    /// How this station is identified to the key registry.
    #[must_use]
    pub fn component(&self) -> ComponentRef {
        ComponentRef::Meter {
            serial: self.meter_serial.clone(),
        }
    }

    /// The registry entry a provisioning run would have written.
    #[must_use]
    pub fn registered_key(&self) -> RegisteredKey {
        RegisteredKey::unbounded(self.public_key(), "simulated type approval")
    }

    /// The register's current value, in kWh.
    #[must_use]
    pub const fn register_kwh(&self) -> Decimal {
        self.register_kwh
    }

    /// Whether this post is one `[AFIR Art. 5(4)]` calls fast — 50 kW or more.
    ///
    /// The threshold decides two things at once: whether the ad-hoc price must
    /// be based on a price per kWh, and whether an occupancy fee per minute may
    /// be added to it. So it also decides which tariff shape this post offers.
    #[must_use]
    pub fn is_fast(&self) -> bool {
        self.rated_power_kw >= Decimal::from(50)
    }

    /// Whether this post's operator prices occupancy as well as energy.
    ///
    /// Only a fast charger may: the occupancy fee is the one addition the
    /// article grants, and it grants it "to discourage long occupancy of the
    /// recharging point" at 50 kW and above. An occupancy fee is a price for a
    /// **duration**, so this post's sessions run into the clock rules and the
    /// energy-only post's do not — a fleet in which every tariff is the same
    /// shape exercises one gate out of two.
    #[must_use]
    pub fn prices_occupancy(&self) -> bool {
        self.is_fast()
    }

    /// Charge a vehicle, and produce what the station would have emitted.
    ///
    /// Advances the register by the plan's energy, emits one signed OCMF record
    /// per reading, and returns the meter series the CSMS would have assembled
    /// from the same readings. The two are two views of one event, which is
    /// exactly the thing the chain cross-checks.
    ///
    /// # Panics
    ///
    /// Never, for a plan this crate can produce: the taper is monotonic, so the
    /// series never runs backwards. A panic here is a bug in the generator
    /// rather than a fact about the fleet.
    pub fn charge(&mut self, plan: &SessionPlan, faults: &[Fault]) -> ChargedSession {
        let readings = plan.readings(self.register_kwh);
        self.register_kwh = readings
            .last()
            .map_or(self.register_kwh, |reading| reading.register);

        let register = if faults.contains(&Fault::WrongDirectionRegister) {
            EXPORT_REGISTER
        } else {
            IMPORT_REGISTER
        };
        let clock = if faults.contains(&Fault::UnsynchronisedClock) {
            'U'
        } else {
            'S'
        };

        let last = readings.len().saturating_sub(1);
        let mut records = Vec::with_capacity(readings.len());
        // What the station actually put on the wire, beside the reason it took
        // each reading and the instant it stamped on the event.
        let mut sent: Vec<(String, ReadingContext, time::OffsetDateTime)> =
            Vec::with_capacity(readings.len());
        for (index, reading) in readings.iter().enumerate() {
            let marker = match index {
                0 => "B",
                i if i == last => "E",
                _ => "C",
            };
            // One fault lands on a middle reading so that the chain has to
            // notice something between two perfectly good endpoints — which is
            // the case a "check the first and last value" verifier misses.
            let middle = index > 0 && index < last;
            let state = if middle && faults.contains(&Fault::SubstituteReading) {
                "S"
            } else {
                "G"
            };
            let marker = if middle && faults.contains(&Fault::ExceptionDuringCharging) {
                "X"
            } else {
                marker
            };

            let signed = self.sign(&SignedReading {
                pagination: u64::try_from(index).unwrap_or(u64::MAX) + 1,
                marker,
                state,
                register,
                clock,
                at: reading.at,
                value: reading.register,
            });
            sent.push((signed.clone(), reading.context, reading.at));
            records.push(signed);
        }

        if faults.contains(&Fault::TamperedValue)
            && let Some(record) = records.last_mut()
        {
            // A digit changes after signing. Every remaining byte is genuine,
            // which is what makes this the failure a signature exists to catch.
            *record = record.replacen("\"RV\":", "\"RV\":9", 1);
            if let Some(entry) = sent.last_mut() {
                entry.0.clone_from(record);
            }
        }
        if faults.contains(&Fault::DroppedRecord) && records.len() > 2 {
            // The middle of the session never arrives. Pagination is the only
            // thing that can see it: every remaining signature still verifies.
            let at = records.len() / 2;
            records.remove(at);
            sent.remove(at);
        }

        ChargedSession {
            events: ocpp_events(&sent, plan),
            records,
            series: MeterSeries::new(
                Direction::Import,
                readings
                    .iter()
                    .map(|r| {
                        MeterReading::new(r.at, r.energy(), Direction::Import, r.context).signed()
                    })
                    .collect(),
            )
            .expect("a plan produces a non-decreasing import series"),
            started_at: plan.started_at,
            ended_at: plan.ended_at,
        }
    }

    fn sign(&self, reading: &SignedReading<'_>) -> String {
        let payload = format!(
            concat!(
                r#"{{"FV":"1.4","GI":"emob-sim","GV":"1","PG":"T{pagination}","#,
                r#""MV":"emob","MM":"virtual","MS":"{serial}","#,
                r#""IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","#,
                r#""RD":[{{"TM":"{time} {clock}","TX":"{marker}","RV":{value},"#,
                r#""RI":"{register}","RU":"kWh","RT":"AC","EF":"","ST":"{state}"}}]}}"#,
            ),
            pagination = reading.pagination,
            serial = self.meter_serial,
            time = ocmf_time(reading.at),
            clock = reading.clock,
            marker = reading.marker,
            value = reading.value,
            register = reading.register,
            state = reading.state,
        );

        let signature: DerSignature = self
            .signing_key
            .sign_prehash(&Sha256::digest(payload.as_bytes()))
            .expect("signing a digest with a valid key");
        format!(
            "OCMF|{payload}|{{\"SD\":\"{}\"}}",
            hex::encode(signature.as_bytes())
        )
    }
}

struct SignedReading<'a> {
    pagination: u64,
    marker: &'a str,
    state: &'a str,
    register: &'a str,
    clock: char,
    at: time::OffsetDateTime,
    value: Decimal,
}

/// `2026-01-02T10:00:00,000+0100` — ISO 8601 with the comma OCMF writes.
fn ocmf_time(at: time::OffsetDateTime) -> String {
    let offset = at.offset();
    let sign = if offset.whole_seconds() < 0 { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02},000{sign}{:02}{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        offset.whole_hours().abs(),
        i64::from(offset.minutes_past_hour()).abs(),
    )
}

/// One reading the station is about to take.
#[derive(Debug, Clone, Copy)]
struct PlannedReading {
    at: time::OffsetDateTime,
    register: Decimal,
    context: ReadingContext,
}

impl PlannedReading {
    fn energy(self) -> Energy {
        Energy::from_kwh(self.register).expect("a register value is non-negative")
    }
}

/// What a vehicle is about to do at a post.
///
/// # The charge curve is not a straight line, and that is the point
///
/// A session that delivers the same energy every quarter hour makes every
/// interpolation exact and every ratio terminate, which is precisely the case
/// the split's conservation proof does not need. A real curve tapers — a
/// battery at 80 % takes far less than one at 20 % — so the plan front-loads
/// the energy and leaves the arithmetic awkward on purpose.
#[derive(Debug, Clone)]
pub struct SessionPlan {
    /// When the vehicle plugged in.
    pub started_at: time::OffsetDateTime,
    /// When it left.
    pub ended_at: time::OffsetDateTime,
    /// How much energy it took.
    pub energy: Energy,
    /// Whether the station reports a clock-aligned reading on every interior
    /// quarter-hour boundary — OCPP's `AlignedDataInterval = 900`.
    ///
    /// A post that does not is a post whose settlement slots are interpolated,
    /// and the split says so.
    pub clock_aligned: bool,
}

impl SessionPlan {
    /// Draw a plan deterministically.
    ///
    /// # Panics
    ///
    /// Never: the drawn duration and energy are both bounded positive, so the
    /// quantities they build cannot be rejected.
    #[must_use]
    pub fn draw(day_start: time::OffsetDateTime, rng: &mut Rng) -> Self {
        // Somewhere in the day, on an awkward minute and an awkward second.
        let offset_minutes = rng.below(20 * 60);
        let duration_minutes = rng.between(17, 190);
        let started_at = day_start
            + time::Duration::minutes(i64::try_from(offset_minutes).unwrap_or(0))
            + time::Duration::seconds(i64::try_from(rng.below(60)).unwrap_or(0));
        let ended_at =
            started_at + time::Duration::minutes(i64::try_from(duration_minutes).unwrap_or(0));

        // Between 3 and 90 kWh, in watt-hours, so the totals do not divide.
        let wh = rng.between(3_000, 90_000);
        Self {
            started_at,
            ended_at,
            energy: Energy::from_wh(Decimal::from(wh)).expect("a positive draw"),
            clock_aligned: !rng.one_in(5),
        }
    }

    /// The readings the station takes, starting from a register value.
    fn readings(&self, opening: Decimal) -> Vec<PlannedReading> {
        let mut instants = vec![(self.started_at, ReadingContext::TransactionBegin)];
        if self.clock_aligned {
            let mut cursor = emob_core::QuarterHour::containing(self.started_at).next();
            while cursor.start() < self.ended_at {
                instants.push((cursor.start(), ReadingContext::SampleClock));
                cursor = cursor.next();
            }
        }
        instants.push((self.ended_at, ReadingContext::TransactionEnd));

        // A tapering curve, allocated by *cumulative* weight so the pieces
        // telescope back to the plan's energy exactly — the same construction
        // the settlement split uses, for the same reason.
        let total_seconds = (self.ended_at - self.started_at).whole_seconds().max(1);
        let energy = self.energy.kwh();
        instants
            .iter()
            .map(|&(at, context)| {
                let elapsed = (at - self.started_at)
                    .whole_seconds()
                    .clamp(0, total_seconds);
                PlannedReading {
                    at,
                    register: opening + taper(energy, elapsed, total_seconds),
                    context,
                }
            })
            .collect()
    }
}

/// The cumulative share of `energy` delivered after `elapsed` of `total`.
///
/// A quadratic that starts steep and flattens: `2t − t²` over the unit
/// interval, which is `1` at the end, so the last reading is the whole energy
/// and nothing is lost to the shape. Computed as one fraction with the division
/// **last**, because that is the rule everywhere else here.
fn taper(energy: Decimal, elapsed: i64, total: i64) -> Decimal {
    let (t, n) = (Decimal::from(elapsed), Decimal::from(total));
    // energy × (2·t·n − t²) / n²
    energy * (Decimal::from(2) * t * n - t * t) / (n * n)
}

/// The OCPP a station would have sent for these records.
///
/// One event per record, in the order the station emitted them: the first
/// opens the transaction, the last closes it, and the rest are updates. The
/// `ReadingContext` travels with each signed value because the protocol is the
/// only thing that knows *why* a reading was taken — and that is what decides
/// whether a settlement slot is measured or interpolated.
///
/// A dropped record is genuinely absent from this stream, which is what makes
/// the fault realistic: a CSMS only ever knows what it was sent.
fn ocpp_events(
    sent: &[(String, ReadingContext, time::OffsetDateTime)],
    plan: &SessionPlan,
) -> Vec<emob_ocpp::TransactionEvent> {
    use emob_ocpp::{SignedMeterValue, SignedReading as OcppReading, TransactionEvent};

    let mut events: Vec<TransactionEvent> = sent
        .iter()
        .enumerate()
        .map(|(index, (record, context, at))| {
            // A station timestamps its event when it takes the reading, so the
            // instant comes from the reading rather than from a division.
            let signed = vec![OcppReading::new(
                SignedMeterValue::new(record),
                Some(context.as_str().to_owned()),
            )];
            if index == 0 {
                TransactionEvent::started(*at, signed)
            } else {
                TransactionEvent::updated(*at, signed)
            }
        })
        .collect();

    // The transaction has to close, and the closing event is the one that
    // carries the last signed value. A station that never sends it leaves a
    // session `Transaction::assemble` refuses as still running.
    if let Some(closing) = events.pop() {
        events.push(emob_ocpp::TransactionEvent::ended(
            plan.ended_at,
            closing.signed,
            emob_session::EndReason::Local,
        ));
    }
    events
}

/// What a station produced for one session.
#[derive(Debug, Clone)]
pub struct ChargedSession {
    /// The OCPP transaction events a CSMS would have received.
    ///
    /// The fleet's sessions are assembled from **these** rather than from
    /// [`Self::series`], so the reference day exercises the seam every real
    /// deployment runs through — including the faults, which land on the events
    /// exactly as they would land on a wire.
    pub events: Vec<emob_ocpp::TransactionEvent>,
    /// The signed OCMF records, in the order the station emitted them.
    pub records: Vec<String>,
    /// The meter series a CSMS would have assembled from the same readings.
    pub series: MeterSeries,
    /// When the vehicle plugged in.
    pub started_at: time::OffsetDateTime,
    /// When it left.
    pub ended_at: time::OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_eichrecht::{Evidence, KeyRegistry};
    use time::macros::datetime;

    fn day() -> time::OffsetDateTime {
        datetime!(2026-01-02 00:00 +1)
    }

    fn registry(station: &VirtualStation) -> KeyRegistry {
        let mut registry = KeyRegistry::new();
        registry
            .insert(station.component(), station.registered_key())
            .unwrap();
        registry
    }

    #[test]
    fn a_virtual_station_signs_records_that_actually_verify() {
        // The claim the crate rests on: these are not fixtures somebody typed,
        // they are signatures the same verifier checks a real meter's with.
        let mut rng = Rng::new(1);
        let mut station = VirtualStation::new(0, &mut rng);
        let plan = SessionPlan::draw(day(), &mut rng);
        let charged = station.charge(&plan, &[]);

        let records: Vec<_> = charged
            .records
            .iter()
            .map(|raw| ocmf::Record::parse(raw).expect("the station emits valid OCMF"))
            .collect();
        let evidence = Evidence::assemble(&records, &registry(&station), plan.started_at);

        assert!(
            evidence.problems.is_empty(),
            "{:?}",
            evidence.reasons().collect::<Vec<_>>()
        );
        assert_eq!(
            evidence.billable_energy(),
            Some(charged.series.total().unwrap()),
            "the signed register and the CSMS series are two views of one event"
        );
    }

    #[test]
    fn the_register_carries_across_sessions() {
        // A session is a *difference* between two readings, so the second
        // session on a post starts where the first left it. A station that
        // resets to zero hides an off-by-one nobody would notice.
        let mut rng = Rng::new(2);
        let mut station = VirtualStation::new(0, &mut rng);
        let opening = station.register_kwh();

        let first = SessionPlan::draw(day(), &mut rng);
        let a = station.charge(&first, &[]);
        let after_first = station.register_kwh();
        assert_eq!(after_first - opening, a.series.total().unwrap().kwh());

        let second = SessionPlan::draw(day(), &mut rng);
        let b = station.charge(&second, &[]);
        assert_eq!(b.series.first().register.kwh(), after_first);
        assert_eq!(
            station.register_kwh() - opening,
            a.series.total().unwrap().kwh() + b.series.total().unwrap().kwh()
        );
    }

    #[test]
    fn the_taper_delivers_exactly_the_planned_energy() {
        // The curve may be any shape it likes; what it may not do is lose or
        // invent a kilowatt-hour on the way.
        let mut rng = Rng::new(3);
        for _ in 0..200 {
            let mut station = VirtualStation::new(0, &mut rng);
            let plan = SessionPlan::draw(day(), &mut rng);
            let charged = station.charge(&plan, &[]);
            assert_eq!(
                charged.series.total().unwrap().kwh(),
                plan.energy.kwh(),
                "the curve must conserve"
            );
        }
    }

    #[test]
    fn the_curve_actually_tapers() {
        // A straight line makes every interpolation exact, which is the case
        // the split's proof does not need.
        let mut rng = Rng::new(4);
        let mut station = VirtualStation::new(0, &mut rng);
        let plan = SessionPlan {
            started_at: day(),
            ended_at: day() + time::Duration::hours(2),
            energy: Energy::from_kwh(Decimal::from(40)).unwrap(),
            clock_aligned: true,
        };
        let charged = station.charge(&plan, &[]);
        let readings = charged.series.readings();

        let first = (readings[1].register.kwh() - readings[0].register.kwh()).abs();
        let last = (readings[readings.len() - 1].register.kwh()
            - readings[readings.len() - 2].register.kwh())
        .abs();
        assert!(
            first > last,
            "the first quarter hour takes more than the last"
        );
    }

    #[test]
    fn each_fault_reaches_the_records() {
        let mut rng = Rng::new(5);
        let station = VirtualStation::new(0, &mut rng);
        let plan = SessionPlan {
            started_at: day(),
            ended_at: day() + time::Duration::hours(1),
            energy: Energy::from_kwh(Decimal::from(20)).unwrap(),
            clock_aligned: true,
        };

        let clean = station.clone().charge(&plan, &[]);
        assert!(clean.records.iter().all(|r| r.contains(r#""ST":"G""#)));

        let substitute = station.clone().charge(&plan, &[Fault::SubstituteReading]);
        assert!(substitute.records.iter().any(|r| r.contains(r#""ST":"S""#)));

        let unsynchronised = station.clone().charge(&plan, &[Fault::UnsynchronisedClock]);
        assert!(unsynchronised.records.iter().all(|r| r.contains(" U\"")));

        let exported = station
            .clone()
            .charge(&plan, &[Fault::WrongDirectionRegister]);
        assert!(exported.records.iter().all(|r| r.contains(EXPORT_REGISTER)));

        let dropped = station.clone().charge(&plan, &[Fault::DroppedRecord]);
        assert_eq!(dropped.records.len(), clean.records.len() - 1);

        let exception = station
            .clone()
            .charge(&plan, &[Fault::ExceptionDuringCharging]);
        assert!(exception.records.iter().any(|r| r.contains(r#""TX":"X""#)));
    }

    #[test]
    fn a_tampered_record_no_longer_verifies() {
        let mut rng = Rng::new(6);
        let mut station = VirtualStation::new(0, &mut rng);
        let plan = SessionPlan::draw(day(), &mut rng);
        let charged = station.charge(&plan, &[Fault::TamperedValue]);

        let records: Vec<_> = charged
            .records
            .iter()
            .map(|raw| ocmf::Record::parse(raw).unwrap())
            .collect();
        let evidence = Evidence::assemble(&records, &registry(&station), plan.started_at);
        assert!(!evidence.is_billable());
        assert!(
            evidence.chain.is_none(),
            "no chain over an unverified record"
        );
    }

    #[test]
    fn one_seed_is_one_fleet() {
        let build = |seed: u64| {
            let mut rng = Rng::new(seed);
            let mut station = VirtualStation::new(3, &mut rng);
            let plan = SessionPlan::draw(day(), &mut rng);
            station.charge(&plan, &[]).records
        };
        assert_eq!(build(77), build(77));
        assert_ne!(build(77), build(78));
    }
}
