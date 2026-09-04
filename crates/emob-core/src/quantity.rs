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

/// How many decimal places an **apportioned** quantity is quoted to.
///
/// See [`apportion`]. Twelve is a nanowatt-hour, nine orders of magnitude finer
/// than the milli-kilowatt-hour a meter states.
pub const APPORTIONED_SCALE: u32 = 12;

/// The cumulative value `offset` units into a window of `span` units across
/// which a register moved by `delta`, counting from `base`.
///
/// The one arithmetic two crates share: `emob-session` places a boundary
/// between two meter readings, and `emob-tariff` places a tariff threshold
/// inside a period. Both are "how far along is the register", both settle
/// money, and two spellings of it would eventually be two answers.
///
/// # Multiply, then divide
///
/// `delta × offset / span` keeps every digit the arithmetic allows;
/// `delta × (offset / span)` has already spent the decimal's precision on a
/// repeating fraction before the multiplication. Seven kilowatt-hours two
/// thirds of the way through a window is `4.666…` either way, and the first
/// form is exact wherever the ratio terminates.
///
/// # …and then round, because conservation is a statement about a sum
///
/// Both callers build a series of boundary values and take **differences**, so
/// that the pieces telescope back to the whole: every interior boundary appears
/// once positive and once negative and cancels, whatever it was rounded to.
/// That argument is arithmetic rather than floating-point folklore, and it has
/// one precondition — the additions themselves must be exact.
///
/// `Decimal` carries a 96-bit mantissa, which is about twenty-nine significant
/// digits. A ratio that does not terminate spends **all** of them on the
/// fraction, and adding two such values needs more digits than there are: the
/// sum is rounded, the interior boundaries no longer cancel, and a conservation
/// check that reads `==` fails by one unit in the last place. It is a
/// microscopic error and it is in exactly the assertion that exists to prove
/// there is none.
///
/// So an apportioned value is quoted to [`APPORTIONED_SCALE`] places. Every
/// difference then carries at most that many, every sum of them is exact up to
/// totals no charging session reaches, and the telescoping identity holds as
/// written rather than nearly. A nanowatt-hour is three microjoules; the meter
/// that could measure one has not been built.
///
/// `base` is returned unrounded and unchanged for a window of no span, which is
/// a window nothing can be apportioned across.
///
/// ```
/// use emob_core::quantity::apportion;
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
///
/// // Seven kilowatt-hours, fourteen seconds into a twenty-one second window.
/// let at = apportion(Decimal::ZERO, Decimal::from_str("7")?, 14, 21);
/// assert_eq!(at.to_string(), "4.666666666667");
///
/// // The pieces telescope back to the whole, exactly.
/// let rest = Decimal::from_str("7")? - at;
/// assert_eq!(at + rest, Decimal::from_str("7")?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn apportion(base: Decimal, delta: Decimal, offset: u64, span: u64) -> Decimal {
    if span == 0 {
        return base;
    }
    // The increment is rounded and the base is not: the base is a boundary that
    // was already quoted this way, or a reading the meter stated.
    base + (delta * Decimal::from(offset) / Decimal::from(span)).round_dp(APPORTIONED_SCALE)
}

/// An ISO 4217 currency code.
///
/// Written on the wire as the three letters — `"EUR"` — and never as the three
/// bytes. A `#[serde(transparent)]` newtype over `[u8; 3]` serialises to
/// `[69, 85, 82]`, which round-trips through this crate perfectly and is
/// unreadable to every partner, invoice format and human that a rated CDR
/// passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Currency([u8; 3]);

#[cfg(feature = "serde")]
impl serde::Serialize for Currency {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Currency {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        // Through `new`, so a code that arrives from a partner is validated on
        // the way in rather than trusted because it was already typed.
        let code = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(&code).map_err(D::Error::custom)
    }
}

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

    /// How many decimal places this currency's minor unit has — the ISO 4217
    /// exponent.
    ///
    /// Two for almost everything, and the exceptions are the point: a total
    /// rounded to two decimals in yen invents a hundredth of a unit that does
    /// not exist, and one rounded to two in Kuwaiti dinar throws a fils away.
    /// Hard-coding `2` works until the day it does not, and that day arrives on
    /// an invoice.
    ///
    /// Unknown codes get two, which is the right guess and a documented one.
    #[must_use]
    pub fn minor_unit_digits(self) -> u32 {
        match self.as_str() {
            // Exponent 0 — no minor unit at all.
            "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF"
            | "UGX" | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
            // Exponent 3.
            "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
            // Everything a European charging platform actually meets — EUR,
            // CHF, GBP, the Nordic and CEE currencies — is exponent 2.
            _ => 2,
        }
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
    ///
    /// The number of places comes from [`Currency::minor_unit_digits`], not
    /// from a hard-coded two, so a yen total is a whole number and a dinar
    /// total keeps its third decimal.
    ///
    /// # It rounds *to* the minor unit, in both directions
    ///
    /// `round_dp` narrows a value that is too precise and leaves one that is
    /// not alone, so `11.90 / 1.19` comes back as `10` — scale zero — and an
    /// invoice line beside `8.44` prints `10`. That is the same money and it is
    /// a document that looks broken, and worse, a partner diffing two exports
    /// sees a change where none happened.
    ///
    /// So the scale is **set**, not merely capped. Money is the one quantity in
    /// this workspace where scale is a property of the *currency* rather than a
    /// claim by the instrument that measured it — a euro has two decimal places
    /// whatever arithmetic produced the figure, which is exactly the opposite of
    /// the rule [`Energy`] keeps and the reason the two are separate types.
    #[must_use]
    pub fn round_to_minor_unit(self) -> Self {
        let digits = self.currency.minor_unit_digits();
        let mut amount = self
            .amount
            .round_dp_with_strategy(digits, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
        // `rescale` only ever widens here — the rounding above already narrowed
        // anything wider — so no digit is lost.
        amount.rescale(digits);
        Self {
            amount,
            currency: self.currency,
        }
    }

    /// Subtract, refusing to mix currencies.
    ///
    /// # Errors
    ///
    /// [`QuantityError::CurrencyMismatch`] when the currencies differ.
    pub fn checked_sub(self, rhs: Self) -> Result<Self, QuantityError> {
        if self.currency != rhs.currency {
            return Err(QuantityError::CurrencyMismatch {
                left: self.currency.to_string(),
                right: rhs.currency.to_string(),
            });
        }
        Ok(Self {
            amount: self.amount - rhs.amount,
            currency: self.currency,
        })
    }

    /// `true` when there is no money here at all.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.amount.is_zero()
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

impl Sub for Money {
    type Output = Result<Self, QuantityError>;

    fn sub(self, rhs: Self) -> Self::Output {
        self.checked_sub(rhs)
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
    fn rounding_sets_the_scale_rather_than_only_capping_it() {
        // `11.90 / 1.19` is exactly 10, at scale zero — and an invoice line
        // printing `10 EUR` beside one printing `8.44 EUR` is a document that
        // looks broken. A euro has two decimal places whatever arithmetic
        // produced the figure, which is the opposite of the rule `Energy` keeps.
        let exact = Money::new(dec("11.90") / dec("1.19"), Currency::EUR);
        assert_eq!(exact.round_to_minor_unit().to_string(), "10.00 EUR");
        assert_eq!(
            Money::new(dec("7"), Currency::EUR)
                .round_to_minor_unit()
                .to_string(),
            "7.00 EUR"
        );
        assert_eq!(
            Money::zero(Currency::EUR).round_to_minor_unit().to_string(),
            "0.00 EUR"
        );

        // …and the currency's own unit, not a hard-coded two.
        let jpy = Currency::new("JPY").unwrap();
        assert_eq!(
            Money::new(dec("1234.5"), jpy)
                .round_to_minor_unit()
                .to_string(),
            "1235 JPY"
        );
        let kwd = Currency::new("KWD").unwrap();
        assert_eq!(
            Money::new(dec("1.2"), kwd)
                .round_to_minor_unit()
                .to_string(),
            "1.200 KWD"
        );
    }

    #[test]
    fn money_subtraction_refuses_to_mix_currencies_too() {
        // A credit note, a refund and a partner settlement are all ordinary
        // amounts that happen to be negative.
        let a = Money::new(dec("10.00"), Currency::EUR);
        let b = Money::new(dec("14.46"), Currency::EUR);
        assert_eq!((a - b).unwrap().amount(), dec("-4.46"));
        assert!((a - Money::new(dec("1.00"), Currency::new("CHF").unwrap())).is_err());
        assert!(Money::zero(Currency::EUR).is_zero());
    }

    #[test]
    fn rounding_follows_the_currencys_own_minor_unit() {
        // Hard-coding two decimals invents a hundredth of a yen that does not
        // exist, and throws away a fils.
        let jpy = Currency::new("JPY").unwrap();
        assert_eq!(jpy.minor_unit_digits(), 0);
        assert_eq!(
            Money::new(dec("1234.56"), jpy)
                .round_to_minor_unit()
                .amount(),
            dec("1235")
        );

        let kwd = Currency::new("KWD").unwrap();
        assert_eq!(kwd.minor_unit_digits(), 3);
        assert_eq!(
            Money::new(dec("1.23456"), kwd)
                .round_to_minor_unit()
                .amount(),
            dec("1.235")
        );

        // Everything a European charging platform actually meets is two.
        for code in [
            "EUR", "CHF", "GBP", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF",
        ] {
            assert_eq!(
                Currency::new(code).unwrap().minor_unit_digits(),
                2,
                "{code}"
            );
        }
        // …and an unknown code gets the right guess, documented.
        assert_eq!(Currency::new("ZZZ").unwrap().minor_unit_digits(), 2);
    }

    #[test]
    fn currency_must_be_three_letters() {
        assert!(Currency::new("EU").is_err());
        assert!(Currency::new("EURO").is_err());
        assert!(Currency::new("E1R").is_err());
        assert_eq!(Currency::new("eur").unwrap(), Currency::EUR);
    }

    #[test]
    fn apportioning_telescopes_however_many_pieces_there_are() {
        // The property both callers rest on: a window cut into pieces sums back
        // to the window. Without the scale floor the additions round — a ratio
        // that does not terminate spends the whole 96-bit mantissa on its
        // fraction, and two of them cannot be added exactly.
        for span in [21_u64, 3600, 4497, 5400] {
            for delta in ["7", "22.163", "31.077", "0.001"] {
                let delta = dec(delta);
                let mut carried = Decimal::ZERO;
                let mut sum = Decimal::ZERO;
                for piece in 1..=7_u64 {
                    let at = apportion(Decimal::ZERO, delta, span * piece / 7, span);
                    sum += at - carried;
                    carried = at;
                }
                assert_eq!(sum, delta, "{delta} over {span}");
            }
        }
    }

    #[test]
    fn an_apportioned_value_never_exceeds_the_window_it_came_from() {
        // Rounding is to a scale the delta itself already fits in, so the last
        // piece cannot round past the whole — which would be a negative
        // remainder, and `Energy` has none.
        let delta = dec("29.500");
        for offset in 0..=900_u64 {
            let at = apportion(dec("100.000"), delta, offset, 900);
            assert!(at >= dec("100.000") && at <= dec("129.500"), "{at}");
        }
    }

    #[test]
    fn a_window_of_no_span_apportions_nothing() {
        assert_eq!(apportion(dec("12.5"), dec("7"), 0, 0), dec("12.5"));
    }

    #[test]
    fn directions_are_distinct() {
        assert_ne!(Direction::Import, Direction::Export);
        assert_eq!(Direction::Import.as_str(), "import");
    }
}
