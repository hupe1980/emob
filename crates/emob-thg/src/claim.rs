//! The notification `[38k §8(1)]` asks for, and what it refuses to contain.

use crate::error::ThgError;
use crate::factors::{DriveEfficiency, EmissionsBasis, MJ_PER_KWH, counting_factor};
use emob_cdr::{Cdr, CdrLedger};
use emob_core::crossing::{Crossing, Note};
use emob_core::obligation::{ObligationId, Status, assess};
use emob_core::station::ChargePointProfile;
use emob_core::{Direction, Energy, EvseId};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, Month};

/// The country whose electricity-tax territory `[38k §5(1)]` names.
const TAX_TERRITORY: &str = "DE";

/// Who is filing, and for whose points `[38k §5(1) S. 2, §5(2)]`.
///
/// The third party is the operator itself, or a person the operator has
/// designated in Textform — and **one** designated person per operator per
/// obligation year. This crate can see the half of that rule which is inside
/// one notification: a point whose operator has not designated this filer does
/// not belong in it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Attribution {
    /// The third party filing the notification.
    pub third_party: String,
    /// The operator identifiers whose designation it holds — the operator part
    /// of an EVSE ID `[AFIR Art. 20(1)]`, uppercased.
    pub operators: BTreeSet<String>,
}

impl Attribution {
    /// An operator filing for its own points.
    #[must_use]
    pub fn own(operator: &str) -> Self {
        Self {
            third_party: operator.to_ascii_uppercase(),
            operators: BTreeSet::from([operator.to_ascii_uppercase()]),
        }
    }

    /// A designated third party and the operators that designated it.
    pub fn designated<I, S>(third_party: impl Into<String>, operators: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            third_party: third_party.into(),
            operators: operators
                .into_iter()
                .map(|o| o.as_ref().to_ascii_uppercase())
                .collect(),
        }
    }

    /// Whether the filer holds this operator's designation.
    #[must_use]
    pub fn covers(&self, operator: &str) -> bool {
        self.operators.contains(&operator.to_ascii_uppercase())
    }
}

/// The window `[38k §6(1) S. 2 Nr. 5]` asks for when the energy was not
/// withdrawn across the whole obligation year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Window {
    /// The first day a countable record started on.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub from: Date,
    /// The last day a countable record ended on.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub to: Date,
}

/// One point's line in the notification `[38k §6(1) S. 2]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PointRecord {
    /// Nr. 1 — the identification code the ID registration organisation
    /// assigned `[AFIR Art. 20(1)]`.
    pub evse_id: EvseId,
    /// Nr. 3 — where the point is.
    pub location: String,
    /// Nr. 4 — the energetic quantity withdrawn for use in electric road
    /// vehicles, determined in conformity with the measuring and calibration
    /// law. Held in kilowatt-hours and reported in megawatt-hours.
    pub energy: Energy,
    /// Nr. 5 — the window the energy was withdrawn in, when it is **not** the
    /// whole obligation year.
    ///
    /// `None` says "the whole year", which is the paragraph's own condition
    /// for leaving the field out — not "unknown".
    pub window: Option<Window>,
    /// How many settled records the quantity came from. Not asked for, and
    /// kept because a line nobody can trace back to sessions is a line nobody
    /// can defend.
    pub sessions: u32,
}

impl PointRecord {
    /// Nr. 4's unit: the exact quantity in megawatt-hours.
    ///
    /// Exact rather than rounded. A meter states three decimals of a
    /// kilowatt-hour on purpose, and dividing by a thousand is a decimal point
    /// moving rather than an approximation.
    #[must_use]
    pub fn megawatt_hours(&self) -> Decimal {
        self.energy.kwh() / Decimal::from(1000)
    }
}

/// A `[38k §8(1)]` notification: every eligible point, its energy, and the
/// three factors the emissions follow from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Claim {
    /// The obligation year.
    pub year: i32,
    /// Who is filing, and for whom.
    pub attribution: Attribution,
    /// One line per point, ordered by identifier.
    pub records: Vec<PointRecord>,
    /// The emissions value and the announcement it came from.
    pub basis: EmissionsBasis,
    /// Anlage 3's adjustment factor.
    pub efficiency: DriveEfficiency,
}

/// Which route of the Verordnung a notification is filed under.
///
/// The two are not a flag on one claim: `[38k §6]` counts metered kilowatt-hours
/// at a **public** point and is filed by its operator or their designated third
/// party; `[38k §7]` counts a published *Schätzwert* per registered battery-
/// electric vehicle and is filed against a Zulassungsbescheinigung. This crate
/// builds the first. The enum exists because the *deadlines* differ, and a
/// deadline is the one fact about the second that is worth stating before the
/// claim type exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Route {
    /// `[38k §6]` — metered energy from publicly accessible charge points.
    PublicChargePoints,
    /// `[38k §7]` — the estimate route, *„in anderen Fällen"*.
    EstimatedPerVehicle,
}

impl Route {
    /// The last day a notification for `year` may be filed under this route
    /// `[38k §8(1) S. 1]`.
    ///
    /// > Der Dritte teilt der zuständigen Stelle … die energetischen Mengen des
    /// > elektrischen Stroms mit, die 1. nach § 6 … entnommen wurde, **bis zum
    /// > Ablauf des 28. Februar des Folgejahres** oder 2. nach § 7 … **bis zum
    /// > Ablauf des 15. November des jeweiligen Verpflichtungsjahres**.
    ///
    /// The two dates are not variations of one rule and one of them is not even
    /// in the following year: a `[38k §7]` claim is due **inside** the year it
    /// is about, six weeks before that year ends. An operator that files both
    /// routes on one calendar has to hold two dates, and this is them.
    ///
    /// The February date is a fixed day and not the end of the month, so a leap
    /// year does **not** move it: 2028's notification is due 28 February 2029
    /// and 2027's is due 28 February 2028, which is a leap year and has a 29th
    /// the Verordnung does not give you.
    ///
    /// # Panics
    ///
    /// Only for a `year` at the edge of `time::Date`'s own supported range, at
    /// which point the obligation year is not one anybody is filing for.
    #[must_use]
    pub fn deadline(self, year: i32) -> Date {
        match self {
            Self::PublicChargePoints => Date::from_calendar_date(year + 1, Month::February, 28)
                .expect("28 February is a date in every year"),
            Self::EstimatedPerVehicle => Date::from_calendar_date(year, Month::November, 15)
                .expect("15 November is a date in every year"),
        }
    }

    /// Whether a notification for `year` filed on `on` is still in time.
    ///
    /// Inclusive of the deadline itself — *„bis zum Ablauf des"* is the end of
    /// that day, not its start.
    #[must_use]
    pub fn in_time(self, year: i32, on: Date) -> bool {
        on <= self.deadline(year)
    }
}

impl Claim {
    /// The route this claim is filed under — always `[38k §6]`, because that is
    /// the claim this crate builds.
    ///
    /// Stated rather than assumed so that [`Self::deadline`] reads it from the
    /// document instead of from the reader's memory, and so that a `[38k §7]`
    /// claim can be added beside it without changing what this one means.
    pub const ROUTE: Route = Route::PublicChargePoints;

    /// The last day this notification may be filed `[38k §8(1) S. 1 Nr. 1]`.
    ///
    /// A date rather than a check, because a domain crate reads no clock: the
    /// service that files compares it against today, and a replay two years
    /// later gets the same answer it got then.
    #[must_use]
    pub fn deadline(&self) -> Date {
        Self::ROUTE.deadline(self.year)
    }

    /// Whether filing on `on` is in time.
    ///
    /// The whole of what a missed deadline costs is the claim: there is no late
    /// filing and no partial credit, and a year of a fleet's public
    /// kilowatt-hours is a five- or six-figure sum. `[38k §8(5)]` adds the other
    /// way it is lost — *„Mitteilungen … die unvollständig sind, werden von der
    /// zuständigen Stelle abgelehnt"* — which is what every refusal in
    /// [`ClaimBuilder`] is for.
    #[must_use]
    pub fn in_time(&self, on: Date) -> bool {
        Self::ROUTE.in_time(self.year, on)
    }

    /// The energetic quantity across every point, in megawatt-hours
    /// `[38k §6(1) S. 2 Nr. 4]`.
    #[must_use]
    pub fn megawatt_hours(&self) -> Decimal {
        self.records.iter().map(PointRecord::megawatt_hours).sum()
    }

    /// The quantity as it enters the reference-value calculation — multiplied
    /// by the year's factor `[38k §5(3) S. 1]`.
    ///
    /// # Panics
    ///
    /// If [`Self::year`] states a year `[38k §5(3)]` counts nothing in — before
    /// [`crate::factors::FIRST_COUNTED_YEAR`]. [`ClaimBuilder`] refuses one, so
    /// no claim this crate assembles can reach it; the fields are public, so a
    /// claim deserialised from a store or written by hand can. That is a
    /// document nobody may file, and it is better to say so than to return a
    /// number for a year the Verordnung has none for.
    #[must_use]
    pub fn counted_megawatt_hours(&self) -> Decimal {
        let factor = counting_factor(self.year).expect("the year was checked at construction");
        self.megawatt_hours() * factor
    }

    /// The greenhouse-gas emissions of that electricity, in kilograms of CO₂
    /// equivalent `[38k §5(3) S. 2]`.
    ///
    /// The energetic quantity, times the year's factor, times the announced
    /// emissions value, times Anlage 3's drive-efficiency factor. Exact
    /// decimal throughout: the conversion to megajoules is `× 3.6`, which is
    /// exact, so nothing here is an approximation and two runs agree to the
    /// last digit.
    ///
    /// What this is **not** is the quota an operator sells. That is the
    /// difference against the reference value in § 37a of the
    /// Bundes-Immissionsschutzgesetz, which is the competent authority's
    /// arithmetic and not this Verordnung's — so this crate stops at the
    /// figure the Verordnung defines.
    #[must_use]
    pub fn emissions_kg_co2e(&self) -> Decimal {
        let mj = self.counted_megawatt_hours() * Decimal::from(1000) * MJ_PER_KWH;
        mj * self.basis.grams_co2e_per_mj() * self.efficiency.factor() / Decimal::from(1000)
    }
}

/// Builds a notification out of profiles and settled records, refusing what
/// `[38k §6(3)]` makes ineligible instead of quietly including it.
#[derive(Debug, Clone)]
pub struct ClaimBuilder {
    year: i32,
    attribution: Attribution,
    basis: EmissionsBasis,
    efficiency: DriveEfficiency,
    records: BTreeMap<String, PointRecord>,
    notes: Vec<Note>,
}

impl ClaimBuilder {
    /// A notification for one obligation year, on one announced basis.
    ///
    /// # Errors
    ///
    /// [`ThgError::YearNotCounted`] for a year `[38k §5(3)]` states no factor
    /// for, and [`ThgError::SourceNotYetCountable`] for a renewable basis whose
    /// source does not count yet.
    pub fn new(
        year: i32,
        attribution: Attribution,
        basis: EmissionsBasis,
        efficiency: DriveEfficiency,
    ) -> Result<Self, ThgError> {
        if counting_factor(year).is_none() {
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
            records: BTreeMap::new(),
            notes: Vec::new(),
        })
    }

    /// The obligation year's first and last day.
    #[must_use]
    fn bounds(&self) -> (Date, Date) {
        let first = Date::from_calendar_date(self.year, Month::January, 1)
            .expect("1 January is a date in every year");
        let last = Date::from_calendar_date(self.year, Month::December, 31)
            .expect("31 December is a date in every year");
        (first, last)
    }

    /// Add one point and the records that ran on it.
    ///
    /// The ledger is read through [`CdrLedger::live`], so a corrected record
    /// contributes once and the superseded original contributes nothing — the
    /// same rule `emob-billing` applies, for the same reason.
    ///
    /// # Errors
    ///
    /// [`ThgError::NotPublic`] and [`ThgError::NotEligible`] for a point
    /// `[38k §6(3)]` refuses, [`ThgError::OutsideTaxTerritory`] for a point
    /// outside Germany, [`ThgError::NoAgreement`] for one whose operator has
    /// not designated this filer, and [`ThgError::Unmeasured`] for a record
    /// carrying energy no meter signed.
    pub fn point(
        &mut self,
        profile: &ChargePointProfile,
        location: impl Into<String>,
        ledger: &CdrLedger,
    ) -> Result<(), ThgError> {
        let id = profile.evse_id.canonical().to_string();

        if profile.evse_id.country_code() != TAX_TERRITORY {
            return Err(ThgError::OutsideTaxTerritory {
                evse_id: id,
                country: profile.evse_id.country_code().to_string(),
            });
        }
        if !self.attribution.covers(profile.evse_id.operator_id()) {
            return Err(ThgError::NoAgreement {
                third_party: self.attribution.third_party.clone(),
                operator: profile.evse_id.operator_id().to_string(),
                evse_id: id,
            });
        }

        // Judged on the last day of the obligation year: a point that became
        // eligible in March is eligible for the year, and the records outside
        // its window are excluded by date rather than by verdict.
        let (first, last) = self.bounds();
        let assessment = assess(profile, last);
        match assessment.status_of(ObligationId::ThgEligibility) {
            Some(Status::Satisfied) => {}
            Some(Status::NotApplicable) => return Err(ThgError::NotPublic { evse_id: id }),
            _ => {
                let remedy = assessment
                    .failing()
                    .find(|f| f.obligation.id == ObligationId::ThgEligibility)
                    .map_or_else(
                        || "the duty is not in force on this date".to_string(),
                        |f| f.obligation.remedy.to_string(),
                    );
                return Err(ThgError::NotEligible {
                    evse_id: id,
                    remedy,
                });
            }
        }

        let Contribution {
            energy,
            sessions,
            earliest,
            latest,
        } = self.contribution(profile, ledger, first, last)?;

        if sessions == 0 {
            self.notes.push(Note::new(
                format!("/records/{id}"),
                "no countable record in the obligation year, so the point has no line".to_string(),
            ));
            return Ok(());
        }

        // Nr. 5 is stated only when the window is narrower than the year.
        let window = match (earliest, latest) {
            (Some(from), Some(to)) if from > first || to < last => Some(Window { from, to }),
            _ => None,
        };

        // `[38k §8(1) S. 3]`: one notification per point per obligation year.
        // A second line for a point already in this notification is the same
        // rule one level down, and letting it overwrite loses the first
        // window's energy without failing anything.
        if self.records.contains_key(&id) {
            return Err(ThgError::PointAlreadyReported { evse_id: id });
        }
        self.records.insert(
            id,
            PointRecord {
                evse_id: profile.evse_id.clone(),
                location: location.into(),
                energy,
                window,
                sessions,
            },
        );
        Ok(())
    }

    /// What one point's records contribute to this obligation year.
    ///
    /// Split out of [`Self::point`] because it is the half that reads the
    /// ledger, and the half above it reads the calendar: a point is eligible or
    /// it is not, and *then* its records are counted.
    ///
    /// # Errors
    ///
    /// [`ThgError::Unmeasured`] for a contributing record carrying energy no
    /// meter signed `[38k §6(3) Nr. 2]`.
    fn contribution(
        &mut self,
        profile: &ChargePointProfile,
        ledger: &CdrLedger,
        first: Date,
        last: Date,
    ) -> Result<Contribution, ThgError> {
        let id = profile.evse_id.canonical().to_string();
        let mut out = Contribution::default();

        for cdr in ledger.live().filter(|c| c.evse_id == profile.evse_id) {
            if cdr.direction != Direction::Import {
                self.notes.push(Note::new(
                    format!("/records/{id}"),
                    format!(
                        "{} is an export: `[38k §5(1)]` counts electricity withdrawn for use in the vehicle",
                        cdr.key.id
                    ),
                ));
                continue;
            }

            // The energy this record withdrew **inside the obligation year**,
            // period by period — not the whole record because it happened to
            // start inside it. See `energy_in_year`.
            let (inside, outside) = energy_in_year(cdr, first, last);
            if inside.is_zero() {
                continue;
            }

            // Asked only of a record that contributes. A record from another
            // year is not this notification's to vouch for.
            //
            // **Present is not verified.** `[38k §6(3) Nr. 2]` wants the energy
            // determined in conformity with metrology law, which is exactly the
            // question `emob-eichrecht` already answered — and asking only
            // whether an `EvidenceRef` exists asks the weaker half of it. A
            // record whose chain did not hold up carries one, says
            // `energy_billable: false`, and is refused for money by the CDR
            // builder and by `priced` — and used to be counted here, in a file
            // whose whole claim is that only signed kilowatt-hours reach it.
            // Evidence that is present and failed is worse than evidence that is
            // absent, and this is the third place that had to learn it (D231).
            let signed = cdr
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.energy_billable);
            if !signed {
                return Err(ThgError::Unmeasured {
                    cdr: cdr.key.id.to_string(),
                });
            }

            if !outside.is_zero() {
                self.notes.push(Note::new(
                    format!("/records/{id}"),
                    format!(
                        "{} spans the turn of the obligation year: {inside} of it was withdrawn \
                         in {}, and {outside} belongs to the neighbouring year and is filed \
                         there. The record's own total is the sum of the two, so a THG file and \
                         a billing file will state different figures for this session on purpose \
                         `[38k §5(1)]`",
                        cdr.key.id, self.year
                    ),
                ));
            }

            out.energy = sum(out.energy, inside);
            out.sessions += 1;
            // The day a period withdrew on is the day its quarter hour
            // **began**, for both bounds. A settlement period is half-open, so
            // the one running 23:45 to 00:00 withdrew nothing on the 1st — and
            // reading its exclusive end as an inclusive day would state a
            // window ending in a year this notification does not report.
            for period in periods_in_year(cdr, first, last) {
                let day = period.quarter_hour.start().date();
                out.earliest = Some(out.earliest.map_or(day, |e: Date| e.min(day)));
                out.latest = Some(out.latest.map_or(day, |l: Date| l.max(day)));
            }
        }

        Ok(out)
    }

    /// The notification, and the account of what did not reach it.
    ///
    /// # Errors
    ///
    /// [`ThgError::NothingToReport`] when no point contributed.
    pub fn build(self) -> Result<Crossing<Claim>, ThgError> {
        if self.records.is_empty() {
            return Err(ThgError::NothingToReport);
        }
        let claim = Claim {
            year: self.year,
            attribution: self.attribution,
            records: self.records.into_values().collect(),
            basis: self.basis,
            efficiency: self.efficiency,
        };
        let mut crossing = Crossing::lossless(claim);
        crossing.absorb_notes("", self.notes);
        Ok(crossing)
    }
}

/// One point's contribution to one obligation year.
#[derive(Debug, Default)]
struct Contribution {
    energy: Energy,
    sessions: u32,
    earliest: Option<Date>,
    latest: Option<Date>,
}

/// Adding two energies cannot fail here: both are non-negative by
/// construction, and the sum of two non-negative decimals is one.
fn sum(total: Energy, more: Energy) -> Energy {
    Energy::from_kwh(total.kwh() + more.kwh())
        .expect("the sum of two non-negative energies is non-negative")
}

/// The charging periods of a record that fall inside an obligation year.
///
/// # Which instant puts a period in a year
///
/// The **start** of its quarter hour. `[38k §5(1)]` counts the electricity
/// *withdrawn* in the obligation year, and the quarter hour running 23:45 to
/// 00:00 on 31 December withdrew its energy on 31 December — whatever it is
/// labelled.
///
/// That distinction is load-bearing here, because German metrology labels a
/// Messperiode by its **end** `[PTB-A 50.7 §3.1.7.2]`, so the same quarter hour
/// appears in an MSCONS load profile under `2027-01-01T00:00`. Reading
/// [`QuarterHour::metering_timestamp`] here would move a whole quarter hour of
/// every New Year's Eve into the following obligation year — the same
/// fifteen-minute shift that error causes on the market side, arriving as a tax
/// question instead of an allocation one.
fn periods_in_year(
    cdr: &Cdr,
    first: Date,
    last: Date,
) -> impl Iterator<Item = &emob_cdr::ChargingPeriod> {
    cdr.periods.iter().filter(move |period| {
        let day = period.quarter_hour.start().date();
        day >= first && day <= last
    })
}

/// A record's energy split at the turn of the obligation year: what was
/// withdrawn inside it, and what was not.
///
/// # Why a session is not attributed whole
///
/// A charge that begins at 23:45 on 31 December and ends at 00:15 on 1 January
/// withdrew half its energy in each obligation year. Attributing the record to
/// the year it *started* in — which is what a `started_at` filter does — files
/// the January half under December and files nothing at all in January, so the
/// operator loses the quota on it and the notification states a figure for a
/// year in which that electricity was not withdrawn.
///
/// The workspace already holds the answer: a CDR carries the quarter-hour
/// periods `emob_session::split` produced, and that split **conserves
/// exactly**. So the two halves sum to the record's own total to the last
/// digit, and a session that does not cross the boundary yields its whole
/// energy on one side and zero on the other — the ordinary case, unchanged.
fn energy_in_year(cdr: &Cdr, first: Date, last: Date) -> (Energy, Energy) {
    let mut inside = Energy::ZERO;
    let mut outside = Energy::ZERO;
    for period in &cdr.periods {
        let day = period.quarter_hour.start().date();
        if day >= first && day <= last {
            inside = sum(inside, period.energy);
        } else {
            outside = sum(outside, period.energy);
        }
    }
    (inside, outside)
}
