//! Reading a limit write off the wire, without going through a float.
//!
//! # `ScaledNumber` is a decimal, and it arrives as one
//!
//! SPINE carries every quantity as `{ number, scale }` — an integer mantissa
//! and a power of ten. That is exactly a decimal, and it is exactly the type
//! this workspace uses for everything that becomes money or power. The
//! conversion is a shift, and it is lossless in both directions inside the
//! range a `Decimal` holds.
//!
//! What is not lossless is the trip through `f64` that a use-case surface
//! typed in watts has to make. `[BK6-22-300 Ziff. 4.5]`'s Mindestleistung is
//! 4.2 kW; three-phase at 6 A is 4.14 kW; the difference is sixty watts, and a
//! platform that cannot tell the two apart cannot say whether it honoured an
//! Anspruch. `[EEG §9]`'s curtailment arithmetic is the same shape one statute
//! along — 70 % of a 9.9 kW array is `6930.000000000001` in binary floating
//! point, and it is a figure a feed-in payment is computed against.
//!
//! # Partial writes, and the element an omission means *unchanged*
//!
//! SPINE's implementation guide §3.3 makes an omitted element in a partial
//! write mean *unchanged* rather than absent, and `eebus` hands a caller both
//! payloads for that reason: `data` says which entries the write addresses and
//! `resolved` says what they become. This module reads **`resolved`**, and the
//! distinction is not academic — a limit update that adjusts only `value`
//! leaves `isLimitActive` out, and reading that as `false` turns a curtailment
//! into a release back to full power.

use eebus::model::{CmdData, LoadControlLimitId, ScaledNumber};
use rust_decimal::Decimal;

use crate::EebusError;

/// A limit a grid operator wrote, in this workspace's own arithmetic.
///
/// The same three facts `eebus::usecases::limitation::LimitWrite` carries, with
/// the value as an exact decimal instead of a `f64`. See [`crate`] for the line
/// this crate draws and why it falls there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LimitWrite {
    /// Whether the limit is in force `[LPC-008]`.
    ///
    /// A write that is not active is a **release**, and the implementation
    /// guide §2.13 wants one sent rather than inferred from silence.
    pub is_active: bool,
    /// The limit, in watts. Always at or above zero `[LPC-001]`.
    pub watts: Decimal,
    /// How long it is valid `[LPC-004]`.
    ///
    /// The duration starts when the write arrives, whether or not the limit is
    /// active, and the limit is deactivated when it reaches zero `[LPC-007]`.
    /// `None` is *"until something replaces it"*, which is a different fact
    /// from a duration of zero.
    #[cfg_attr(
        feature = "serde",
        serde(with = "emob_core::wire::duration_seconds::option")
    )]
    pub valid_for: Option<time::Duration>,
}

/// The value of a `ScaledNumber`, exactly.
///
/// `number × 10^scale`, built by shifting a decimal rather than by multiplying
/// a float. An absent `scale` reads as zero, which is what the Resource
/// Specification says and what peers written against it send — though the
/// implementation guide §3.2.1 has since made it mandatory on the wire, by an
/// erratum that exists because *"severe misinterpretations"* happened in the
/// field.
///
/// [`None`] where there is no `number` at all, or where the scale would move
/// the value outside what a `Decimal` holds. Refusing is the only honest
/// answer to the second: an unrepresentable value is not a value, and
/// substituting one the peer never sent would apply a limit nobody asked for.
///
/// ```
/// # use emob_eebus::watts_of;
/// # use eebus::model::ScaledNumber;
/// # use rust_decimal::Decimal;
/// assert_eq!(watts_of(&ScaledNumber::new(42, 2)), Some(Decimal::from(4200)));
/// // …and a negative scale is a fraction, which is the case a float loses.
/// assert_eq!(
///     watts_of(&ScaledNumber::new(4_140, 0)),
///     Some(Decimal::from(4_140)),
/// );
/// assert_eq!(
///     watts_of(&ScaledNumber::new(414, 1)),
///     Some(Decimal::from(4_140)),
/// );
/// ```
#[must_use]
pub fn watts_of(value: &ScaledNumber) -> Option<Decimal> {
    let number = value.number?.get();
    let scale = value.scale.map_or(0, eebus::model::Scale::get);
    shift(Decimal::from(number), scale)
}

/// `mantissa × 10^scale`, by moving the decimal point rather than multiplying.
///
/// `Decimal` panics on overflow rather than saturating, and a `scale` is a
/// signed 16-bit number a peer chooses — so `i64::MAX` at scale 300 is one
/// well-formed message away. Every step is checked, and a value that runs out
/// of room is [`None`] rather than a process this workspace no longer has.
fn shift(mantissa: Decimal, scale: i16) -> Option<Decimal> {
    const TEN: Decimal = Decimal::from_parts(10, 0, 0, false, 0);
    let mut value = mantissa;
    for _ in 0..scale.unsigned_abs() {
        value = if scale.is_positive() {
            value.checked_mul(TEN)?
        } else {
            value.checked_div(TEN)?
        };
    }
    Some(value)
}

/// Read a limit write out of the payload a peer wrote.
///
/// `resolved` is `eebus::spine::WriteRequest::resolved` — what the addressed
/// function *becomes* if the write is accepted, which is the payload to decide
/// values from. See the module documentation for why `data` is not.
///
/// # Errors
///
/// [`EebusError::NotALimitWrite`] where the payload is not a
/// `LoadControlLimitListData` or carries no entry for `limit_id`;
/// [`EebusError::UnreadableLimit`] where it carries one whose value no decimal
/// can hold; and [`EebusError::NegativeLimit`] for one below zero, which
/// `[LPC-001]` forbids and which would otherwise reach an allocation as a
/// ceiling below the floor every point has.
pub fn read_limit_write(
    resolved: &CmdData,
    limit_id: LoadControlLimitId,
) -> Result<LimitWrite, EebusError> {
    let CmdData::LoadControlLimitListData(list) = resolved else {
        return Err(EebusError::NotALimitWrite {
            detail: "the payload is not a LoadControlLimitListData".to_owned(),
        });
    };
    let entry = list
        .load_control_limit_data
        .as_ref()
        .and_then(|entries| entries.iter().find(|e| e.limit_id == Some(limit_id)))
        .ok_or_else(|| EebusError::NotALimitWrite {
            detail: format!("no entry for limit id {limit_id:?}"),
        })?;

    // An absent `value` is zero watts, which is what the kit's own reader does
    // and what the schema's optionality means here: a write that activates a
    // limit and states no value is a limit of nothing.
    let watts = match entry.value.as_ref() {
        Some(value) => watts_of(value).ok_or_else(|| EebusError::UnreadableLimit {
            detail: "a scaled number no decimal can hold".to_owned(),
        })?,
        None => Decimal::ZERO,
    };
    if watts.is_sign_negative() {
        return Err(EebusError::NegativeLimit { watts });
    }

    // `[LPC §3.1.8.2]`: *"Durations used within this Use Case SHALL be presented
    // as relative times. The same holds for the `endTime` Element used for the
    // duration of validity ([LPC-004])."* An `endTime` that is present and is
    // **not** a duration — an absolute instant, which the schema's union permits
    // and this use case forbids — is a broken write.
    //
    // Reading it as `None` would be the dangerous half of the mistake: `None`
    // means *no duration*, so a limit the operator meant to expire in two hours
    // would be applied until something else replaced it.
    let valid_for = match entry.time_period.as_ref().and_then(|p| p.end_time.as_ref()) {
        Some(end_time) => Some(
            end_time
                .as_duration()
                .and_then(|d| time::Duration::try_from(d).ok())
                .ok_or_else(|| EebusError::UnreadableLimit {
                    detail: "an endTime that is not a relative duration [LPC-004]".to_owned(),
                })?,
        ),
        None => None,
    };

    Ok(LimitWrite {
        is_active: entry.is_limit_active.unwrap_or(false),
        watts,
        valid_for,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eebus::model::{LoadControlLimitData, LoadControlLimitListData};

    fn limit(value: Option<ScaledNumber>, active: bool) -> CmdData {
        CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(eebus::usecases::limitation::LIMIT_ID),
                is_limit_active: Some(active),
                value,
                ..Default::default()
            }]),
        })
    }

    #[test]
    fn the_mindestleistung_and_three_phase_six_amperes_are_sixty_watts_apart() {
        // The whole argument for this module in one assertion. Both figures
        // arrive as scaled numbers, both come out exact, and a platform that
        // cannot tell them apart cannot say whether it honoured an Anspruch
        // `[BK6-22-300 Ziff. 4.5]`.
        let anspruch = watts_of(&ScaledNumber::new(42, 2)).expect("4200 W");
        let six_amperes = watts_of(&ScaledNumber::new(414, 1)).expect("4140 W");
        assert_eq!(anspruch - six_amperes, Decimal::from(60));
    }

    #[test]
    fn a_fractional_watt_survives_the_crossing() {
        // `4139.5` W: a scale of −1 is a fraction, and it is the shape a
        // conversion through `f64` stops being exact at.
        assert_eq!(
            watts_of(&ScaledNumber::new(41_395, -1)),
            Some(Decimal::from_str_exact("4139.5").expect("a decimal"))
        );
    }

    #[test]
    fn a_scale_no_decimal_can_hold_is_refused_rather_than_saturated() {
        // `Decimal` panics on overflow rather than saturating, and a `scale` is
        // a signed 16-bit number the peer chooses: one well-formed message is
        // enough. Refusing produces a NACK; substituting a number nobody sent
        // would apply a limit nobody asked for.
        assert_eq!(watts_of(&ScaledNumber::new(i64::MAX, 300)), None);
        let refused = read_limit_write(
            &limit(Some(ScaledNumber::new(i64::MAX, 300)), true),
            eebus::usecases::limitation::LIMIT_ID,
        )
        .expect_err("no decimal holds it");
        assert!(
            matches!(refused, EebusError::UnreadableLimit { .. }),
            "{refused}"
        );
    }

    #[test]
    fn a_negative_limit_is_refused_by_name() {
        // `[LPC-001]` puts the limit at or above zero. One below it would reach
        // an allocation as a ceiling under the floor every point has, and the
        // site would hold every box at zero over a value the specification
        // forbids.
        let refused = read_limit_write(
            &limit(Some(ScaledNumber::new(-4_200, 0)), true),
            eebus::usecases::limitation::LIMIT_ID,
        )
        .expect_err("below zero");
        assert!(
            matches!(refused, EebusError::NegativeLimit { .. }),
            "{refused}"
        );
    }

    #[test]
    fn a_write_with_no_value_is_a_limit_of_nothing_rather_than_an_error() {
        let write = read_limit_write(&limit(None, true), eebus::usecases::limitation::LIMIT_ID)
            .expect("an active limit stating no value");
        assert_eq!(write.watts, Decimal::ZERO);
        assert!(write.is_active);
    }

    #[test]
    fn a_payload_that_is_not_a_limit_write_says_so() {
        let refused = read_limit_write(
            &CmdData::LoadControlLimitListData(LoadControlLimitListData {
                load_control_limit_data: Some(Vec::new()),
            }),
            eebus::usecases::limitation::LIMIT_ID,
        )
        .expect_err("no entry for this limit");
        assert!(
            matches!(refused, EebusError::NotALimitWrite { .. }),
            "{refused}"
        );
    }
}
