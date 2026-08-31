//! What a charge point *is*, in the terms the law asks about.
//!
//! This is deliberately not a rich station model — that lives in `emob-poi`,
//! keyed to OCPI's `Location`/`EVSE`/`Connector`. What is here is the set of
//! facts every regulatory question turns on: how much power, to whom is it
//! accessible, when was it built, what can it speak, and how does someone pay.
//!
//! Keeping that set small and explicit is the point. An obligation that reads
//! twelve fields off a two-hundred-field aggregate cannot be audited; one that
//! reads [`ChargePointProfile`] can be read in a sitting and matched against
//! the Verordnung it cites.

use rust_decimal::Decimal;
use time::Date;

use crate::ids::EvseId;

/// Who may use a charge point.
///
/// AFIR's duties attach almost entirely to *publicly accessible* points
/// `[AFIR Art. 2(48)]`; a depot behind a fence is a different regime, and the
/// 2027 ISO 15118-20 duty is the one place a private point is also bound
/// `[DA-656 Annex II 1.2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Accessibility {
    /// Publicly accessible in the AFIR sense.
    Public,
    /// Restricted to a defined group — a depot, a workplace, a home.
    Private,
}

/// Alternating or direct current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "UPPERCASE"))]
pub enum CurrentType {
    /// AC — Mode 3, Type 2.
    Ac,
    /// DC — Mode 4, Combo 2.
    Dc,
}

/// How a driver without a contract can pay.
///
/// AFIR distinguishes the two sharply: a card reader is required at new DC
/// points of at least 50 kW, while every public point must at minimum offer
/// *some* ad-hoc path `[AFIR Art. 5(1)–(2)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AdHocPayment {
    /// Nothing: a contract is the only way in.
    None,
    /// A web/QR flow, or an app that does not require registration.
    DigitalOnly,
    /// A payment card reader at the point itself (contactless at minimum).
    CardReader,
}

/// Which vehicle-communication generations a point implements.
///
/// `Pwm` alone is the legacy basic charging of IEC 61851; the DA-656 duty
/// explicitly exempts existing PWM-only points, which is why it is a variant
/// rather than an absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct V2gCommunication {
    /// Basic signalling only (IEC 61851 PWM).
    pub pwm: bool,
    /// EN ISO 15118-2:2016 and the -1/-3/-4/-5 set around it.
    pub iso15118_2: bool,
    /// EN ISO 15118-20:2022 — the generation AFIR requires from 2027.
    pub iso15118_20: bool,
}

impl V2gCommunication {
    /// A point that can only do basic PWM signalling.
    #[must_use]
    pub const fn pwm_only() -> Self {
        Self {
            pwm: true,
            iso15118_2: false,
            iso15118_20: false,
        }
    }

    /// A point that speaks both high-level generations.
    #[must_use]
    pub const fn both_generations() -> Self {
        Self {
            pwm: true,
            iso15118_2: true,
            iso15118_20: true,
        }
    }
}

/// The data-publication duties a point is wired for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DataPublication {
    /// Static and dynamic data reach the National Access Point.
    pub national_access_point: bool,
    /// …in the DATEX II Recharging profile.
    pub datex2: bool,
    /// The point is entered in the Bundesnetzagentur's Ladesäulenregister.
    pub registered: bool,
}

/// The Eichrecht posture of a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MeteringPosture {
    /// The meter carries a conformity assessment under the MID.
    pub mid_conformity_assessed: bool,
    /// The point emits signed measured values (OCMF or equivalent).
    pub signed_values: bool,
    /// Sessions are billed by energy. A point that bills by time or not at all
    /// is a different Eichrecht case.
    pub bills_by_energy: bool,
}

/// Everything an obligation may ask about one charge point.
///
/// Deliberately a flat bag of facts rather than a nest of sub-structures: each
/// field is one thing a Verordnung asks about, and an obligation that reads
/// three of them can be checked against the text it cites in a sitting.
///
/// Construct it from whatever the platform knows — `emob-poi` builds it from
/// the OCPI model, the simulator builds it from a fixture — and hand it to
/// [`crate::obligation::assess`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one regulatory fact"
)]
pub struct ChargePointProfile {
    /// Which point this is.
    pub evse_id: EvseId,
    /// Who may use it.
    pub accessibility: Accessibility,
    /// AC or DC.
    pub current_type: CurrentType,
    /// Rated power in kW. `Decimal` rather than a float because the 50 kW
    /// threshold is a legal boundary and `49.999999999999996 >= 50.0` is the
    /// kind of comparison that should never be able to go wrong.
    pub rated_power_kw: Decimal,
    /// When the point was put into service.
    pub commissioned_on: Date,
    /// When it was last substantially renovated, if ever. AFIR's "newly
    /// installed or renovated" wording makes a renovation a second birth date.
    pub renovated_on: Option<Date>,
    /// Whether the point sits along the trans-European transport network,
    /// which triggers the 2027 card-reader retrofit `[AFIR Art. 5(2)]`.
    pub on_ten_t: bool,
    /// How someone without a contract pays.
    pub ad_hoc_payment: AdHocPayment,
    /// Whether the ad-hoc price is shown before the session starts.
    pub price_displayed_before_session: bool,
    /// Whether the ad-hoc price is free of roaming surcharges.
    pub ad_hoc_price_free_of_roaming_surcharge: bool,
    /// Which vehicle-communication generations it speaks.
    pub v2g: V2gCommunication,
    /// Whether it offers Plug & Charge.
    pub offers_plug_and_charge: bool,
    /// What is published about it, and where.
    pub data: DataPublication,
    /// Its metering posture.
    pub metering: MeteringPosture,
    /// Whether third parties can charge here — a THG-Quote precondition
    /// alongside the register entry `[38k]`.
    pub open_to_third_parties: bool,
}

impl ChargePointProfile {
    /// The date the point counts as "new" from: its renovation if it has had
    /// one, otherwise its commissioning.
    ///
    /// AFIR attaches several duties to points "newly installed or renovated"
    /// after a date, so a 2019 point renovated in 2026 is inside the 2026 duty
    /// and a 2019 point left alone is not.
    #[must_use]
    pub fn effective_installation_date(&self) -> Date {
        self.renovated_on.unwrap_or(self.commissioned_on)
    }

    /// `true` when the point is publicly accessible.
    #[must_use]
    pub fn is_public(&self) -> bool {
        self.accessibility == Accessibility::Public
    }

    /// `true` for a point of at least 50 kW — the AFIR threshold for card
    /// readers and for several reporting duties.
    #[must_use]
    pub fn is_at_least_50_kw(&self) -> bool {
        self.rated_power_kw >= Decimal::from(50)
    }

    /// `true` when the point does high-level communication at all. A point
    /// that only signals with PWM is exempt from the DA-656 retrofit duty.
    #[must_use]
    pub fn does_high_level_communication(&self) -> bool {
        self.v2g.iso15118_2 || self.v2g.iso15118_20
    }

    /// `true` for an existing point that only signals with PWM.
    ///
    /// `[DA-656]` exempts these from the ISO 15118 duties in as many words, and
    /// naming the exemption is worth a method: it appears in two obligations and
    /// is the kind of double negative that is misread once and then trusted.
    #[must_use]
    pub fn is_legacy_pwm_only(&self) -> bool {
        self.v2g.pwm && !self.does_high_level_communication()
    }

    /// A minimal profile for tests and fixtures: a public AC point,
    /// commissioned on the given date, with nothing enabled.
    ///
    /// Every flag defaults to the *non-compliant* value on purpose. A fixture
    /// that starts compliant hides the obligation it should be exercising.
    #[must_use]
    pub fn bare(evse_id: EvseId, commissioned_on: Date) -> Self {
        Self {
            evse_id,
            accessibility: Accessibility::Public,
            current_type: CurrentType::Ac,
            rated_power_kw: Decimal::from(11),
            commissioned_on,
            renovated_on: None,
            on_ten_t: false,
            ad_hoc_payment: AdHocPayment::None,
            price_displayed_before_session: false,
            ad_hoc_price_free_of_roaming_surcharge: false,
            v2g: V2gCommunication::pwm_only(),
            offers_plug_and_charge: false,
            data: DataPublication::default(),
            metering: MeteringPosture::default(),
            open_to_third_parties: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn evse() -> EvseId {
        "DE*AB7*E840*6487".parse().unwrap()
    }

    #[test]
    fn a_renovation_is_a_second_birth_date() {
        let mut p = ChargePointProfile::bare(evse(), date!(2019 - 05 - 01));
        assert_eq!(p.effective_installation_date(), date!(2019 - 05 - 01));
        p.renovated_on = Some(date!(2026 - 03 - 01));
        assert_eq!(p.effective_installation_date(), date!(2026 - 03 - 01));
    }

    #[test]
    fn the_fifty_kilowatt_threshold_is_exact() {
        let mut p = ChargePointProfile::bare(evse(), date!(2026 - 01 - 01));
        p.rated_power_kw = Decimal::from(50);
        assert!(p.is_at_least_50_kw(), "50 kW is at least 50 kW");
        p.rated_power_kw = rust_decimal::Decimal::from_str_exact("49.999").unwrap();
        assert!(!p.is_at_least_50_kw());
    }

    #[test]
    fn a_bare_profile_is_non_compliant_on_purpose() {
        let p = ChargePointProfile::bare(evse(), date!(2026 - 01 - 01));
        assert_eq!(p.ad_hoc_payment, AdHocPayment::None);
        assert!(!p.data.datex2);
        assert!(!p.metering.signed_values);
    }
}
