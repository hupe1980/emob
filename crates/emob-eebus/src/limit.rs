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

use eebus::model::{CmdData, LoadControlLimitData, LoadControlLimitId, ScaledNumber};
use rust_decimal::Decimal;

use crate::EebusError;

/// A limit a grid operator wrote, in this workspace's own arithmetic.
///
/// Carries the activation, exact watts and validity from a resolved LPC write.
/// Watts are a decimal so the allocator can use them directly.
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
    let canonical = value.canonical();
    let mantissa = i128::from(canonical.number?.get());
    let scale = i32::from(canonical.scale.map_or(0, eebus::model::Scale::get));
    if scale <= 0 {
        return Decimal::try_from_i128_with_scale(mantissa, scale.unsigned_abs()).ok();
    }
    let factor = 10_i128.checked_pow(scale.unsigned_abs())?;
    Decimal::try_from_i128_with_scale(mantissa.checked_mul(factor)?, 0).ok()
}

pub(crate) fn entry_for(
    resolved: &CmdData,
    limit_id: LoadControlLimitId,
) -> Result<&LoadControlLimitData, EebusError> {
    let CmdData::LoadControlLimitListData(list) = resolved else {
        return Err(EebusError::NotALimitWrite {
            detail: "the payload is not a LoadControlLimitListData".to_owned(),
        });
    };
    let mut matching = list
        .load_control_limit_data
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.limit_id == Some(limit_id));
    let entry = matching.next().ok_or_else(|| EebusError::NotALimitWrite {
        detail: format!("no entry for limit id {limit_id:?}"),
    })?;
    if matching.next().is_some() {
        return Err(EebusError::UnreadableLimit {
            detail: format!("multiple entries for limit id {limit_id:?}"),
        });
    }
    Ok(entry)
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
/// [`EebusError::UnreadableLimit`] for duplicate entries, missing activation or
/// value or invalid validity. [`EebusError::OutsidePrecision`] identifies unrepresentable watts. [`EebusError::NegativeLimit`]
/// identifies a consumption limit below zero, which
/// `[LPC-001]` forbids and which would otherwise reach an allocation as a
/// ceiling below the floor every point has.
pub fn read_limit_write(
    resolved: &CmdData,
    limit_id: LoadControlLimitId,
) -> Result<LimitWrite, EebusError> {
    let entry = entry_for(resolved, limit_id)?;
    let is_active = entry
        .is_limit_active
        .ok_or_else(|| EebusError::UnreadableLimit {
            detail: "the resolved limit has no activation flag".to_owned(),
        })?;
    let value = entry
        .value
        .as_ref()
        .ok_or_else(|| EebusError::UnreadableLimit {
            detail: "the resolved limit has no value".to_owned(),
        })?;
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

    let number = value.number.ok_or_else(|| EebusError::UnreadableLimit {
        detail: "missing scaled number".to_owned(),
    })?;
    let watts = watts_of(value).ok_or_else(|| {
        if number.get() < 0 {
            EebusError::UnreadableLimit {
                detail: "negative consumption limit".to_owned(),
            }
        } else {
            EebusError::OutsidePrecision
        }
    })?;
    if watts.is_sign_negative() {
        return Err(EebusError::NegativeLimit { watts });
    }

    Ok(LimitWrite {
        is_active,
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
        assert_eq!(watts_of(&ScaledNumber::new(1, -29)), None);
        assert_eq!(watts_of(&ScaledNumber::new(15, -29)), None);
        assert_eq!(
            watts_of(&ScaledNumber::new(10, -29)),
            Some(Decimal::from_parts(1, 0, 0, false, 28))
        );
        let refused = read_limit_write(
            &limit(Some(ScaledNumber::new(i64::MAX, 300)), true),
            eebus::usecases::limitation::LIMIT_ID,
        )
        .expect_err("no decimal holds it");
        assert!(matches!(refused, EebusError::OutsidePrecision), "{refused}");
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
    fn resolved_writes_require_one_value_and_activation_and_accept_explicit_zero() {
        use eebus::usecases::limitation::{self, Direction, LIMIT_ID};

        let zero = ScaledNumber::new(0, 0);
        for active in [true, false] {
            let data = limit(Some(zero), active);
            let reference = limitation::read_limit_write(&data, LIMIT_ID, Direction::Consumption)
                .expect("explicit zero is a complete write");
            let actual = read_limit_write(&data, LIMIT_ID).unwrap();
            assert_eq!(actual.is_active, reference.is_active);
            assert_eq!(actual.watts, Decimal::ZERO);
        }

        let complete = LoadControlLimitData {
            limit_id: Some(LIMIT_ID),
            is_limit_active: Some(true),
            value: Some(zero),
            ..Default::default()
        };
        for entries in [
            vec![LoadControlLimitData {
                value: None,
                ..complete.clone()
            }],
            vec![LoadControlLimitData {
                is_limit_active: None,
                ..complete.clone()
            }],
            vec![complete.clone(), complete],
        ] {
            let data = CmdData::LoadControlLimitListData(LoadControlLimitListData {
                load_control_limit_data: Some(entries),
            });
            assert!(
                limitation::read_limit_write(&data, LIMIT_ID, Direction::Consumption).is_none()
            );
            assert!(matches!(
                read_limit_write(&data, LIMIT_ID),
                Err(EebusError::UnreadableLimit { .. })
            ));
        }
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
