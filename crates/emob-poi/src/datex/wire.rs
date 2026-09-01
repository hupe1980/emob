//! The four spellings the profile uses everywhere, in one place.
//!
//! DATEX II version 3 has a small number of structural conventions that recur
//! in every class, and getting one of them subtly wrong produces a document
//! that validates and means something else. They are written once here so the
//! publication modules can be about charging infrastructure.

use std::fmt;

use rust_decimal::Decimal;
use serde::{Serialize, Serializer};

/// An enumerated value: `{"value": "iec62196T2"}`.
///
/// Never a bare string. Every enumeration in the profile is a class with a
/// `value` attribute, so that a later revision can add attributes beside it
/// without breaking a reader — and a producer that writes the bare string
/// produces a document no consumer can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Enumerated {
    /// The literal.
    pub value: &'static str,
}

impl Enumerated {
    /// A literal from one of the profile's enumerations.
    #[must_use]
    pub const fn new(value: &'static str) -> Self {
        Self { value }
    }
}

/// An extended enumerated value: `{"value": "extendedG", "extendedValueG": …}`.
///
/// The profile's escape hatch for a value its enumeration does not contain. It
/// is how an EVSE id is published — `typeOfIdentifier` has no `evseId` literal,
/// so the identifier type is `extendedG` and the real answer sits beside it
/// `[DATEX-II-Profil]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extended {
    /// Always `extendedG`.
    pub value: &'static str,
    /// The value the enumeration does not contain.
    #[serde(rename = "extendedValueG")]
    pub extended_value: String,
}

impl Extended {
    /// An extension value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: "extendedG",
            extended_value: value.into(),
        }
    }
}

/// A string with a language tag: `{"values": [{"lang": "de", "value": …}]}`.
///
/// Every human-readable string in the profile is one of these, including the
/// ones that plainly are not translated. Publishing a bare string where the
/// profile expects a `MultilingualString` is the second commonest structural
/// mistake after the bare enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Multilingual {
    /// One entry per language.
    pub values: Vec<LocalisedText>,
}

impl Multilingual {
    /// One string, in one language.
    #[must_use]
    pub fn new(lang: &str, value: impl Into<String>) -> Self {
        Self {
            values: vec![LocalisedText {
                lang: lang.to_owned(),
                value: value.into(),
            }],
        }
    }
}

/// One language's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalisedText {
    /// The IETF language tag.
    pub lang: String,
    /// The text.
    pub value: String,
}

/// An exact decimal, written as a JSON **number**.
///
/// The reason this type exists rather than a plain `Decimal`: the profile
/// carries prices and coordinates as JSON numbers, and the ordinary route from
/// a decimal to a JSON number goes through `f64`. `0.35` survives that; `0.1 +
/// 0.2` does not, and neither does a coordinate's sixth decimal place at some
/// latitudes.
///
/// `serde_json` is built with `arbitrary_precision` across this workspace
/// precisely so a number can be carried as the digits somebody wrote. This
/// writes the decimal's own string representation into that slot, so the value
/// in the national access point feed is the value the rating engine charges
/// with, digit for digit.
///
/// Serializing to a format other than JSON falls back to the decimal's string
/// form, which is lossless but is a string. The profile is JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exact(pub Decimal);

impl Serialize for Exact {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The round trip through `Value` is what reaches `arbitrary_precision`'s
        // raw-number representation with only public API. It is not on a hot
        // path: a publication is built once per export.
        let literal = self.0.normalize().to_string();
        match serde_json::from_str::<serde_json::Value>(&literal) {
            Ok(number @ serde_json::Value::Number(_)) => number.serialize(serializer),
            _ => serializer.serialize_str(&literal),
        }
    }
}

impl From<Decimal> for Exact {
    fn from(value: Decimal) -> Self {
        Self(value)
    }
}

impl fmt::Display for Exact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Kilowatts as the whole watts the profile carries.
///
/// `maxPowerAtSocket`, `totalMaximumPower` and `availableChargingPower` are all
/// watts `[DATEX-II-Profil Tab. 7]`; a datasheet, a tariff and `[AFIR
/// Art. 5(4)]`'s threshold are all kilowatts. The multiplication is exact and
/// happens once, here, so no other module has to remember which unit it is in.
#[must_use]
pub fn watts(kw: Decimal) -> Exact {
    Exact(kw * Decimal::from(1000))
}

/// An instant, in the RFC 3339 spelling the profile's examples use.
///
/// A formatting failure is impossible — the description is a constant — so the
/// fallback is a Unix timestamp rather than a panic: a publication that names
/// the wrong instant is a bug somebody can see, and a publication job that
/// aborts at three in the morning is one nobody does.
#[must_use]
pub fn timestamp(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

/// A time of day, as `HH:MM:SSZ`.
#[must_use]
pub fn time_of_day(at: time::Time) -> String {
    format!("{:02}:{:02}:{:02}Z", at.hour(), at.minute(), at.second())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_price_reaches_the_feed_as_the_digits_somebody_wrote() {
        // The whole reason this type exists. Through `f64` this is
        // 0.35000000000000003 or 0.34999999999999998 depending on the route.
        let json = serde_json::to_string(&Exact(Decimal::from_str_exact("0.35").unwrap())).unwrap();
        assert_eq!(json, "0.35");
    }

    #[test]
    fn a_coordinate_keeps_its_sixth_decimal() {
        let json =
            serde_json::to_string(&Exact(Decimal::from_str_exact("50.779599").unwrap())).unwrap();
        assert_eq!(json, "50.779599");
    }

    #[test]
    fn kilowatts_become_whole_watts() {
        // 22 kW is 22000 W, and the profile wants the second number. A feed
        // that publishes 22 is publishing a 22-watt charger.
        let json = serde_json::to_string(&watts(Decimal::from_str_exact("22.5").unwrap())).unwrap();
        assert_eq!(json, "22500");
    }

    #[test]
    fn an_enumerated_value_is_an_object_and_never_a_bare_string() {
        let json = serde_json::to_string(&Enumerated::new("iec62196T2")).unwrap();
        assert_eq!(json, r#"{"value":"iec62196T2"}"#);
    }

    #[test]
    fn an_evse_id_travels_as_an_extension_because_the_enumeration_has_no_literal() {
        let json = serde_json::to_string(&Extended::new("evseId")).unwrap();
        assert_eq!(json, r#"{"value":"extendedG","extendedValueG":"evseId"}"#);
    }
}
