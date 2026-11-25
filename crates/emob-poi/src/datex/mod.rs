//! The DATEX II AFIR Recharging profile, version 01-00-00.
//!
//! `[AFIR Art. 20]` obliges an operator of publicly accessible recharging
//! points to make static and dynamic data available through the national access
//! point, free of charge and without discrimination. From **14 April 2026** the
//! German access point — the Mobilithek — requires that data in this profile
//! `[DATEX-II-Profil]`, which is DATEX II version 3 with AFIR extensions and is
//! proposed as a future CEN standard.
//!
//! Two publications, and they are not symmetrical:
//!
//! | | [`table`] | [`status`] |
//! |---|---|---|
//! | Carries | the infrastructure and its prices | references and live state |
//! | Changes when | somebody sends an engineer | somebody plugs in |
//! | AFIR | Art. 20(2)(a)–(b) | Art. 20(2)(f) |
//! | Wrapper | a bare `payload` | a `messageContainer` with exchange metadata |
//!
//! # Whose types these are
//!
//! The documents are built from [`datex2`]'s model of DATEX II version 3 and
//! written by its Mobilithek writer, which spells the profile's dialect and
//! refuses what the profile's schemas would reject, naming the member and the
//! rule. This module decides what the documents *say* — which facility, which
//! price, which literal — and nothing about how they are spelled.

pub mod status;
pub mod table;
pub mod wire;

pub use status::{Delivery, PointUpdate, PriceUpdate, StatusPublication};
pub use table::{InformationStatus, Published, Publisher, TablePublication};

use datex2::v3::afir_energy_infrastructure::{
    AuthenticationAndIdentificationEnum as Authentication, ConnectorTypeEnum, CurrentTypeEnum,
    PriceTypeEnum, RatePolicyEnum, RefillPointStatusEnum,
    VehicleToGridCommunicationTypeEnum as V2gType,
};
use emob_core::{AdHocPayment, ConnectorType, CurrentType, V2gCommunication};

use crate::rate::{PriceType, RatePolicy};
use crate::status::PointStatus;

/// How `[AFIR Art. 5(1)]`'s payment instruments are spelled in the profile.
///
/// `[DATEX-II-Profil]`'s `AuthenticationAndIdentificationEnum` is a list of
/// twenty; the mapping that matters is the one that keeps a driver's actual
/// options truthful:
///
/// - a card reader is `creditCard`, `debitCard` and `nfc` — the article names a
///   payment card reader **or** a contactless device, and a terminal that reads
///   cards contactlessly is all three;
/// - a QR code is `website`, because that is what it opens. It is deliberately
///   **not** `apps`: `[AFIR Art. 5(1)(c)]` requires payment without an app, and
///   publishing `apps` for a QR code claims the opposite of the duty it is
///   meant to satisfy.
#[must_use]
pub fn authentication_methods(payment: AdHocPayment) -> Vec<Authentication> {
    match payment {
        AdHocPayment::None => Vec::new(),
        AdHocPayment::QrCode => vec![Authentication::Website],
        AdHocPayment::CardReader => vec![
            Authentication::CreditCard,
            Authentication::DebitCard,
            Authentication::Nfc,
        ],
    }
}

/// The `VehicleToGridCommunicationTypeEnum` value for a point's generations.
///
/// The trap this function exists for: the profile defines its `iso15118`
/// literal as "Communication according to **ISO15118-20**"
/// `[DATEX-II-Profil Tab. A.130]` — the 2022 generation specifically, not the
/// 2016 one everybody calls ISO 15118. A point that speaks only ISO 15118-2 and
/// publishes `iso15118` is claiming readiness for exactly the duty AFIR and
/// `[DA-656 Anh. 2.1.3]` phase in from 2027, and a market that reads the feed
/// would count it as compliant.
///
/// So `iso15118` is written only for a point that really does -20. A -2-only
/// point is `other`, which is the truth: the enumeration has no literal for it.
#[must_use]
pub const fn v2g_type(v2g: V2gCommunication) -> V2gType {
    if v2g.iso15118_20 {
        V2gType::Iso15118
    } else if v2g.iso15118_2 || v2g.din70121 {
        // Neither has a literal. DIN SPEC 70121 is high-level communication
        // over PLC and most of the pre-2020 European DC fleet speaks it, so
        // `none` would be false about it in exactly the way `iso15118` would be
        // false about a -2 point.
        V2gType::Other
    } else {
        // PWM is signalling, not a vehicle-to-grid exchange. The enumeration's
        // own definition of `none` — "No communication between vehicle and the
        // grid" — is what a basic-signalling point does.
        V2gType::None
    }
}

/// A connector, as `[DATEX-II-Profil Tab. A.88]` names it.
///
/// `datex2` holds the CEN model's `domesticFType` and writes the profile's
/// `domesticF`.
pub(crate) fn connector_type(kind: ConnectorType) -> ConnectorTypeEnum {
    match kind {
        ConnectorType::Iec62196T2 => ConnectorTypeEnum::Iec62196T2,
        ConnectorType::Iec62196T2Combo => ConnectorTypeEnum::Iec62196T2COMBO,
        ConnectorType::Iec62196T1 => ConnectorTypeEnum::Iec62196T1,
        ConnectorType::Iec62196T1Combo => ConnectorTypeEnum::Iec62196T1COMBO,
        ConnectorType::Chademo => ConnectorTypeEnum::Chademo,
        ConnectorType::Iec62196T3A => ConnectorTypeEnum::Iec62196T3A,
        ConnectorType::DomesticF => ConnectorTypeEnum::DomesticFType,
        ConnectorType::Cee5 => ConnectorTypeEnum::Cee5,
        // `ConnectorType` is non-exhaustive; a variant added upstream without a
        // line here is published as the profile's own escape, not guessed at.
        _ => ConnectorTypeEnum::Other,
    }
}

pub(crate) const fn current_type(current: CurrentType) -> CurrentTypeEnum {
    match current {
        CurrentType::Ac => CurrentTypeEnum::Ac,
        CurrentType::Dc => CurrentTypeEnum::Dc,
    }
}

/// `[DATEX-II-Profil Tab. A.116]`.
pub(crate) const fn price_type(price: PriceType) -> PriceTypeEnum {
    match price {
        PriceType::BasePrice => PriceTypeEnum::BasePrice,
        PriceType::FlatRate => PriceTypeEnum::FlatRate,
        PriceType::Free => PriceTypeEnum::Free,
        PriceType::Other => PriceTypeEnum::Other,
        PriceType::PricePerKwh => PriceTypeEnum::PricePerKWh,
        PriceType::PricePerMinute => PriceTypeEnum::PricePerMinute,
    }
}

/// `[DATEX-II-Profil Tab. A.118]`.
pub(crate) const fn rate_policy(policy: RatePolicy) -> RatePolicyEnum {
    match policy {
        RatePolicy::AdHoc => RatePolicyEnum::AdHoc,
        RatePolicy::Contract => RatePolicyEnum::Contract,
    }
}

/// `[DATEX-II-Profil Tab. B.45]`.
pub(crate) const fn point_status(status: PointStatus) -> RefillPointStatusEnum {
    match status {
        PointStatus::Unknown => RefillPointStatusEnum::Unknown,
        PointStatus::Available => RefillPointStatusEnum::Available,
        PointStatus::Blocked => RefillPointStatusEnum::Blocked,
        PointStatus::Charging => RefillPointStatusEnum::Charging,
        PointStatus::Faulted => RefillPointStatusEnum::Faulted,
        PointStatus::Inoperative => RefillPointStatusEnum::Inoperative,
        PointStatus::Occupied => RefillPointStatusEnum::Occupied,
        PointStatus::OutOfOrder => RefillPointStatusEnum::OutOfOrder,
        PointStatus::OutOfStock => RefillPointStatusEnum::OutOfStock,
        PointStatus::Planned => RefillPointStatusEnum::Planned,
        PointStatus::Removed => RefillPointStatusEnum::Removed,
        PointStatus::Reserved => RefillPointStatusEnum::Reserved,
        PointStatus::Unavailable => RefillPointStatusEnum::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_that_speaks_only_the_2016_generation_does_not_claim_the_2027_one() {
        // The profile's `iso15118` means -20. Publishing it for a -2 point
        // claims compliance with the duty that is being phased in.
        let old = V2gCommunication {
            pwm: true,
            din70121: false,
            iso15118_2: true,
            iso15118_20: false,
        };
        assert_eq!(v2g_type(old), V2gType::Other);
        assert_eq!(
            v2g_type(V2gCommunication::both_generations()),
            V2gType::Iso15118
        );
        assert_eq!(v2g_type(V2gCommunication::pwm_only()), V2gType::None);

        // …and `none` says "No communication between vehicle and the grid",
        // which is false about a DIN SPEC 70121 charger: it talks to the car
        // over PLC, it just does not do it in a document AFIR names.
        assert_eq!(v2g_type(V2gCommunication::din_only()), V2gType::Other);
    }

    #[test]
    fn a_qr_code_is_a_website_and_never_an_app() {
        // `[AFIR Art. 5(1)(c)]` is satisfied by a device allowing secure
        // payment *without* an app. Publishing `apps` would advertise the one
        // thing the paragraph rules out.
        assert_eq!(
            authentication_methods(AdHocPayment::QrCode),
            [Authentication::Website]
        );
        assert!(authentication_methods(AdHocPayment::None).is_empty());
        assert!(
            authentication_methods(AdHocPayment::CardReader).contains(&Authentication::Nfc),
            "a contactless reader is a contactless reader"
        );
    }
}
