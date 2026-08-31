//! The tariff: what a session costs, and the only thing allowed to say so.
//!
//! # One object, two jobs
//!
//! A charging tariff has to do two things that platforms usually implement
//! twice: it has to **rate** a finished session, and it has to be **displayed**
//! to a driver before the session starts `[AFIR Art. 5(4)]`. When those come
//! from two places they drift, and the failure mode is a driver charged
//! something other than the price they were shown — which is the
//! price-transparency breach the article exists to prevent.
//!
//! So there is one [`Tariff`]. [`crate::rating::rate`] reads its price
//! components; [`crate::display::describe`] reads the same ones. Neither can
//! quote a number the other does not use, and a test asserts it.
//!
//! # Shape
//!
//! The structure follows OCPI's, because that is what a roaming partner will
//! send and expect: a tariff is a list of *elements*, each a list of *price
//! components* guarded by *restrictions*. The first element whose restrictions
//! match a period is the one that prices it.

use emob_core::{Currency, TariffId};
use rust_decimal::Decimal;

/// What a price component charges for.
///
/// The order of the variants is the order `[AFIR Art. 5(4)]` prescribes for
/// display below 50 kW — per kWh, per minute, per session, then anything else —
/// and [`Ord`] is derived so that sorting a list of components *is* complying
/// with the article. See [`crate::display`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "SCREAMING_SNAKE_CASE"))]
pub enum Dimension {
    /// Energy delivered, priced per kWh.
    Energy,
    /// Time spent charging, priced per hour.
    Time,
    /// Time connected but not charging, priced per hour. The "occupancy fee"
    /// `[AFIR Art. 5(4)]` permits at 50 kW and above, *in addition* to the
    /// energy price.
    ParkingTime,
    /// A fixed amount per session.
    Flat,
}

impl Dimension {
    /// The unit a price for this dimension is quoted in.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Energy => "kWh",
            Self::Time | Self::ParkingTime => "h",
            Self::Flat => "session",
        }
    }

    /// Whether this dimension prices energy.
    ///
    /// The question `[AFIR Art. 5(4)]` turns on at 50 kW and above: the ad-hoc
    /// price there "shall be based on the price per kWh".
    #[must_use]
    pub const fn is_energy(self) -> bool {
        matches!(self, Self::Energy)
    }
}

/// One priced dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PriceComponent {
    /// What is being charged for.
    pub dimension: Dimension,
    /// The price per unit, in the tariff's currency.
    pub price: Decimal,
    /// The VAT percentage, when one applies.
    pub vat: Option<Decimal>,
    /// The block size the dimension is billed in.
    ///
    /// One Wh for [`Dimension::Energy`], one second for the time dimensions,
    /// meaningless for [`Dimension::Flat`]. OCPI 3.0 removes the field and
    /// advises setting it to 1; [`PriceComponent::new`] does, and a value above
    /// 1 is deliberate rounding *against* the customer that has to be written
    /// out in full.
    pub step_size: u32,
}

impl PriceComponent {
    /// A component with no VAT and no block rounding.
    #[must_use]
    pub const fn new(dimension: Dimension, price: Decimal) -> Self {
        Self {
            dimension,
            price,
            vat: None,
            step_size: 1,
        }
    }

    /// The same component with a VAT percentage.
    #[must_use]
    pub const fn with_vat(mut self, vat: Decimal) -> Self {
        self.vat = Some(vat);
        self
    }

    /// The same component billed in blocks.
    ///
    /// # Panics
    ///
    /// Never. A `step_size` of 0 is coerced to 1, because billing in blocks of
    /// zero is not a rounding policy, it is a division by zero waiting to
    /// happen in whatever reads the field next.
    #[must_use]
    pub const fn with_step_size(mut self, step_size: u32) -> Self {
        self.step_size = if step_size == 0 { 1 } else { step_size };
        self
    }
}

/// When an element applies.
///
/// Only the restrictions this crate can evaluate from a session are modelled.
/// A tariff carrying restrictions outside this set is not silently treated as
/// unrestricted — [`crate::rating::rate`] reports it — because assuming a
/// restriction is absent is how a night tariff gets applied at noon.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Restrictions {
    /// Applies only from this time of day, local to the station.
    pub start_time: Option<time::Time>,
    /// Applies only until this time of day.
    pub end_time: Option<time::Time>,
    /// Applies only once this much energy has been delivered, in kWh.
    pub min_kwh: Option<Decimal>,
    /// Applies only below this much energy, in kWh.
    pub max_kwh: Option<Decimal>,
    /// Applies only from this duration into the session, in seconds.
    pub min_duration_s: Option<u64>,
    /// Applies only below this duration, in seconds.
    pub max_duration_s: Option<u64>,
    /// Applies only on these weekdays. Empty means every day.
    pub days_of_week: Vec<time::Weekday>,
}

impl Restrictions {
    /// Whether anything is restricted at all.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        *self == Self::default()
    }
}

/// A group of price components and the restrictions that gate them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TariffElement {
    /// What this element charges.
    pub components: Vec<PriceComponent>,
    /// When it applies.
    pub restrictions: Restrictions,
}

impl TariffElement {
    /// An element that always applies.
    #[must_use]
    pub fn unrestricted(components: Vec<PriceComponent>) -> Self {
        Self {
            components,
            restrictions: Restrictions::default(),
        }
    }

    /// The component for a dimension, if this element prices it.
    #[must_use]
    pub fn component(&self, dimension: Dimension) -> Option<&PriceComponent> {
        self.components.iter().find(|c| c.dimension == dimension)
    }
}

/// Whether prices include tax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "SCREAMING_SNAKE_CASE"))]
pub enum TaxIncluded {
    /// The prices are gross.
    Yes,
    /// The prices are net; VAT is added.
    No,
    /// The party does not know, because no tax regime applies to it.
    NotApplicable,
}

/// Who a tariff is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TariffKind {
    /// The price a driver with no contract pays at the point.
    ///
    /// The one `[AFIR Art. 5(4)]` regulates: it must be shown before the
    /// session starts, and at 50 kW and above it must be based on a price per
    /// kWh.
    AdHoc,
    /// The price under a contract with an e-mobility provider.
    Contract,
}

/// A tariff.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tariff {
    /// Which tariff.
    pub id: TariffId,
    /// The currency every price in it is quoted in.
    pub currency: Currency,
    /// Who it is for.
    pub kind: TariffKind,
    /// Whether the prices include tax.
    pub tax_included: TaxIncluded,
    /// The elements, in the order they are tried. Cardinality `+`.
    pub elements: Vec<TariffElement>,
    /// A session under this tariff costs at least this much.
    pub min_price: Option<Decimal>,
    /// A session under this tariff costs at most this much.
    pub max_price: Option<Decimal>,
}

impl Tariff {
    /// A tariff with one unrestricted element.
    #[must_use]
    pub fn simple(
        id: TariffId,
        currency: Currency,
        kind: TariffKind,
        components: Vec<PriceComponent>,
    ) -> Self {
        Self {
            id,
            currency,
            kind,
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement::unrestricted(components)],
            min_price: None,
            max_price: None,
        }
    }

    /// Every dimension this tariff prices anywhere.
    #[must_use]
    pub fn dimensions(&self) -> Vec<Dimension> {
        let mut found: Vec<Dimension> = self
            .elements
            .iter()
            .flat_map(|e| e.components.iter().map(|c| c.dimension))
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Whether any element prices energy.
    ///
    /// The `[AFIR Art. 5(4)]` question at 50 kW and above.
    #[must_use]
    pub fn prices_energy(&self) -> bool {
        self.dimensions().iter().any(|d| d.is_energy())
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
    fn dimensions_order_the_way_afir_prescribes() {
        // Sorting the components *is* complying with the display order, so the
        // ordering is a property of the type rather than of a formatter.
        let mut ds = vec![
            Dimension::Flat,
            Dimension::ParkingTime,
            Dimension::Energy,
            Dimension::Time,
        ];
        ds.sort_unstable();
        assert_eq!(
            ds,
            vec![
                Dimension::Energy,
                Dimension::Time,
                Dimension::ParkingTime,
                Dimension::Flat
            ]
        );
    }

    #[test]
    fn a_zero_step_size_becomes_one() {
        // Billing in blocks of zero is a division waiting to happen.
        let c = PriceComponent::new(Dimension::Energy, dec("0.49")).with_step_size(0);
        assert_eq!(c.step_size, 1);
    }

    #[test]
    fn a_tariff_knows_whether_it_prices_energy() {
        let energy = Tariff::simple(
            "t1".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
        );
        assert!(energy.prices_energy());

        let by_the_minute = Tariff::simple(
            "t2".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Time, dec("0.10"))],
        );
        assert!(!by_the_minute.prices_energy());
    }

    #[test]
    fn dimensions_are_reported_once_each_and_in_order() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement::unrestricted(vec![
                    PriceComponent::new(Dimension::Flat, dec("0.50")),
                    PriceComponent::new(Dimension::Energy, dec("0.49")),
                ]),
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.59"),
                )]),
            ],
            min_price: None,
            max_price: None,
        };
        assert_eq!(t.dimensions(), vec![Dimension::Energy, Dimension::Flat]);
    }

    #[test]
    fn units_are_the_ones_prices_are_quoted_in() {
        assert_eq!(Dimension::Energy.unit(), "kWh");
        assert_eq!(Dimension::Time.unit(), "h");
        assert_eq!(Dimension::Flat.unit(), "session");
    }
}
