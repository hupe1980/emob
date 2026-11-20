//! emob's exact values in the scalar types `datex2` writes.
//!
//! `datex2` owns the profile's spelling — the `aegi`/`afac`/`loc` prefixes,
//! the `G` suffixes, `{"value": …}` around every enumeration, `extendedG` for a
//! literal the enumeration lacks — and refuses to write what the Mobilithek's
//! schemas would reject. What is left here is the crossing from this
//! workspace's types into the model's: an amount of money stays a decimal with
//! its own digits, and the profile's `Float` members are filled from decimal
//! text so a float exists only where the profile puts one.

use std::str::FromStr;

use datex2::v3::afir_facilities::AmountOfMoney;
use datex2::{Date, DateTime, MultilingualString, Time};
use rust_decimal::Decimal;

/// An amount of money, with exactly the digits the tariff states.
///
/// `AmountOfMoney` holds decimal text rather than a binary float, so `0.59`
/// leaves as `0.59`. The profile allows two places after the point; an amount
/// with more is refused by [`crate::feed::Feed::check`] before it gets here,
/// and by `datex2`'s writer if it is built outside a feed.
pub(crate) fn money(amount: Decimal) -> Option<AmountOfMoney> {
    amount.normalize().to_string().parse().ok()
}

/// Whether the profile can state this amount: two places after the point at
/// most `[DATEX-II-Profil]` (`AmountOfMoney`, "a monetary value expressed to
/// two decimal places").
#[must_use]
pub fn in_cents(amount: Decimal) -> bool {
    amount.normalize().scale() <= 2
}

/// A member the profile types as `Float`, from a decimal's own text.
///
/// Power, coordinates and the tax rate. Parsing the text picks the nearest
/// value the profile's type holds, and `datex2` writes that back as the
/// shortest text that reads as it again — `22000` stays `22000`. A coordinate
/// keeps about seven significant digits, which is the profile's precision and
/// well under a metre.
pub(crate) fn float<T: FromStr>(value: Decimal) -> Option<T> {
    value.normalize().to_string().parse().ok()
}

/// Kilowatts as the watts the profile carries.
///
/// `maxPowerAtSocket`, `totalMaximumPower` and `availableChargingPower` are all
/// watts `[DATEX-II-Profil Tab. 7]`; a datasheet, a tariff and `[AFIR
/// Art. 5(4)]`'s threshold are all kilowatts. A 150 kW charger published as
/// `150` is a 150-watt charger.
pub(crate) fn watts<T: FromStr>(kw: Decimal) -> Option<T> {
    float(kw * Decimal::from(1000))
}

/// An instant, with its offset.
pub(crate) fn date_time(at: time::OffsetDateTime) -> DateTime {
    DateTime {
        date: Date {
            year: at.year(),
            month: u8::from(at.month()),
            day: at.day(),
        },
        time: time_of_day(at.time()),
        offset_minutes: Some(at.offset().whole_minutes()),
    }
}

/// A time of day, to the second.
pub(crate) const fn time_of_day(at: time::Time) -> Time {
    Time {
        hour: at.hour(),
        minute: at.minute(),
        second: at.second(),
        nanosecond: 0,
        fraction_digits: 0,
    }
}

/// One string, in one language.
pub(crate) fn text(lang: &str, value: impl Into<String>) -> MultilingualString {
    MultilingualString::new(lang, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).unwrap()
    }

    #[test]
    fn an_amount_keeps_the_digits_somebody_wrote() {
        // Through `f64` this is 0.35000000000000003 or 0.34999999999999998
        // depending on the route.
        assert_eq!(money(dec("0.35")).unwrap().as_str(), "0.35");
        assert_eq!(money(dec("0.350")).unwrap().as_str(), "0.35");
    }

    #[test]
    fn a_tenth_of_a_cent_is_not_an_amount_the_profile_can_state() {
        assert!(in_cents(dec("0.59")));
        assert!(in_cents(dec("0.5900")));
        assert!(!in_cents(dec("0.589")));
        assert!(!in_cents(dec("0.0416666666666666666666666667")));
    }

    use datex2::v3::common::Float;

    #[test]
    fn kilowatts_become_watts() {
        assert_eq!(watts::<Float>(dec("22.5")).unwrap().to_string(), "22500");
    }

    #[test]
    fn a_coordinate_keeps_the_precision_the_profile_holds() {
        // `Float` keeps about seven significant digits: 50.779599 is written
        // 50.7796, 0.1 m away. 6.104507 survives whole.
        assert_eq!(
            float::<Float>(dec("50.779599")).unwrap().to_string(),
            "50.7796"
        );
        assert_eq!(
            float::<Float>(dec("6.104507")).unwrap().to_string(),
            "6.104507"
        );
    }

    #[test]
    fn an_instant_keeps_its_offset() {
        let at = date_time(time::macros::datetime!(2026-04-14 14:00 +02:00));
        assert_eq!(at.to_string(), "2026-04-14T14:00:00+02:00");
    }
}
