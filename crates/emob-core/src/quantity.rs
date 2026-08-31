//! Energy, money and time — exact, signed where the physics is signed, and
//! never a binary float.
//!
//! # Why `Decimal` and not `f64`
//!
//! OCMF *defines* a session's energy as a subtraction of two register readings
//! `[OCMF Tab. 7]`, and that number ends up on an invoice between two
//! companies. In `f64`, `10.1 - 0.1` is `10.000000000000002`. Cents decide real
//! disputes, and a kWh that is wrong in the fifteenth decimal is a kWh that
//! reconciles against nothing.
//!
//! `cargo xtask no-floats` fails the build if any public field or signature in
//! the workspace mentions `f32` or `f64`.
//!
//! # Scale is information
//!
//! `Decimal` keeps trailing zeros, and this module never strips them.
//! A register that reports `2935.600 kWh` is stating three decimal places of
//! resolution; rewriting it as `2935.6` throws away a statement the meter made
//! about its own accuracy. OCMF says so explicitly — the representation "must
//! not be transformed by further handling methods … since this would change the
//! representation of the physical quantity and thus potentially the number of
//! valid digits" `[OCMF Tab. 7, RV]`.
//!
//! # Direction is a field, not a sign
//!
//! [`Energy`] is a non-negative magnitude and [`Direction`] says which way it
//! flowed. Netting the two directions inside one billing period would let a
//! V2G discharge cancel a draw — and both would leave their supplier's
//! Bilanzkreis unaccounted for. `mako-emob` enforces the same separation on the
//! market side `[A6 §IV.1]`, and the two must not disagree.

use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Sub};

use rust_decimal::Decimal;

use crate::error::QuantityError;

/// Which way energy flowed across the meter.
///
/// Never netted against each other inside a billing period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Direction {
    /// Into the vehicle — an ordinary charge.
    #[default]
    Import,
    /// Out of the vehicle — V2G/V2H discharge.
    Export,
}

impl Direction {
    /// The OCPI/OCPP-facing spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Export => "export",
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An amount of electrical energy in kilowatt-hours, exact, non-negative.
///
/// ```
/// use emob_core::quantity::Energy;
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
///
/// // The subtraction OCMF prescribes, done exactly.
/// let start = Energy::from_kwh(Decimal::from_str("2935.600")?)?;
/// let end = Energy::from_kwh(Decimal::from_str("2965.100")?)?;
/// assert_eq!((end - start)?.to_string(), "29.500 kWh");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Energy(Decimal);

impl Energy {
    /// No energy at all.
    pub const ZERO: Self = Self(Decimal::ZERO);

    /// An energy in kWh.
    ///
    /// # Errors
    ///
    /// [`QuantityError::Negative`] for a negative magnitude — direction is
    /// [`Direction`], never a minus sign.
    pub fn from_kwh(kwh: Decimal) -> Result<Self, QuantityError> {
        if kwh.is_sign_negative() && !kwh.is_zero() {
            return Err(QuantityError::Negative {
                what: "energy",
                value: kwh.to_string(),
            });
        }
        Ok(Self(kwh))
    }

    /// An energy in watt-hours, converted to kWh **without losing resolution**.
    ///
    /// The conversion moves the decimal point rather than dividing, so a meter
    /// reporting `29500 Wh` yields `29.500 kWh` and not `29.5 kWh`. The
    /// difference is not cosmetic: the first states a resolution of one
    /// watt-hour, which is what the meter actually claimed, and the second
    /// states a hundred times less. Scale is a statement about accuracy
    /// `[OCMF Tab. 7, RV]`, and a unit conversion has no business weakening it.
    ///
    /// Falls back to division in the one case the shift cannot represent — a
    /// value already at `Decimal`'s maximum scale.
    ///
    /// # Errors
    ///
    /// [`QuantityError::Negative`] for a negative magnitude.
    pub fn from_wh(wh: Decimal) -> Result<Self, QuantityError> {
        let mut shifted = wh;
        if shifted.set_scale(wh.scale() + 3).is_ok() {
            Self::from_kwh(shifted)
        } else {
            Self::from_kwh(wh / Decimal::from(1000))
        }
    }

    /// The value in kWh, with the scale it was given.
    #[must_use]
    pub const fn kwh(self) -> Decimal {
        self.0
    }

    /// The value in Wh.
    #[must_use]
    pub fn wh(self) -> Decimal {
        self.0 * Decimal::from(1000)
    }

    /// `true` when there is no energy here at all.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    /// The difference between two register readings, in reading order.
    ///
    /// # Errors
    ///
    /// [`QuantityError::Negative`] when `earlier` exceeds `self` — a register
    /// that ran backwards is a fault to escalate, not a negative quantity to
    /// bill.
    pub fn difference_from(self, earlier: Self) -> Result<Self, QuantityError> {
        Self::from_kwh(self.0 - earlier.0)
    }
}

impl Add for Energy {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Energy {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for Energy {
    type Output = Result<Self, QuantityError>;

    /// Subtraction is fallible: energy has no negative values.
    fn sub(self, rhs: Self) -> Self::Output {
        Self::from_kwh(self.0 - rhs.0)
    }
}

impl Sum for Energy {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |a, b| a + b)
    }
}

impl fmt::Display for Energy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} kWh", self.0)
    }
}

/// An ISO 4217 currency code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Currency([u8; 3]);

impl Currency {
    /// The euro.
    pub const EUR: Self = Self(*b"EUR");

    /// Parse a three-letter currency code.
    ///
    /// # Errors
    ///
    /// [`QuantityError::BadCurrency`] when the code is not three letters.
    pub fn new(code: &str) -> Result<Self, QuantityError> {
        let bytes = code.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_alphabetic) {
            return Err(QuantityError::BadCurrency(code.to_owned()));
        }
        Ok(Self([
            bytes[0].to_ascii_uppercase(),
            bytes[1].to_ascii_uppercase(),
            bytes[2].to_ascii_uppercase(),
        ]))
    }

    /// The code as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Constructed only from ASCII letters.
        core::str::from_utf8(&self.0).unwrap_or("???")
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An amount of money: an exact decimal and the currency it is in.
///
/// Signed, because a credit note, a refund and a partner settlement are all
/// ordinary amounts that happen to be negative.
///
/// ```
/// use emob_core::quantity::{Currency, Money};
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
///
/// let a = Money::new(Decimal::from_str("0.10")?, Currency::EUR);
/// let b = Money::new(Decimal::from_str("0.20")?, Currency::EUR);
/// assert_eq!((a + b)?.to_string(), "0.30 EUR"); // exactly, unlike 0.1_f64 + 0.2
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Money {
    amount: Decimal,
    currency: Currency,
}

impl Money {
    /// An amount in a currency.
    #[must_use]
    pub const fn new(amount: Decimal, currency: Currency) -> Self {
        Self { amount, currency }
    }

    /// Zero, in a currency. Zero still has a currency: a ledger that adds a
    /// currencyless zero to a euro balance has silently agreed that the two
    /// are the same kind of thing.
    #[must_use]
    pub const fn zero(currency: Currency) -> Self {
        Self {
            amount: Decimal::ZERO,
            currency,
        }
    }

    /// The amount.
    #[must_use]
    pub const fn amount(self) -> Decimal {
        self.amount
    }

    /// The currency.
    #[must_use]
    pub const fn currency(self) -> Currency {
        self.currency
    }

    /// Round to the currency's minor unit, half away from zero — the rule
    /// German invoicing practice and EN 16931 both assume.
    #[must_use]
    pub fn round_to_minor_unit(self) -> Self {
        Self {
            amount: self
                .amount
                .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointAwayFromZero),
            currency: self.currency,
        }
    }

    /// Add, refusing to mix currencies.
    ///
    /// # Errors
    ///
    /// [`QuantityError::CurrencyMismatch`] when the currencies differ.
    pub fn checked_add(self, rhs: Self) -> Result<Self, QuantityError> {
        if self.currency != rhs.currency {
            return Err(QuantityError::CurrencyMismatch {
                left: self.currency.to_string(),
                right: rhs.currency.to_string(),
            });
        }
        Ok(Self {
            amount: self.amount + rhs.amount,
            currency: self.currency,
        })
    }
}

impl Add for Money {
    type Output = Result<Self, QuantityError>;

    fn add(self, rhs: Self) -> Self::Output {
        self.checked_add(rhs)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount, self.currency)
    }
}

/// A price per kilowatt-hour.
///
/// A distinct type from [`Money`] because the AFIR price-transparency duty is
/// stated in €/kWh `[AFIR Art. 5(4)]` and multiplying a €/kWh by a kWh must
/// produce a [`Money`], never another rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PricePerKwh {
    rate: Decimal,
    currency: Currency,
}

impl PricePerKwh {
    /// A rate in currency per kWh.
    #[must_use]
    pub const fn new(rate: Decimal, currency: Currency) -> Self {
        Self { rate, currency }
    }

    /// The rate itself.
    #[must_use]
    pub const fn rate(self) -> Decimal {
        self.rate
    }

    /// The currency.
    #[must_use]
    pub const fn currency(self) -> Currency {
        self.currency
    }

    /// What `energy` costs at this rate, unrounded.
    ///
    /// Rounding is the caller's decision because rounding per line and rounding
    /// per invoice give different totals, and which one is right is a tax
    /// question rather than an arithmetic one.
    #[must_use]
    pub fn times(self, energy: Energy) -> Money {
        Money::new(self.rate * energy.kwh(), self.currency)
    }
}

impl fmt::Display for PricePerKwh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}/kWh", self.rate, self.currency)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn the_subtraction_ocmf_prescribes_is_exact() {
        // In f64 this is 10.000000000000002.
        let start = Energy::from_kwh(dec("0.1")).unwrap();
        let end = Energy::from_kwh(dec("10.2")).unwrap();
        assert_eq!((end - start).unwrap().kwh(), dec("10.1"));
    }

    #[test]
    fn scale_survives_because_it_is_a_claim_about_accuracy() {
        let e = Energy::from_kwh(dec("2935.600")).unwrap();
        assert_eq!(e.to_string(), "2935.600 kWh");
        let end = Energy::from_kwh(dec("2965.100")).unwrap();
        assert_eq!((end - e).unwrap().to_string(), "29.500 kWh");
    }

    #[test]
    fn energy_has_no_negative_values() {
        assert!(Energy::from_kwh(dec("-1")).is_err());
        let small = Energy::from_kwh(dec("1")).unwrap();
        let large = Energy::from_kwh(dec("2")).unwrap();
        // A register that ran backwards is a fault, not a negative quantity.
        assert!((small - large).is_err());
        assert!(large.difference_from(small).is_ok());
    }

    #[test]
    fn negative_zero_is_still_zero() {
        assert!(Energy::from_kwh(dec("-0.00")).is_ok());
    }

    #[test]
    fn wh_to_kwh_preserves_the_meters_resolution() {
        // 29500 Wh states one-watt-hour resolution. `29.5 kWh` would state a
        // hundred times less, so the conversion shifts the point rather than
        // dividing.
        let e = Energy::from_wh(dec("29500")).unwrap();
        assert_eq!(e.kwh().to_string(), "29.500");
        assert_eq!(e.wh(), dec("29500.000"));
        assert_eq!(e.kwh(), dec("29.5"), "still numerically equal");

        // And a value that already has decimals keeps them all.
        let f = Energy::from_wh(dec("1234.5")).unwrap();
        assert_eq!(f.kwh().to_string(), "1.2345");
    }

    #[test]
    fn money_refuses_to_mix_currencies() {
        let eur = Money::new(dec("1.00"), Currency::EUR);
        let chf = Money::new(dec("1.00"), Currency::new("CHF").unwrap());
        assert!((eur + chf).is_err());
        assert_eq!((eur + eur).unwrap().amount(), dec("2.00"));
    }

    #[test]
    fn money_addition_is_exact() {
        // 0.1 + 0.2 == 0.30000000000000004 in f64.
        let a = Money::new(dec("0.10"), Currency::EUR);
        let b = Money::new(dec("0.20"), Currency::EUR);
        assert_eq!((a + b).unwrap().amount(), dec("0.30"));
    }

    #[test]
    fn rounding_is_half_away_from_zero() {
        let m = Money::new(dec("1.005"), Currency::EUR);
        assert_eq!(m.round_to_minor_unit().amount(), dec("1.01"));
        let n = Money::new(dec("-1.005"), Currency::EUR);
        assert_eq!(n.round_to_minor_unit().amount(), dec("-1.01"));
    }

    #[test]
    fn a_rate_times_energy_is_money() {
        let rate = PricePerKwh::new(dec("0.49"), Currency::EUR);
        let energy = Energy::from_kwh(dec("29.500")).unwrap();
        assert_eq!(rate.times(energy).amount(), dec("14.45500"));
        assert_eq!(
            rate.times(energy).round_to_minor_unit().amount(),
            dec("14.46")
        );
    }

    #[test]
    fn currency_must_be_three_letters() {
        assert!(Currency::new("EU").is_err());
        assert!(Currency::new("EURO").is_err());
        assert!(Currency::new("E1R").is_err());
        assert_eq!(Currency::new("eur").unwrap(), Currency::EUR);
    }

    #[test]
    fn directions_are_distinct() {
        assert_ne!(Direction::Import, Direction::Export);
        assert_eq!(Direction::Import.as_str(), "import");
    }
}
