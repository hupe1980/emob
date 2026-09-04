//! A tariff's identity over time: which version was in force, and which one
//! priced a record.
//!
//! # The hole this closes
//!
//! A tariff id is a name, and names get reused. A CPO that edits a tariff in
//! place keeps the id, so a CDR saying "priced under `ad-hoc`" names something
//! that no longer exists — and a partner re-rating that record six weeks later
//! against the tariff it can fetch gets a different number and cannot tell an
//! honest price change from a restated one.
//!
//! That is the same failure the Eichrecht chain solved one layer down, and it
//! gets the same answer: **name it by content**. [`Tariff::fingerprint`] is a
//! SHA-256 over a canonical encoding of everything that can change a price, and
//! a CDR carries it beside the id. Two parties holding the same fingerprint are
//! holding the same tariff; two holding different ones know it immediately,
//! rather than arguing about a total.
//!
//! # And which version governs a session
//!
//! `[AFIR Art. 5(4)]` requires the ad-hoc price to be "known to end users
//! **before they initiate** a recharging session". That settles the question a
//! tariff change mid-session otherwise leaves open: the governing tariff is the
//! one in force when the session started, because that is the one the driver
//! was shown. [`TariffHistory::in_force_at`] is that rule, and
//! [`crate::rating::rate`]'s element restrictions still handle variation
//! *within* a version.
//!
//! Validity windows are **half-open**, `[from, until)`, for the reason the key
//! registry's are: with two inclusive bounds a tariff replaced at midnight has
//! two versions covering that instant, and the answer depends on insertion
//! order.
//!
//! # …and a price may only move on the settlement grid
//!
//! `[PTB-A 50.7 §3.1.7.2]`: "Ein Tarifwechsel ist erst mit dem Beginn der
//! nächsten Messperiode durchzuführen." A change that lands mid-quarter-hour
//! puts one settlement slot under two tariffs — a slot nobody can allocate
//! `[A6 §IV.1]` and a price nobody can reproduce — so [`TariffHistory::new`]
//! refuses one rather than letting it surface in a settlement file. The error
//! names the next boundary, and it is the *next* one rather than the nearest,
//! because a price may not start applying earlier than it was published.

use core::fmt;

use emob_core::{QuarterHour, TariffId};
use sha2::{Digest, Sha256};

use crate::tariff::{Restrictions, Tariff, TariffElement};

/// A content address for a tariff: SHA-256 over everything that can change a
/// price.
///
/// Rendered and parsed as lower-case hex, because it travels on a record
/// between two companies and has to survive a spreadsheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TariffFingerprint([u8; 32]);

impl TariffFingerprint {
    /// The raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first eight hex characters — enough to name a version in a log line
    /// or a support ticket, and never enough to compare two by.
    #[must_use]
    pub fn short(&self) -> String {
        self.to_string()[..8].to_owned()
    }
}

impl fmt::Display for TariffFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for TariffFingerprint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for TariffFingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = <String as serde::Deserialize>::deserialize(deserializer)?;
        if hex.len() != 64 {
            return Err(serde::de::Error::custom(
                "a tariff fingerprint is 64 hex characters",
            ));
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| serde::de::Error::custom("a tariff fingerprint is hex"))?;
        }
        Ok(Self(bytes))
    }
}

/// A canonical writer: every value goes in with an explicit format and a
/// terminator, so no two different tariffs can encode to the same bytes.
///
/// Nothing here uses a `Display` impl from another crate, and nothing here uses
/// a **derived `Debug`** either. A fingerprint that changed because `time`
/// reformatted a date would silently split one tariff into two across a
/// dependency bump; one that changed because a variant was renamed would do the
/// same across a refactor of this crate, which is the easier mistake to make and
/// the harder one to notice. Every enum that goes in has a declared token —
/// [`Dimension::as_str`](crate::Dimension::as_str) and its siblings — and the
/// whole point of the value is that it does not move unless the tariff does.
struct Canonical(Vec<u8>);

impl Canonical {
    fn new() -> Self {
        // A version tag, so a future change to this encoding is a visible
        // change of fingerprint rather than an invisible one.
        Self(b"emob-tariff-fingerprint/1\n".to_vec())
    }

    fn field(&mut self, value: &str) -> &mut Self {
        // Length-prefixed: `ab` then `c` cannot encode the same as `a` then
        // `bc`, whatever the values contain.
        self.0.extend_from_slice(value.len().to_string().as_bytes());
        self.0.push(b':');
        self.0.extend_from_slice(value.as_bytes());
        self.0.push(b'\n');
        self
    }

    fn optional(&mut self, value: Option<String>) -> &mut Self {
        match value {
            Some(v) => self.field("+").field(&v),
            None => self.field("-"),
        }
    }

    fn count(&mut self, n: usize) -> &mut Self {
        self.field(&n.to_string())
    }

    fn finish(self) -> TariffFingerprint {
        TariffFingerprint(Sha256::digest(&self.0).into())
    }
}

/// A date as `YYYY-MM-DD`, formatted here rather than by `time`.
fn date(d: time::Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// A time of day as `HH:MM:SS`.
fn clock(t: time::Time) -> String {
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

/// An instant as its Unix timestamp plus its offset, so two spellings of one
/// moment encode identically and two different moments never do.
fn instant(at: time::OffsetDateTime) -> String {
    format!("{}@{}", at.unix_timestamp(), at.offset().whole_seconds())
}

impl Tariff {
    /// A content address for this tariff.
    ///
    /// Covers every field that can change a price or a display: the id, the
    /// currency, the kind, **the zone the wall clock is read in**, the tax
    /// basis, the bounds, the validity window, and every element with its
    /// restrictions and components **in order**, because element order decides
    /// which one applies.
    ///
    /// Scale is part of it. `0.49` and `0.490` are numerically equal and are
    /// two different prices to show a driver, so they fingerprint differently —
    /// the same reasoning that keeps a meter's trailing zeros through the
    /// evidence chain.
    #[must_use]
    pub fn fingerprint(&self) -> TariffFingerprint {
        let mut c = Canonical::new();
        c.field(self.id.as_str())
            .field(self.currency.as_str())
            .field(self.kind.as_str())
            // The zone the wall-clock restrictions are read in. Two tariffs
            // with identical `22:00` elements in `Europe/Berlin` and
            // `Europe/Lisbon` price the same session differently, so they are
            // two tariffs and a record has to be able to say which one priced
            // it.
            .field(self.time_zone.name())
            .field(self.tax_included.as_str())
            .optional(self.min_price.map(|p| p.to_string()))
            .optional(self.max_price.map(|p| p.to_string()))
            .optional(self.valid_from.map(instant))
            .optional(self.valid_until.map(instant))
            .count(self.elements.len());
        for element in &self.elements {
            fingerprint_element(&mut c, element);
        }
        c.finish()
    }
}

fn fingerprint_element(c: &mut Canonical, element: &TariffElement) {
    fingerprint_restrictions(c, &element.restrictions);
    c.count(element.components.len());
    for component in &element.components {
        c.field(component.dimension.as_str())
            .field(&component.price.to_string())
            .optional(component.vat.map(|v| v.to_string()))
            .field(&component.step_size.to_string());
    }
}

fn fingerprint_restrictions(c: &mut Canonical, r: &Restrictions) {
    // Canonical, because a set of weekdays is a set: `[Mon, Tue]` and
    // `[Tue, Mon]` restrict identically and price identically, and a
    // fingerprint that moved between them would report one tariff as two —
    // which is the failure this value exists to prevent, pointed the other way.
    let mut days: Vec<u8> = r
        .days_of_week
        .iter()
        .map(|day| day.number_days_from_monday())
        .collect();
    days.sort_unstable();
    days.dedup();

    c.optional(r.start_time.map(clock))
        .optional(r.end_time.map(clock))
        .optional(r.start_date.map(date))
        .optional(r.end_date.map(date))
        .optional(r.min_kwh.map(|v| v.to_string()))
        .optional(r.max_kwh.map(|v| v.to_string()))
        .optional(r.min_power_kw.map(|v| v.to_string()))
        .optional(r.max_power_kw.map(|v| v.to_string()))
        .optional(r.min_duration_s.map(|v| v.to_string()))
        .optional(r.max_duration_s.map(|v| v.to_string()))
        // A **declared** token, as every field here is: an element that prices
        // a reservation and one that prices a session are two elements, and a
        // tariff that gains or loses the restriction is a different tariff.
        .optional(r.reservation.map(|kind| kind.as_str().to_owned()))
        .count(days.len());
    for day in &days {
        c.field(&day.to_string());
    }
    // An unevaluable restriction is part of the tariff's identity even though
    // this build cannot judge it: two tariffs differing only in one are two
    // tariffs, and the one carrying it prices nothing under that element.
    c.count(r.unevaluable.len());
    for unknown in &r.unevaluable {
        c.field(unknown);
    }
}

/// Every version of one tariff, in force over disjoint windows.
///
/// Built rather than assembled: [`TariffHistory::new`] refuses a set whose
/// windows overlap, because an instant covered by two versions is an instant
/// with two prices and no rule for choosing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TariffHistory {
    id: TariffId,
    versions: Vec<Tariff>,
}

impl TariffHistory {
    /// Build a history from a tariff's versions.
    ///
    /// # Errors
    ///
    /// [`TariffHistoryError`] when there are no versions, when they do not all
    /// carry the same id, or when two windows overlap.
    pub fn new(mut versions: Vec<Tariff>) -> Result<Self, TariffHistoryError> {
        let Some(first) = versions.first() else {
            return Err(TariffHistoryError::Empty);
        };
        let id = first.id.clone();
        if let Some(other) = versions.iter().find(|t| t.id != id) {
            return Err(TariffHistoryError::MixedIds {
                expected: id.to_string(),
                found: other.id.to_string(),
            });
        }

        // A version whose window is inside out covers no instant at all —
        // `covers` is `from <= at < until` — so it can never price anything,
        // and a history holding one has a silent hole where somebody believes
        // there is a price. The overlap sweep below cannot see it: it sorts by
        // `valid_from` and compares neighbours, and an empty window overlaps
        // nothing by construction.
        for version in &versions {
            if let (Some(from), Some(until)) = (version.valid_from, version.valid_until)
                && until <= from
            {
                return Err(TariffHistoryError::EmptyWindow {
                    version: version.fingerprint().short(),
                    valid_from: from,
                    valid_until: until,
                });
            }
        }

        // A price may only start applying at a settlement-period boundary
        // `[PTB-A 50.7 §3.1.7.2]`: "Ein Tarifwechsel ist erst mit dem Beginn
        // der nächsten Messperiode durchzuführen." A change that lands
        // mid-period puts one quarter hour under two tariffs — a slot nobody
        // can allocate `[A6 §IV.1]` and a price nobody can reproduce — so it
        // is refused here rather than discovered in a settlement file.
        for version in &versions {
            for instant in [version.valid_from, version.valid_until] {
                if let Some(at) = instant
                    && !QuarterHour::is_boundary(at)
                {
                    return Err(TariffHistoryError::UnalignedChange {
                        at,
                        nearest: QuarterHour::boundary_at_or_after(at),
                    });
                }
            }
        }

        // `None` is negative infinity, so it sorts first.
        versions.sort_by_key(|t| t.valid_from);

        for pair in versions.windows(2) {
            let (earlier, later) = (&pair[0], &pair[1]);
            // `None` is an unbounded end: an earlier version that never closes
            // runs into whatever follows, and a later one that never opens runs
            // back into whatever precedes. Either way the two share an instant.
            let overlaps = match (earlier.valid_until, later.valid_from) {
                (Some(until), Some(from)) => until > from,
                _ => true,
            };
            if overlaps {
                return Err(TariffHistoryError::Overlap {
                    earlier: earlier.fingerprint().short(),
                    later: later.fingerprint().short(),
                });
            }
        }

        Ok(Self { id, versions })
    }

    /// The tariff, when there is only ever one version of it.
    ///
    /// # Errors
    ///
    /// [`TariffHistoryError`] never, for a single version — the signature is
    /// uniform so a caller can move between the two shapes without changing
    /// its error handling.
    pub fn single(tariff: Tariff) -> Result<Self, TariffHistoryError> {
        Self::new(vec![tariff])
    }

    /// Which tariff this is the history of.
    #[must_use]
    pub const fn id(&self) -> &TariffId {
        &self.id
    }

    /// Every version, earliest first.
    #[must_use]
    pub fn versions(&self) -> &[Tariff] {
        &self.versions
    }

    /// The version in force at an instant, if any.
    ///
    /// The rule `[AFIR Art. 5(4)]` implies for a session: the price has to be
    /// known "before they initiate a recharging session", so the version that
    /// governs is the one in force when the session started.
    #[must_use]
    pub fn in_force_at(&self, at: time::OffsetDateTime) -> Option<&Tariff> {
        self.versions.iter().find(|t| t.covers(at))
    }

    /// The windows no version covers.
    ///
    /// A gap is not an error — a tariff may genuinely have been withdrawn and
    /// reinstated — but it is an interval in which nothing can be priced, and
    /// finding that out when a session lands in one is finding out late.
    #[must_use]
    pub fn gaps(&self) -> Vec<(time::OffsetDateTime, time::OffsetDateTime)> {
        self.versions
            .windows(2)
            .filter_map(|pair| match (pair[0].valid_until, pair[1].valid_from) {
                (Some(until), Some(from)) if until < from => Some((until, from)),
                _ => None,
            })
            .collect()
    }
}

/// What can be wrong with a set of tariff versions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TariffHistoryError {
    /// No versions at all.
    #[error("a tariff history needs at least one version")]
    Empty,

    /// The versions are not all of one tariff.
    #[error("a tariff history holds one tariff: expected {expected}, found {found}")]
    MixedIds {
        /// The id the first version carried.
        expected: String,
        /// The id that turned up.
        found: String,
    },

    /// A version's window ends at or before it begins, so it covers nothing.
    #[error(
        "tariff version {version} is in force from {valid_from} until {valid_until}, which is an empty window: it can never price a session, and a history holding it has a hole where somebody believes there is a price"
    )]
    EmptyWindow {
        /// The version, by short fingerprint.
        version: String,
        /// When it claims to start.
        valid_from: time::OffsetDateTime,
        /// When it claims to stop.
        valid_until: time::OffsetDateTime,
    },

    /// A version begins or ends off the settlement grid.
    ///
    /// `[PTB-A 50.7 §3.1.7.2]`: "Ein Tarifwechsel ist erst mit dem Beginn der
    /// nächsten Messperiode durchzuführen."
    #[error(
        "a tariff version changes at {at}, which is not a settlement-period boundary [PTB-A 50.7 §3.1.7.2]: one quarter hour would fall under two tariffs, which is a slot nobody can allocate and a price nobody can reproduce. The next boundary is {nearest}"
    )]
    UnalignedChange {
        /// The instant the version starts or stops at.
        at: time::OffsetDateTime,
        /// The first boundary at or after it — forward, never back, because a
        /// price may not start applying earlier than it was published.
        nearest: time::OffsetDateTime,
    },

    /// Two versions cover the same instant.
    #[error(
        "tariff versions {earlier} and {later} cover the same instant: an instant with two prices has no rule for choosing between them"
    )]
    Overlap {
        /// The earlier version, by short fingerprint.
        earlier: String,
        /// The later one.
        later: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tariff::PriceLimit;
    use crate::tariff::{Dimension, PriceComponent, TariffKind};
    use emob_core::Currency;
    use emob_core::TimeZone;
    use rust_decimal::Decimal;
    use rust_decimal::prelude::FromStr;
    use time::macros::datetime;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn tariff(price: &str) -> Tariff {
        Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec(price))],
        )
    }

    fn window(
        price: &str,
        from: Option<time::OffsetDateTime>,
        until: Option<time::OffsetDateTime>,
    ) -> Tariff {
        let mut t = tariff(price);
        t.valid_from = from;
        t.valid_until = until;
        t
    }

    #[test]
    fn one_tariff_fingerprints_the_same_way_twice() {
        assert_eq!(tariff("0.49").fingerprint(), tariff("0.49").fingerprint());
        assert_eq!(
            tariff("0.49").fingerprint().to_string().len(),
            64,
            "64 hex characters, so it survives a spreadsheet"
        );
    }

    #[test]
    fn any_change_that_can_change_a_price_changes_the_fingerprint() {
        let base = tariff("0.49");
        let mut cases = Vec::new();

        cases.push(tariff("0.59"));

        let mut currency = base.clone();
        currency.currency = Currency::new("CHF").unwrap();
        cases.push(currency);

        let mut kind = base.clone();
        kind.kind = TariffKind::Contract;
        cases.push(kind);

        let mut tax = base.clone();
        tax.tax_included = crate::tariff::TaxIncluded::No;
        cases.push(tax);

        let mut minimum = base.clone();
        minimum.min_price = Some(PriceLimit::gross(dec("5.00")));
        cases.push(minimum);

        let mut vat = base.clone();
        vat.elements[0].components[0].vat = Some(dec("19"));
        cases.push(vat);

        let mut step = base.clone();
        step.elements[0].components[0].step_size = 1000;
        cases.push(step);

        let mut restricted = base.clone();
        restricted.elements[0].restrictions.max_kwh = Some(dec("10"));
        cases.push(restricted);

        let mut window = base.clone();
        window.valid_from = Some(datetime!(2026-01-01 0:00 UTC));
        cases.push(window);

        let mut unevaluable = base.clone();
        unevaluable.elements[0]
            .restrictions
            .unevaluable
            .push("min_current=5".to_owned());
        cases.push(unevaluable);

        // An element that prices a reservation is not the element that prices
        // the session, and the two outcomes are not each other either.
        for kind in [
            crate::tariff::ReservationRestriction::Reservation,
            crate::tariff::ReservationRestriction::ReservationExpires,
        ] {
            let mut reserving = base.clone();
            reserving.elements[0].restrictions.reservation = Some(kind);
            cases.push(reserving);
        }

        for changed in cases {
            assert_ne!(
                base.fingerprint(),
                changed.fingerprint(),
                "a change nothing notices is a tariff edited in place: {changed:?}"
            );
        }
    }

    #[test]
    fn scale_is_part_of_the_price_and_therefore_of_the_identity() {
        // `0.49` and `0.490` are numerically equal and are two different
        // prices to show a driver — the same reasoning that keeps a meter's
        // trailing zeros through the evidence chain.
        assert_eq!(dec("0.49"), dec("0.490"));
        assert_ne!(tariff("0.49").fingerprint(), tariff("0.490").fingerprint());
    }

    #[test]
    fn element_order_is_part_of_the_identity() {
        // The first matching element prices the period, so two tariffs with
        // the same elements in a different order are two different tariffs.
        let night = crate::tariff::TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
            restrictions: Restrictions {
                start_time: Some(time::macros::time!(22:00)),
                ..Restrictions::default()
            },
        };
        let day = crate::tariff::TariffElement::unrestricted(vec![PriceComponent::new(
            Dimension::Energy,
            dec("0.49"),
        )]);

        let mut a = tariff("0.49");
        a.elements = vec![night.clone(), day.clone()];
        let mut b = tariff("0.49");
        b.elements = vec![day, night];

        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn the_field_encoding_cannot_be_confused_by_a_shifted_boundary() {
        // Length-prefixing is what stops `ab` + `c` encoding the same as
        // `a` + `bc`, which for ids and currencies is a real collision.
        let mut a = tariff("0.49");
        a.id = "ad".parse().unwrap();
        let mut b = tariff("0.49");
        b.id = "a".parse().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn a_history_picks_the_version_in_force() {
        let history = TariffHistory::new(vec![
            window("0.49", None, Some(datetime!(2026-06-01 0:00 UTC))),
            window("0.59", Some(datetime!(2026-06-01 0:00 UTC)), None),
        ])
        .unwrap();

        assert_eq!(
            history
                .in_force_at(datetime!(2026-03-01 12:00 UTC))
                .unwrap()
                .elements[0]
                .components[0]
                .price,
            dec("0.49")
        );
        // Half-open: the swap instant belongs to exactly one version, so the
        // answer does not depend on insertion order.
        assert_eq!(
            history
                .in_force_at(datetime!(2026-06-01 0:00 UTC))
                .unwrap()
                .elements[0]
                .components[0]
                .price,
            dec("0.59")
        );
        assert_eq!(history.id().as_str(), "ad-hoc");
        assert_eq!(history.versions().len(), 2);
    }

    #[test]
    fn overlapping_versions_are_refused_rather_than_ordered() {
        // An instant covered by two versions is an instant with two prices and
        // no rule for choosing between them.
        let err = TariffHistory::new(vec![
            window("0.49", None, Some(datetime!(2026-07-01 0:00 UTC))),
            window("0.59", Some(datetime!(2026-06-01 0:00 UTC)), None),
        ])
        .unwrap_err();
        assert!(matches!(err, TariffHistoryError::Overlap { .. }));
        assert!(err.to_string().contains("no rule for choosing"));

        // Two open-ended versions are the same fault stated differently.
        assert!(matches!(
            TariffHistory::new(vec![tariff("0.49"), tariff("0.59")]),
            Err(TariffHistoryError::Overlap { .. })
        ));
    }

    #[test]
    fn a_version_whose_window_is_inside_out_is_refused() {
        // `covers` is `from <= at < until`, so a window that ends before it
        // begins covers no instant at all — and the overlap sweep cannot see
        // it, because an empty window overlaps nothing by construction. Left
        // alone it is a version somebody believes prices sessions and that
        // never prices one, which surfaces as `NoTariffInForce` on a CDR weeks
        // later.
        let err = TariffHistory::new(vec![window(
            "0.49",
            Some(datetime!(2026-07-01 0:00 UTC)),
            Some(datetime!(2026-06-01 0:00 UTC)),
        )])
        .unwrap_err();
        assert!(matches!(err, TariffHistoryError::EmptyWindow { .. }));
        assert!(err.to_string().contains("can never price a session"));

        // A window of zero length is the same fault: `[from, from)` is empty.
        assert!(matches!(
            TariffHistory::new(vec![window(
                "0.49",
                Some(datetime!(2026-06-01 0:00 UTC)),
                Some(datetime!(2026-06-01 0:00 UTC)),
            )]),
            Err(TariffHistoryError::EmptyWindow { .. })
        ));
    }

    #[test]
    fn a_price_may_only_move_on_the_settlement_grid() {
        // `[PTB-A 50.7 §3.1.7.2]`: "Ein Tarifwechsel ist erst mit dem Beginn
        // der nächsten Messperiode durchzuführen." A change at 10:07 would put
        // the quarter hour beginning 10:00 under two tariffs — a slot nobody
        // can allocate `[A6 §IV.1]` and a price nobody can reproduce.
        let err = TariffHistory::new(vec![
            window("0.49", None, Some(datetime!(2026-06-01 10:07 UTC))),
            window("0.59", Some(datetime!(2026-06-01 10:07 UTC)), None),
        ])
        .unwrap_err();

        assert!(
            matches!(
                err,
                TariffHistoryError::UnalignedChange {
                    nearest,
                    ..
                } if nearest == datetime!(2026-06-01 10:15 UTC)
            ),
            "{err:?}"
        );
        // Forward, never back: a price may not start applying earlier than it
        // was published.
        assert!(err.to_string().contains("10:15"));

        // …and on the grid it builds.
        assert!(
            TariffHistory::new(vec![
                window("0.49", None, Some(datetime!(2026-06-01 10:15 UTC))),
                window("0.59", Some(datetime!(2026-06-01 10:15 UTC)), None),
            ])
            .is_ok()
        );
    }

    #[test]
    fn an_unaligned_end_is_refused_as_well_as_an_unaligned_start() {
        // Both edges of a window are a tariff change: one version stops where
        // the next begins, and a stop off the grid is the same broken slot.
        assert!(matches!(
            TariffHistory::single(window("0.49", None, Some(datetime!(2026-06-01 10:07 UTC)))),
            Err(TariffHistoryError::UnalignedChange { .. })
        ));
        assert!(matches!(
            TariffHistory::single(window("0.49", Some(datetime!(2026-06-01 10:07 UTC)), None)),
            Err(TariffHistoryError::UnalignedChange { .. })
        ));
    }

    #[test]
    fn a_history_holds_one_tariff() {
        let mut other = tariff("0.59");
        other.id = "contract".parse().unwrap();
        assert!(matches!(
            TariffHistory::new(vec![tariff("0.49"), other]),
            Err(TariffHistoryError::MixedIds { .. })
        ));
        assert!(matches!(
            TariffHistory::new(vec![]),
            Err(TariffHistoryError::Empty)
        ));
    }

    #[test]
    fn a_gap_is_reported_rather_than_papered_over() {
        // Lawful — a tariff can be withdrawn and reinstated — and an interval
        // in which nothing can be priced. Finding that out when a session
        // lands in one is finding out late.
        let history = TariffHistory::new(vec![
            window("0.49", None, Some(datetime!(2026-06-01 0:00 UTC))),
            window("0.59", Some(datetime!(2026-07-01 0:00 UTC)), None),
        ])
        .unwrap();

        assert_eq!(
            history.gaps(),
            vec![(
                datetime!(2026-06-01 0:00 UTC),
                datetime!(2026-07-01 0:00 UTC)
            )]
        );
        assert!(
            history
                .in_force_at(datetime!(2026-06-15 0:00 UTC))
                .is_none()
        );
        assert!(
            TariffHistory::single(tariff("0.49"))
                .unwrap()
                .gaps()
                .is_empty()
        );
    }

    #[test]
    fn a_tariff_with_no_window_covers_every_instant() {
        let t = tariff("0.49");
        assert!(t.covers(datetime!(2020-01-01 0:00 UTC)));
        assert!(t.covers(datetime!(2030-01-01 0:00 UTC)));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_fingerprint_survives_the_wire_as_hex() {
        let fingerprint = tariff("0.49").fingerprint();
        let json = serde_json::to_string(&fingerprint).unwrap();
        assert_eq!(json.len(), 66, "64 hex characters in quotes");
        assert_eq!(
            serde_json::from_str::<TariffFingerprint>(&json).unwrap(),
            fingerprint
        );
        assert!(serde_json::from_str::<TariffFingerprint>("\"beef\"").is_err());
    }
}
