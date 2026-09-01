//! The settlement period: fifteen minutes, and which instant names it.
//!
//! # Why this is vocabulary rather than a detail of the session crate
//!
//! The quarter hour is not something a session has. It is the grid the German
//! market settles on `[A6 §IV.1]`, the grid a meter stores its load profile in
//! `[PTB-A 50.7]`, and — since a tariff change may only take effect at a period
//! boundary — the grid a price may move on. Three crates need to say something
//! about it, so it lives in the one they all depend on.
//!
//! # The timestamp that names a period is not the one this type stores
//!
//! `[PTB-A 50.7 §3.1.7.2]` is explicit, in a footnote that is easy to read past:
//!
//! > Der Zeitstempel, der zu einer Messperiode gehört, ist immer der Zeitpunkt
//! > des **Endes** einer Messperiode (z.B. gehört bei einer Messperiode von
//! > 15 min. der Zeitstempel 00:15:00 zur ersten Messperiode eines Tages).
//!
//! A German meter, an MSCONS load profile and a market partner all label the
//! period 00:00–00:15 as **00:15**. This type labels it **00:00**, because a
//! half-open interval named by its start is the only spelling in which
//! `containing()` is a truncation and consecutive periods tile without
//! arithmetic.
//!
//! Both conventions are internally consistent, and mixing them shifts every
//! slot in a settlement file by fifteen minutes — an error that reconciles to
//! zero in total and is wrong for every individual balance group. So the
//! conversion has a name, [`QuarterHour::metering_timestamp`], and an adapter
//! writing to the market side calls it rather than reaching for `start()`.

use core::fmt;

/// Fifteen minutes of real time, starting at an instant on a quarter-hour
/// boundary.
///
/// Named by its **start**. See the module documentation for why, and for the
/// conversion a market-facing adapter needs.
///
/// Written on the wire as the RFC 3339 spelling of its **start**, through
/// [`crate::wire`]: `time`'s own serialisation is a nine-element array of its
/// internal fields, which no partner can read and no version of `time`
/// promises to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QuarterHour(time::OffsetDateTime);

#[cfg(feature = "serde")]
impl serde::Serialize for QuarterHour {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        time::serde::rfc3339::serialize(&self.0, serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for QuarterHour {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let at = time::serde::rfc3339::deserialize(deserializer)?;
        // A quarter hour that does not start on a boundary is not one. Reading
        // it back through `containing` would silently move a settlement slot;
        // refusing says so where it happened.
        if !Self::is_boundary(at) {
            return Err(D::Error::custom(format!(
                "{at} is not a settlement-period boundary, so it does not name a quarter hour"
            )));
        }
        Ok(Self(at))
    }
}

impl QuarterHour {
    /// Fifteen minutes, in seconds.
    pub const SECONDS: i64 = 900;

    /// How many periods a day has when nothing moves the clock — the number
    /// `[PTB-A 50.7 §3.1.7.2]` requires a meter to preserve across a
    /// synchronisation.
    ///
    /// Not a number anything here counts to: a clock-change day has 92 or 100,
    /// and a grid built by adding fifteen minutes needs no branch for it.
    pub const PERIODS_PER_ORDINARY_DAY: u32 = 96;

    /// The quarter hour that contains `at`.
    #[must_use]
    pub fn containing(at: time::OffsetDateTime) -> Self {
        // Truncate towards negative infinity on the UTC timeline, so a session
        // that starts at 10:07:30 belongs to the slot beginning 10:00 whatever
        // the local offset happens to be.
        let unix = at.unix_timestamp();
        let floored = unix.div_euclid(Self::SECONDS) * Self::SECONDS;
        Self(
            time::OffsetDateTime::from_unix_timestamp(floored)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(at.offset()),
        )
    }

    /// Whether an instant falls exactly on a period boundary.
    ///
    /// The question `[PTB-A 50.7 §3.1.7.2]` makes load-bearing for prices: "Ein
    /// Tarifwechsel ist erst mit dem Beginn der nächsten Messperiode
    /// durchzuführen." A change that lands mid-period would put one settlement
    /// slot under two tariffs, which is a slot nobody can allocate and a price
    /// nobody can reproduce.
    #[must_use]
    pub fn is_boundary(at: time::OffsetDateTime) -> bool {
        at.unix_timestamp().rem_euclid(Self::SECONDS) == 0 && at.nanosecond() == 0
    }

    /// The first period boundary at or after an instant.
    ///
    /// What a tariff change that has to be aligned should be moved to: forward,
    /// never back, because a price may not start applying earlier than it was
    /// published.
    #[must_use]
    pub fn boundary_at_or_after(at: time::OffsetDateTime) -> time::OffsetDateTime {
        if Self::is_boundary(at) {
            at
        } else {
            Self::containing(at).end()
        }
    }

    /// The instant this quarter hour begins.
    #[must_use]
    pub const fn start(self) -> time::OffsetDateTime {
        self.0
    }

    /// The instant it ends, which is the next one's start.
    #[must_use]
    pub fn end(self) -> time::OffsetDateTime {
        self.0 + time::Duration::seconds(Self::SECONDS)
    }

    /// The timestamp German metrology and the market side label this period
    /// with — its **end** `[PTB-A 50.7 §3.1.7.2]`.
    ///
    /// The same instant as [`Self::end`], under the name that says which
    /// convention is being spoken. An adapter writing an MSCONS load profile or
    /// handing a series to `mako-emob` calls this; everything inside this
    /// workspace uses [`Self::start`]. Reaching for the wrong one shifts every
    /// slot by fifteen minutes, which sums to zero and is wrong for every
    /// individual balance group.
    #[must_use]
    pub fn metering_timestamp(self) -> time::OffsetDateTime {
        self.end()
    }

    /// The quarter hour after this one.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.end())
    }
}

impl fmt::Display for QuarterHour {
    /// The period's **start**, to the minute.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            self.0.year(),
            u8::from(self.0.month()),
            self.0.day(),
            self.0.hour(),
            self.0.minute()
        )
    }
}

/// The shortest time span a station's clock may be billed for.
///
/// # The rule
///
/// `[REA 6-A §3.1]` sets an error limit and then a floor:
///
/// > Verwendete Uhren haben im Anwendungsbereich der E-Mobilität die
/// > Fehlergrenze von 1 % bezogen auf die gemessene Zeitspanne zu erfüllen. Die
/// > kürzest mögliche Zeitspanne, für die diese Fehlergrenze erfüllt wird, darf
/// > nicht mehr als 60 Sekunden betragen … **Messwerte unterhalb der kürzest
/// > möglichen Zeitspanne werden nicht für Abrechnungszwecke verwendet.**
///
/// So a duration shorter than the device's own shortest measurable span is not
/// a number an invoice may use — the same shape as an unsynchronised clock
/// (`[OCMF Tab. 19]`), arriving from the other end: there the clock cannot be
/// *placed*, here the span cannot be *resolved*.
///
/// # Why the default is the regulation's cap rather than a device's figure
///
/// The manufacturer states the real figure in the instructions, and it may be
/// far below sixty seconds. A platform that does not have it has not been told
/// the device is better than the worst case the regulation permits, and
/// assuming otherwise is assuming a fact nobody stated — the same reasoning
/// that makes an unevaluable tariff restriction never match.
///
/// Written on the wire as a whole number of seconds — the unit
/// `[REA 6-A §3.1]` states the rule in — rather than `time::Duration`'s
/// `[seconds, nanoseconds]` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClockResolution(time::Duration);

#[cfg(feature = "serde")]
impl serde::Serialize for ClockResolution {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::wire::duration_seconds::serialize(&self.0, serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ClockResolution {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        // Through `stated`, so a resolution above the regulation's cap is
        // refused on the way in rather than carried as a figure nothing may use.
        Self::stated(crate::wire::duration_seconds::deserialize(deserializer)?)
            .map_err(D::Error::custom)
    }
}

impl ClockResolution {
    /// The longest shortest-span `[REA 6-A §3.1]` permits: sixty seconds.
    ///
    /// "Die kürzest mögliche Zeitspanne … darf nicht mehr als 60 Sekunden
    /// betragen." A device claiming worse is not one that may be used for
    /// billing in e-mobility at all, which is why [`Self::stated`] refuses it.
    pub const REGULATORY_CAP: time::Duration = time::Duration::seconds(60);

    /// What any conforming device guarantees, and nothing more.
    ///
    /// The default for a station whose type approval has not been read into the
    /// platform.
    #[must_use]
    pub const fn conforming() -> Self {
        Self(Self::REGULATORY_CAP)
    }

    /// The figure a device's documentation actually states.
    ///
    /// # Errors
    ///
    /// [`ClockResolutionError`] for a span above the regulatory cap or not
    /// above zero — neither describes a clock that may bill a duration in
    /// e-mobility.
    pub fn stated(shortest_span: time::Duration) -> Result<Self, ClockResolutionError> {
        if shortest_span <= time::Duration::ZERO {
            return Err(ClockResolutionError::NotPositive);
        }
        if shortest_span > Self::REGULATORY_CAP {
            return Err(ClockResolutionError::AboveCap {
                stated_seconds: shortest_span.whole_seconds(),
            });
        }
        Ok(Self(shortest_span))
    }

    /// The shortest span this clock may be billed for.
    #[must_use]
    pub const fn shortest_billable_span(self) -> time::Duration {
        self.0
    }

    /// Whether a measured span is long enough to bill.
    #[must_use]
    pub fn permits(self, span: time::Duration) -> bool {
        span >= self.0
    }
}

impl Default for ClockResolution {
    fn default() -> Self {
        Self::conforming()
    }
}

/// A clock resolution that does not describe a usable clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClockResolutionError {
    /// A span of zero or less.
    #[error("a clock's shortest measurable span must be positive")]
    NotPositive,

    /// Above the sixty seconds `[REA 6-A §3.1]` allows.
    #[error(
        "a clock whose shortest measurable span is {stated_seconds} s may not be used for billing in e-mobility: [REA 6-A §3.1] caps it at {} s",
        ClockResolution::REGULATORY_CAP.whole_seconds()
    )]
    AboveCap {
        /// What the device claimed.
        stated_seconds: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn a_quarter_hour_is_named_by_its_start_and_labelled_by_its_end() {
        // The convention hazard, as a test. `[PTB-A 50.7 §3.1.7.2]`: "der
        // Zeitstempel 00:15:00 [gehört] zur ersten Messperiode eines Tages".
        let first = QuarterHour::containing(datetime!(2026-01-02 00:07 UTC));
        assert_eq!(first.start(), datetime!(2026-01-02 00:00 UTC));
        assert_eq!(first.to_string(), "2026-01-02T00:00");
        assert_eq!(
            first.metering_timestamp(),
            datetime!(2026-01-02 00:15 UTC),
            "what a German meter and an MSCONS profile call this period"
        );
        assert_eq!(first.metering_timestamp(), first.end());
    }

    #[test]
    fn containing_truncates_towards_the_past() {
        for (minute, expected) in [(0, 0), (7, 0), (14, 0), (15, 15), (29, 15), (59, 45)] {
            let at = datetime!(2026-01-02 10:00 UTC) + time::Duration::minutes(minute);
            assert_eq!(
                QuarterHour::containing(at).start().minute(),
                expected,
                "minute {minute}"
            );
        }
    }

    #[test]
    fn a_boundary_is_recognised_to_the_nanosecond() {
        assert!(QuarterHour::is_boundary(datetime!(2026-01-02 10:15:00 UTC)));
        assert!(!QuarterHour::is_boundary(
            datetime!(2026-01-02 10:15:01 UTC)
        ));
        assert!(!QuarterHour::is_boundary(
            datetime!(2026-01-02 10:15:00 UTC) + time::Duration::nanoseconds(1)
        ));
        // Every civil UTC offset is a whole number of quarter hours, so a
        // boundary is a boundary in every timezone.
        assert!(QuarterHour::is_boundary(
            datetime!(2026-01-02 10:15:00 +5:45)
        ));
    }

    #[test]
    fn an_unaligned_instant_is_moved_forward_never_back() {
        // A price may not start applying earlier than it was published.
        assert_eq!(
            QuarterHour::boundary_at_or_after(datetime!(2026-01-02 10:07 UTC)),
            datetime!(2026-01-02 10:15 UTC)
        );
        assert_eq!(
            QuarterHour::boundary_at_or_after(datetime!(2026-01-02 10:15 UTC)),
            datetime!(2026-01-02 10:15 UTC),
            "an instant already on a boundary does not move"
        );
    }

    #[test]
    fn a_span_below_the_clock_s_resolution_is_not_billable() {
        // `[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen
        // Zeitspanne werden nicht für Abrechnungszwecke verwendet."
        let conforming = ClockResolution::conforming();
        assert_eq!(
            conforming.shortest_billable_span(),
            time::Duration::seconds(60)
        );
        assert!(!conforming.permits(time::Duration::seconds(59)));
        assert!(conforming.permits(time::Duration::seconds(60)));
        assert!(conforming.permits(time::Duration::minutes(30)));

        // A device that states a better figure gets it.
        let precise = ClockResolution::stated(time::Duration::seconds(10)).unwrap();
        assert!(precise.permits(time::Duration::seconds(30)));
        assert!(!precise.permits(time::Duration::seconds(9)));
    }

    #[test]
    fn a_clock_worse_than_the_cap_may_not_bill_at_all() {
        // The cap is the regulation's, so the type refuses a device that
        // claims worse rather than carrying a figure nothing may use.
        let err = ClockResolution::stated(time::Duration::seconds(61)).unwrap_err();
        assert!(matches!(err, ClockResolutionError::AboveCap { .. }));
        assert!(err.to_string().contains("caps it at 60"));

        assert!(matches!(
            ClockResolution::stated(time::Duration::ZERO),
            Err(ClockResolutionError::NotPositive)
        ));
        assert_eq!(ClockResolution::default(), ClockResolution::conforming());
    }

    #[test]
    fn periods_tile_without_arithmetic() {
        let q = QuarterHour::containing(datetime!(2026-01-02 10:07 UTC));
        assert_eq!(q.end(), q.next().start());
        assert_eq!(
            q.next().start() - q.start(),
            time::Duration::seconds(QuarterHour::SECONDS)
        );
    }
}
