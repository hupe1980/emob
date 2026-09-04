//! The time zone a wall clock is read in.
//!
//! # Why an offset is not a zone, and why that is a money question
//!
//! A `time::OffsetDateTime` carries a **UTC offset** — what a clock was written
//! with. A **zone** is the rule that decides the offset at any instant,
//! including the two days a year it changes. The two are not interchangeable,
//! and in this workspace the difference is paid in cents.
//!
//! A tariff that charges 0.30 from 22:00 is a statement about **local civil time
//! at the charge point** `[OCPI 2.3.0 §mod_tariffs]`, and OCPI carries the zone
//! it is read in on the Location, where it is mandatory
//! `[OCPI 2.3.0 §mod_locations_location_object]`. Judge that against whatever
//! offset the timestamps happened to carry and the same physical session costs a
//! different amount depending on how its clock was written down: twenty
//! kilowatt-hours under a German night tariff come to €6.00 stamped `+01:00` and
//! €9.00 stamped `Z`, silently, because nothing failed to match.
//!
//! Every session an eMSP re-rates from a roaming partner arrives in UTC, so the
//! wrong reading is the ordinary one.
//!
//! # Why a zone database does not break replay
//!
//! The database is **compiled in, not read**: [`TimeZone`] resolves against the
//! copy `jiff-tzdb` embeds at build time, so nothing opens
//! `/usr/share/zoneinfo`, reads `TZ` or asks the operating system anything —
//! `just purity` holds and two machines with different system tzdata agree.
//! `Cargo.lock` pins the version a tagged build shipped. And a tzdb release
//! announces what a zone *will* do, while the civil offsets of instants that
//! have already happened are frozen — which are the only instants a settled
//! session has.
//!
//! One property of the bundled copy is worth stating rather than discovering:
//! it carries **no transitions before 1970**, so a zone asked about 1930 answers
//! with its earliest post-1970 rule rather than with the offset that was in
//! force. Harmless here — no charging session predates 1970 — and it is the
//! reason `QuarterHour::periods_in_local_day`'s one non-96 fixture is Liberia in
//! 1970 rather than Amsterdam in 1930 (D218).

use core::fmt;

/// A named IANA time zone, resolved once against a compiled-in database.
///
/// Identity is the **name**: two `TimeZone`s are equal when they name the same
/// zone, so a tariff's fingerprint changes when its zone does and a serialised
/// tariff round-trips to something that compares equal.
///
/// ```
/// use emob_core::TimeZone;
/// use time::macros::datetime;
///
/// let berlin = TimeZone::new("Europe/Berlin")?;
///
/// // The same instant, in two spellings, is one instant — and one local hour.
/// let winter = berlin.local(datetime!(2026-01-02 21:00 +0));
/// assert_eq!(winter.time.hour(), 22);
///
/// // …and the zone knows which side of the clock change it is on.
/// let summer = berlin.local(datetime!(2026-07-02 21:00 +0));
/// assert_eq!(summer.time.hour(), 23);
/// # Ok::<(), emob_core::ZoneError>(())
/// ```
#[derive(Clone)]
pub struct TimeZone {
    name: Box<str>,
    tz: jiff::tz::TimeZone,
}

/// A wall-clock reading: the date, the time and the weekday a zone puts an
/// instant at, and the offset that was in force.
///
/// The three fields a tariff restriction is judged against, computed together
/// because computing them separately is three chances to use two different
/// instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// The local calendar date.
    pub date: time::Date,
    /// The local time of day.
    pub time: time::Time,
    /// The local weekday.
    pub weekday: time::Weekday,
    /// The UTC offset in force at that instant, in seconds east of UTC.
    pub offset_seconds: i32,
}

/// A zone name no database knows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "'{name}' is not an IANA time zone name; a charge point's zone is a tzdata identifier such as 'Europe/Berlin'"
)]
pub struct ZoneError {
    /// The name that was offered.
    pub name: String,
}

impl TimeZone {
    /// The zone every instant is already in — offset zero, no transitions.
    ///
    /// Correct for a tariff whose restrictions really are written in UTC, and
    /// wrong for one written in local civil time, which is almost all of them.
    /// It is a named constructor rather than a default for that reason: a zone
    /// nobody chose is the bug this module exists to remove.
    #[must_use]
    pub fn utc() -> Self {
        Self {
            name: "UTC".into(),
            tz: jiff::tz::TimeZone::UTC,
        }
    }

    /// Resolve an IANA zone name — `"Europe/Berlin"`, `"Europe/Amsterdam"`.
    ///
    /// # Errors
    ///
    /// [`ZoneError`] when the compiled-in database does not carry the name. A
    /// zone that cannot be resolved is refused rather than silently replaced
    /// with UTC, because the substitution is invisible and moves prices.
    pub fn new(name: &str) -> Result<Self, ZoneError> {
        let tz = jiff::tz::db().get(name).map_err(|_| ZoneError {
            name: name.to_owned(),
        })?;
        Ok(Self {
            // `jiff` canonicalises an alias to the zone it points at; the name
            // kept here is the one the caller wrote, because that is what the
            // Location document says and what a partner will compare against.
            name: name.into(),
            tz,
        })
    }

    /// The IANA name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The wall clock this zone puts an instant at.
    #[must_use]
    pub fn local(&self, at: time::OffsetDateTime) -> Local {
        let zoned = self.zoned(at);
        let offset_seconds = zoned.offset().seconds();
        // Build the local civil fields from the instant shifted into the
        // offset that was actually in force, rather than from `jiff`'s own
        // calendar types, so every date this workspace compares is a
        // `time::Date` produced the same way.
        let shifted = at.to_offset(
            time::UtcOffset::from_whole_seconds(offset_seconds).unwrap_or(time::UtcOffset::UTC),
        );
        Local {
            date: shifted.date(),
            time: shifted.time(),
            weekday: shifted.weekday(),
            offset_seconds,
        }
    }

    /// **Every** instant at which a local wall clock reads `date` and `time` in
    /// this zone, in time order.
    ///
    /// # Why this is a list
    ///
    /// A civil time is not an instant. Twice a year in every zone that observes
    /// summer time:
    ///
    /// - a **gap** swallows an hour — `02:30` simply does not happen on the
    ///   spring-forward day — and the instant the wall clock passes the
    ///   threshold is the transition itself, so there is exactly one;
    /// - a **fold** repeats an hour — `02:30` happens twice on the
    ///   autumn day — and there are exactly **two**.
    ///
    /// A tariff that switches to a night rate at 02:30 switches twice on the
    /// fold day, and a cut placed only at the first leaves the second hour
    /// priced by whatever applied before it. That is a real hour of a real
    /// tariff, so both are returned and the caller cuts at both.
    ///
    /// Empty only for a date `time` and `jiff` disagree about the existence of,
    /// which no calendar date a session carries reaches.
    #[must_use]
    pub fn instants_at(&self, date: time::Date, at: time::Time) -> Vec<time::OffsetDateTime> {
        let Some(civil) = civil_of(date, at) else {
            return Vec::new();
        };
        let ambiguous = self.tz.to_ambiguous_timestamp(civil);
        let seconds: Vec<i64> = match ambiguous.offset() {
            // The hour that happens twice: both crossings are real.
            jiff::tz::AmbiguousOffset::Fold { .. } => [ambiguous.earlier(), ambiguous.later()]
                .into_iter()
                .flatten()
                .map(jiff::Timestamp::as_second)
                .collect(),
            // The hour that never happens, and the ordinary case: one instant.
            // For a gap `compatible` is the forward shift — the instant the
            // wall clock jumps past the threshold, which is when the price
            // changes.
            jiff::tz::AmbiguousOffset::Gap { .. }
            | jiff::tz::AmbiguousOffset::Unambiguous { .. } => ambiguous
                .compatible()
                .ok()
                .map(jiff::Timestamp::as_second)
                .into_iter()
                .collect(),
        };

        let mut out: Vec<time::OffsetDateTime> = seconds
            .into_iter()
            .filter_map(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The first instant at which a local wall clock reads `date` and `time`.
    ///
    /// The single-answer form of [`Self::instants_at`], for a caller that wants
    /// a boundary rather than every crossing of one — the start of a local day,
    /// say. `None` where the civil value does not name a date at all.
    #[must_use]
    pub fn instant_at(&self, date: time::Date, at: time::Time) -> Option<time::OffsetDateTime> {
        self.instants_at(date, at).first().copied()
    }

    fn zoned(&self, at: time::OffsetDateTime) -> jiff::Zoned {
        let timestamp = jiff::Timestamp::from_second(at.unix_timestamp())
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        timestamp.to_zoned(self.tz.clone())
    }
}

/// A `time` date and time as `jiff`'s civil datetime, or `None` where the two
/// calendars disagree about the value — which no date a charging session
/// carries reaches.
fn civil_of(date: time::Date, at: time::Time) -> Option<jiff::civil::DateTime> {
    jiff::civil::DateTime::new(
        i16::try_from(date.year()).ok()?,
        date.month() as i8,
        i8::try_from(date.day()).ok()?,
        i8::try_from(at.hour()).ok()?,
        i8::try_from(at.minute()).ok()?,
        i8::try_from(at.second()).ok()?,
        0,
    )
    .ok()
}

impl PartialEq for TimeZone {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for TimeZone {}

impl core::hash::Hash for TimeZone {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl PartialOrd for TimeZone {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimeZone {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl fmt::Debug for TimeZone {
    /// The name alone. The resolved zone is a compiled table, and printing it
    /// in a test failure buries the one field that identifies it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TimeZone({})", self.name)
    }
}

impl fmt::Display for TimeZone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl core::str::FromStr for TimeZone {
    type Err = ZoneError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for TimeZone {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.name)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for TimeZone {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        // Through `new`, so a zone that arrives in a document is resolved on the
        // way in rather than trusted because it was already typed — the same
        // rule `Currency` follows one module over.
        let name = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(&name).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime, time as clock};

    #[test]
    fn a_zone_reads_the_same_instant_differently_in_the_two_halves_of_the_year() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();

        let winter = berlin.local(datetime!(2026-01-02 21:00 +0));
        assert_eq!(winter.date, date!(2026 - 01 - 02));
        assert_eq!(winter.time, clock!(22:00));
        assert_eq!(winter.offset_seconds, 3600);

        let summer = berlin.local(datetime!(2026-07-02 21:00 +0));
        assert_eq!(summer.time, clock!(23:00));
        assert_eq!(summer.offset_seconds, 7200);
    }

    #[test]
    fn the_offset_the_timestamp_carries_does_not_change_the_answer() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();
        // One instant, three spellings.
        let utc = berlin.local(datetime!(2026-01-02 21:00 +0));
        let local = berlin.local(datetime!(2026-01-02 22:00 +1));
        let elsewhere = berlin.local(datetime!(2026-01-03 06:00 +9));
        assert_eq!(utc, local);
        assert_eq!(utc, elsewhere);
    }

    #[test]
    fn a_local_midnight_is_an_instant_and_a_clock_change_moves_it() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();
        assert_eq!(
            berlin.instant_at(date!(2026 - 01 - 02), time::Time::MIDNIGHT),
            Some(datetime!(2026-01-01 23:00 +0))
        );
        // In summer the same local midnight is an hour earlier in UTC.
        assert_eq!(
            berlin.instant_at(date!(2026 - 07 - 02), time::Time::MIDNIGHT),
            Some(datetime!(2026-07-01 22:00 +0))
        );
    }

    #[test]
    fn a_civil_time_that_never_happens_is_one_crossing_and_it_moves_forward() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();
        // 02:00–03:00 does not exist on the spring-forward day, so the wall
        // clock passes 02:30 exactly once — at the transition itself.
        let crossings = berlin.instants_at(date!(2026 - 03 - 29), clock!(2:30));
        assert_eq!(crossings, vec![datetime!(2026-03-29 01:30 +0)]);
        assert_eq!(berlin.local(crossings[0]).time, clock!(3:30));
    }

    #[test]
    fn a_civil_time_that_happens_twice_is_two_crossings() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();
        let crossings = berlin.instants_at(date!(2026 - 10 - 25), clock!(2:30));
        // Both are real, an hour apart, and a tariff switching at 02:30
        // switches twice — which is why this returns a list.
        assert_eq!(
            crossings,
            vec![
                datetime!(2026-10-25 00:30 +0),
                datetime!(2026-10-25 01:30 +0)
            ]
        );
        assert_eq!(berlin.local(crossings[0]).offset_seconds, 7200);
        assert_eq!(berlin.local(crossings[1]).offset_seconds, 3600);
        assert_eq!(berlin.local(crossings[1]).time, clock!(2:30));
    }

    #[test]
    fn an_ordinary_civil_time_is_exactly_one_crossing() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();
        assert_eq!(
            berlin.instants_at(date!(2026 - 06 - 01), clock!(22:00)),
            vec![datetime!(2026-06-01 20:00 +0)]
        );
    }

    #[test]
    fn a_zone_no_database_knows_is_refused_rather_than_replaced_with_utc() {
        let error = TimeZone::new("Europe/Atlantis").unwrap_err();
        assert!(error.to_string().contains("Europe/Atlantis"));
        assert!(error.to_string().contains("Europe/Berlin"));
    }

    #[test]
    fn utc_is_a_zone_like_any_other() {
        let utc = TimeZone::utc();
        assert_eq!(utc.name(), "UTC");
        let at = utc.local(datetime!(2026-01-02 22:00 +1));
        assert_eq!(at.time, clock!(21:00));
        assert_eq!(at.offset_seconds, 0);
    }

    #[test]
    fn identity_is_the_name_so_a_fingerprint_notices_a_change() {
        assert_eq!(
            TimeZone::new("Europe/Berlin").unwrap(),
            TimeZone::new("Europe/Berlin").unwrap()
        );
        assert_ne!(
            TimeZone::new("Europe/Berlin").unwrap(),
            TimeZone::new("Europe/Lisbon").unwrap()
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn it_travels_as_its_name_and_is_resolved_on_the_way_back() {
        let berlin = TimeZone::new("Europe/Berlin").unwrap();
        let json = serde_json::to_string(&berlin).unwrap();
        assert_eq!(json, r#""Europe/Berlin""#);
        assert_eq!(serde_json::from_str::<TimeZone>(&json).unwrap(), berlin);
        assert!(serde_json::from_str::<TimeZone>(r#""Europe/Atlantis""#).is_err());
    }
}
