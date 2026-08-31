//! The obligation calendar: regulatory duties as dated, cited, executable data.
//!
//! # Why this exists
//!
//! "Are we AFIR-ready for 2027?" is normally a consulting engagement. Here it
//! is a query:
//!
//! ```
//! use emob_core::obligation::{assess, Verdict};
//! use emob_core::station::ChargePointProfile;
//! use time::macros::date;
//!
//! let point = ChargePointProfile::bare("DE*AB7*E840*6487".parse()?, date!(2026-06-01));
//! let report = assess(&point, date!(2027-01-01));
//!
//! // Everything it fails, each naming the document that says so.
//! for finding in report.failing() {
//!     println!("{}  {}", finding.obligation.citation, finding.obligation.title);
//! }
//! assert!(report.failing().count() > 0);
//! assert_eq!(report.verdict(), Verdict::Failing);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # The three properties that make this trustworthy
//!
//! **Every duty carries its citation.** `cargo xtask check-citations` fails the
//! build if a citation names a document `specs/README.md` does not index — so a
//! rule can always be followed to a file, a section and a retrieval URL. A rule
//! citing a Verordnung nobody can produce is indistinguishable from one
//! somebody invented.
//!
//! **Every duty carries its window.** `applies_from`, and `applies_until` for
//! the ones that end. Asking the calendar about a date is the only way to use
//! it, so a duty cannot be applied a year before it exists — the single most
//! common compliance-code bug, and the reason the LSV 2016 → LSV 2026
//! transition is representable at all.
//!
//! **Applicability and satisfaction are separate questions.** A private point
//! is not *failing* the ad-hoc payment duty; the duty does not bind it. Merging
//! the two produces reports that cry wolf, which is how compliance dashboards
//! come to be ignored.

use time::Date;
use time::macros::date;

use crate::station::{AdHocPayment, ChargePointProfile};

/// A stable identifier for one obligation.
///
/// Stable across releases: it appears in reports, in evidence exports and in
/// operator tickets. Renaming one is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[non_exhaustive]
pub enum ObligationId {
    /// Ad-hoc charging must be possible without a contract.
    AfirAdHocAccess,
    /// A point that charges for the service needs a widely used payment
    /// instrument.
    AfirPaymentInstrument,
    /// Points of at least 50 kW on TEN-T or a safe and secure parking area must
    /// be retrofitted with a card reader or contactless device.
    AfirPaymentInstrumentRetrofit,
    /// Where automatic authentication is offered, the right not to use it must
    /// be shown and offered.
    AfirAutomaticAuthenticationOptOut,
    /// At 50 kW and above the ad-hoc price must be based on a price per kWh.
    AfirEnergyBasedAdHocPrice,
    /// At 50 kW and above the price per kWh and any occupancy fee must be shown
    /// at the station.
    AfirPriceShownAtStation,
    /// Below 50 kW every price component must be available, in the prescribed
    /// order.
    AfirPriceComponentsInOrder,
    /// A mobility service provider must disclose every price component,
    /// e-roaming costs included, and may not surcharge cross-border roaming.
    AfirMspPriceDisclosure,
    /// Static and dynamic data must reach the National Access Point.
    AfirNapData,
    /// …in the DATEX II Recharging profile.
    AfirDatex2,
    /// New or renovated public points must implement EN ISO 15118-2.
    Da656Iso15118Dash2,
    /// From 2027, EN ISO 15118-20 as well.
    Da656Iso15118Dash20,
    /// A Plug & Charge point must support both generations.
    Da656PlugAndChargeBothGenerations,
    /// Public points must be entered in the register.
    Lsv2026Registration,
    /// Energy billing requires a conformity-assessed meter.
    EichrechtConformityAssessedMeter,
    /// …and measured values the customer can verify.
    EichrechtVerifiableValues,
    /// THG-Quote requires a register entry and third-party access.
    ThgEligibility,
}

impl ObligationId {
    /// The kebab-case slug used in reports and exports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AfirAdHocAccess => "afir-ad-hoc-access",
            Self::AfirPaymentInstrument => "afir-payment-instrument",
            Self::AfirPaymentInstrumentRetrofit => "afir-payment-instrument-retrofit",
            Self::AfirAutomaticAuthenticationOptOut => "afir-automatic-authentication-opt-out",
            Self::AfirEnergyBasedAdHocPrice => "afir-energy-based-ad-hoc-price",
            Self::AfirPriceShownAtStation => "afir-price-shown-at-station",
            Self::AfirPriceComponentsInOrder => "afir-price-components-in-order",
            Self::AfirMspPriceDisclosure => "afir-msp-price-disclosure",
            Self::AfirNapData => "afir-nap-data",
            Self::AfirDatex2 => "afir-datex2",
            Self::Da656Iso15118Dash2 => "da656-iso15118-2",
            Self::Da656Iso15118Dash20 => "da656-iso15118-20",
            Self::Da656PlugAndChargeBothGenerations => "da656-pnc-both-generations",
            Self::Lsv2026Registration => "lsv2026-registration",
            Self::EichrechtConformityAssessedMeter => "eichrecht-conformity-assessed-meter",
            Self::EichrechtVerifiableValues => "eichrecht-verifiable-values",
            Self::ThgEligibility => "thg-eligibility",
        }
    }
}

impl core::fmt::Display for ObligationId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who a duty binds.
///
/// Not every obligation in European charging law is about a charge point.
/// `[AFIR Art. 5(5)]` binds the *mobility service provider*; NIS2 and the Cyber
/// Resilience Act bind the operator as an undertaking. Judging those against a
/// [`ChargePointProfile`] would be a category error, and leaving them out of
/// the calendar entirely would let them be forgotten — so they are here, and
/// they report [`Status::DifferentScope`] when a charge point is what was
/// asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum Scope {
    /// The duty is a property of one charge point.
    ChargePoint,
    /// The duty binds a mobility service provider.
    MobilityServiceProvider,
}

/// One regulatory duty: what it demands, of whom, from when, and who says so.
#[derive(Debug, Clone, Copy)]
pub struct Obligation {
    /// The stable identifier.
    pub id: ObligationId,
    /// A one-line statement of the duty.
    pub title: &'static str,
    /// The citation, in the form `specs/README.md` indexes.
    pub citation: &'static str,
    /// Who the duty binds.
    pub scope: Scope,
    /// The first day the duty binds.
    pub applies_from: Date,
    /// The last day it binds, for duties that are superseded.
    pub applies_until: Option<Date>,
    /// Whether the duty binds this point at all.
    applicable: fn(&ChargePointProfile) -> bool,
    /// Whether the point meets it.
    satisfied: fn(&ChargePointProfile) -> bool,
    /// What to do about it when it is not met.
    pub remedy: &'static str,
}

impl Obligation {
    /// `true` when the duty is in force on `on`.
    #[must_use]
    pub fn in_force_on(&self, on: Date) -> bool {
        on >= self.applies_from && self.applies_until.is_none_or(|until| on <= until)
    }
}

/// How one obligation came out for one point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The duty binds this point and it is met.
    Satisfied,
    /// The duty binds this point and it is not met.
    Failing,
    /// The duty does not bind this point — wrong accessibility, wrong power
    /// class, built before the cut-off.
    NotApplicable,
    /// The duty is not yet in force, or no longer is, on the date asked about.
    NotYetInForce,
    /// The duty binds somebody other than a charge point, so a charge-point
    /// profile cannot answer it.
    DifferentScope,
}

/// One obligation, judged.
#[derive(Debug, Clone, Copy)]
pub struct Finding {
    /// The duty.
    pub obligation: Obligation,
    /// How it came out.
    pub status: Status,
}

/// The overall answer for a point on a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every duty that binds this point is met.
    Compliant,
    /// At least one is not.
    Failing,
}

/// Every obligation, judged, for one point on one date.
#[derive(Debug, Clone)]
pub struct Assessment {
    /// Which point.
    pub evse_id: crate::ids::EvseId,
    /// Which date the question was asked about.
    pub on: Date,
    /// One finding per obligation in the calendar.
    pub findings: Vec<Finding>,
}

impl Assessment {
    /// The findings that are failing.
    pub fn failing(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.status == Status::Failing)
    }

    /// The findings that are satisfied.
    pub fn satisfied(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == Status::Satisfied)
    }

    /// Compliant only when nothing that binds this point is failing.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if self.failing().next().is_some() {
            Verdict::Failing
        } else {
            Verdict::Compliant
        }
    }
}

/// The calendar itself.
///
/// A `const` table so the whole rule set is visible in one screen and reviewed
/// against the documents it cites, rather than scattered across the services
/// that enforce it.
pub const CALENDAR: &[Obligation] = &[
    Obligation {
        id: ObligationId::AfirAdHocAccess,
        title: "Ad-hoc charging must be possible without a contract",
        citation: "[AFIR Art. 5(1)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.ad_hoc_payment != AdHocPayment::None,
        remedy: "offer a contract-free payment path at the point",
    },
    Obligation {
        id: ObligationId::AfirPaymentInstrument,
        title: "A point deployed from 13.04.2024 needs a payment instrument widely used in the Union",
        citation: "[AFIR Art. 5(1)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        // Three conditions, and the third is the one implementations miss: the
        // whole régime "shall not apply to publicly accessible recharging
        // points that do not require payment for the recharging service". A
        // free municipal charger owes no card reader.
        applicable: |p| {
            p.is_public()
                && p.requires_payment
                && p.effective_installation_date() >= date!(2024 - 04 - 13)
        },
        // A QR-code device satisfies (c) only below 50 kW; at or above it, only
        // a card reader or contactless device does.
        satisfied: |p| p.ad_hoc_payment.satisfies_afir_at(p.rated_power_kw),
        remedy: "fit a card reader or contactless device (a QR flow only qualifies below 50 kW)",
    },
    Obligation {
        id: ObligationId::AfirPaymentInstrumentRetrofit,
        title: "Points of at least 50 kW on TEN-T or a safe and secure parking area must be retrofitted",
        citation: "[AFIR Art. 5(1)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2027 - 01 - 01),
        applies_until: None,
        // Explicitly reaches points deployed *before* 13.04.2024 — the whole
        // point of the paragraph — and covers safe and secure parking areas as
        // well as the TEN-T road network.
        applicable: |p| {
            p.is_public()
                && p.requires_payment
                && p.is_at_least_50_kw()
                && (p.on_ten_t || p.on_safe_secure_parking)
        },
        satisfied: |p| p.ad_hoc_payment == AdHocPayment::CardReader,
        remedy: "retrofit a card reader or contactless device before 01.01.2027",
    },
    Obligation {
        id: ObligationId::AfirAutomaticAuthenticationOptOut,
        title: "Where automatic authentication is offered, the right not to use it must be shown",
        citation: "[AFIR Art. 5(2)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| p.is_public() && p.offers_automatic_authentication,
        satisfied: |p| p.automatic_authentication_opt_out_offered,
        remedy: "show the ad-hoc and contract alternatives clearly, and offer them conveniently",
    },
    Obligation {
        id: ObligationId::AfirEnergyBasedAdHocPrice,
        title: "At 50 kW and above the ad-hoc price must be based on a price per kWh",
        citation: "[AFIR Art. 5(4)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| {
            p.is_public()
                && p.requires_payment
                && p.is_at_least_50_kw()
                && p.effective_installation_date() >= date!(2024 - 04 - 13)
        },
        // A purely per-minute tariff on a fast charger is unlawful. An
        // occupancy fee per minute is permitted *in addition* to the kWh price,
        // never instead of it.
        satisfied: |p| p.price_transparency.energy_based,
        remedy: "price the ad-hoc tariff per kWh; an occupancy fee per minute may only be added to it",
    },
    Obligation {
        id: ObligationId::AfirPriceShownAtStation,
        title: "At 50 kW and above the price per kWh and any occupancy fee must be shown at the station",
        citation: "[AFIR Art. 5(4)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| {
            p.is_public()
                && p.requires_payment
                && p.is_at_least_50_kw()
                && p.effective_installation_date() >= date!(2024 - 04 - 13)
        },
        satisfied: |p| p.price_transparency.shown_at_station,
        remedy: "show the price before the session starts, derived from the tariff that rates it",
    },
    Obligation {
        id: ObligationId::AfirPriceComponentsInOrder,
        title: "Below 50 kW every price component must be available, in the prescribed order",
        citation: "[AFIR Art. 5(4)]",
        scope: Scope::ChargePoint,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        // The article prescribes the *order*: per kWh, per minute, per session,
        // then anything else. An unordered list of the right numbers does not
        // satisfy it.
        applicable: |p| p.is_public() && p.requires_payment && !p.is_at_least_50_kw(),
        satisfied: |p| p.price_transparency.components_in_prescribed_order,
        remedy: "present the components as kWh, then minute, then session, then the rest",
    },
    Obligation {
        id: ObligationId::AfirMspPriceDisclosure,
        title: "A mobility service provider must disclose every price component, e-roaming costs included, and may not surcharge cross-border roaming",
        citation: "[AFIR Art. 5(5)]",
        scope: Scope::MobilityServiceProvider,
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        // Binds the provider, not the point. Kept in the calendar so it cannot
        // be forgotten, and reported as out of scope when a charge point is
        // what was asked about.
        applicable: |_| false,
        satisfied: |_| false,
        remedy: "disclose all components before the session, and never surcharge cross-border e-roaming",
    },
    Obligation {
        id: ObligationId::AfirNapData,
        title: "Static and dynamic data must reach the National Access Point, free of charge",
        citation: "[AFIR Art. 20]",
        scope: Scope::ChargePoint,
        applies_from: date!(2025 - 04 - 14),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.data.national_access_point,
        remedy: "publish the point through poid to the Mobilithek",
    },
    Obligation {
        id: ObligationId::AfirDatex2,
        title: "National Access Point data must be delivered in the DATEX II Recharging profile",
        citation: "[AFIR Art. 20]",
        scope: Scope::ChargePoint,
        applies_from: date!(2026 - 04 - 14),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.data.datex2,
        remedy: "switch the NAP feed to the DATEX II Recharging profile",
    },
    Obligation {
        id: ObligationId::Da656Iso15118Dash2,
        title: "Newly installed or renovated public points must implement EN ISO 15118-2",
        citation: "[DA-656]",
        scope: Scope::ChargePoint,
        applies_from: date!(2026 - 01 - 08),
        applies_until: None,
        // The exemption is the interesting half: an existing PWM-only point is
        // explicitly out of scope. Modelling it as "not applicable" rather than
        // as a pass keeps the two distinguishable in a report.
        applicable: |p| {
            p.is_public()
                && p.effective_installation_date() >= date!(2026 - 01 - 08)
                && !p.is_legacy_pwm_only()
        },
        satisfied: |p| p.v2g.iso15118_2,
        remedy: "deploy firmware implementing EN ISO 15118-1…-5",
    },
    Obligation {
        id: ObligationId::Da656Iso15118Dash20,
        title: "Public and private Mode 3/4 points must implement EN ISO 15118-20",
        citation: "[DA-656]",
        scope: Scope::ChargePoint,
        applies_from: date!(2027 - 01 - 01),
        applies_until: None,
        // This is the one duty that reaches private points too.
        applicable: |p| !p.is_legacy_pwm_only(),
        satisfied: |p| p.v2g.iso15118_20,
        remedy: "TLS 1.3 and the larger certificates of -20 usually mean a hardware refresh",
    },
    Obligation {
        id: ObligationId::Da656PlugAndChargeBothGenerations,
        title: "A Plug & Charge point must support both EN ISO 15118-2 and -20",
        citation: "[DA-656]",
        scope: Scope::ChargePoint,
        applies_from: date!(2027 - 01 - 01),
        applies_until: None,
        applicable: |p| p.offers_plug_and_charge,
        satisfied: |p| p.v2g.iso15118_2 && p.v2g.iso15118_20,
        remedy: "a PnC point may not drop -2: vehicles on both generations must be served",
    },
    Obligation {
        id: ObligationId::Lsv2026Registration,
        title: "Public points must be registered and their operation reported",
        citation: "[LSV26]",
        scope: Scope::ChargePoint,
        applies_from: date!(2026 - 01 - 01),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.data.registered,
        remedy: "report commissioning through the registry state machine in poid",
    },
    Obligation {
        id: ObligationId::EichrechtConformityAssessedMeter,
        title: "Billing by energy requires a conformity-assessed meter",
        citation: "[MessEG §33]",
        scope: Scope::ChargePoint,
        applies_from: date!(2019 - 04 - 01),
        applies_until: None,
        applicable: |p| p.metering.bills_by_energy,
        satisfied: |p| p.metering.mid_conformity_assessed,
        remedy: "a point without an assessed meter may not bill by kWh at all",
    },
    Obligation {
        id: ObligationId::EichrechtVerifiableValues,
        title: "The customer must be able to verify the billed measured value",
        citation: "[PTB-A 50.7]",
        scope: Scope::ChargePoint,
        applies_from: date!(2019 - 04 - 01),
        applies_until: None,
        applicable: |p| p.metering.bills_by_energy,
        satisfied: |p| p.metering.signed_values,
        remedy: "emit OCMF-signed values and retain them with the session",
    },
    Obligation {
        id: ObligationId::ThgEligibility,
        title: "THG-Quote requires a register entry and access for third parties",
        citation: "[38k]",
        scope: Scope::ChargePoint,
        applies_from: date!(2022 - 01 - 01),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.data.registered && p.open_to_third_parties,
        remedy: "register the point and keep it open to third parties, or forgo the quota",
    },
];

/// Judge one point against the whole calendar on one date.
#[must_use]
pub fn assess(point: &ChargePointProfile, on: Date) -> Assessment {
    let findings = CALENDAR
        .iter()
        .map(|obligation| {
            let status = if obligation.scope != Scope::ChargePoint {
                Status::DifferentScope
            } else if !obligation.in_force_on(on) {
                Status::NotYetInForce
            } else if !(obligation.applicable)(point) {
                Status::NotApplicable
            } else if (obligation.satisfied)(point) {
                Status::Satisfied
            } else {
                Status::Failing
            };
            Finding {
                obligation: *obligation,
                status,
            }
        })
        .collect();

    Assessment {
        evse_id: point.evse_id.clone(),
        on,
        findings,
    }
}

/// Every obligation in force on a date, whatever it binds.
///
/// The planning query: what changes between today and a date, so a fleet
/// programme can be built from it.
pub fn in_force_on(on: Date) -> impl Iterator<Item = &'static Obligation> {
    CALENDAR.iter().filter(move |o| o.in_force_on(on))
}

/// Every obligation that starts binding strictly between two dates.
///
/// ```
/// use emob_core::obligation::starting_between;
/// use time::macros::date;
///
/// // What lands on the fleet in 2027?
/// let upcoming: Vec<_> = starting_between(date!(2026-09-01), date!(2027-12-31))
///     .map(|o| o.id)
///     .collect();
/// assert!(!upcoming.is_empty());
/// ```
pub fn starting_between(from: Date, to: Date) -> impl Iterator<Item = &'static Obligation> {
    CALENDAR
        .iter()
        .filter(move |o| o.applies_from > from && o.applies_from <= to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::EvseId;
    use crate::station::{AdHocPayment, ChargePointProfile, CurrentType, V2gCommunication};
    use rust_decimal::Decimal;

    fn evse() -> EvseId {
        "DE*AB7*E840*6487".parse().unwrap()
    }

    fn status_of(assessment: &Assessment, id: ObligationId) -> Status {
        assessment
            .findings
            .iter()
            .find(|f| f.obligation.id == id)
            .expect("every obligation is judged")
            .status
    }

    #[test]
    fn a_duty_cannot_bind_before_it_exists() {
        let point = ChargePointProfile::bare(evse(), date!(2020 - 01 - 01));
        // DATEX II starts 14.04.2026. On 13.04.2026 it is not yet in force.
        let before = assess(&point, date!(2026 - 04 - 13));
        assert_eq!(
            status_of(&before, ObligationId::AfirDatex2),
            Status::NotYetInForce
        );
        let on_the_day = assess(&point, date!(2026 - 04 - 14));
        assert_eq!(
            status_of(&on_the_day, ObligationId::AfirDatex2),
            Status::Failing
        );
    }

    #[test]
    fn a_private_point_is_not_failing_the_public_duties() {
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.accessibility = crate::station::Accessibility::Private;
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirAdHocAccess),
            Status::NotApplicable,
            "a depot is not failing the ad-hoc duty; the duty does not bind it"
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirNapData),
            Status::NotApplicable
        );
    }

    #[test]
    fn the_pwm_exemption_is_not_applicable_rather_than_satisfied() {
        // An existing PWM-only point is explicitly out of scope of DA-656.
        let mut legacy = ChargePointProfile::bare(evse(), date!(2019 - 01 - 01));
        legacy.v2g = V2gCommunication::pwm_only();
        let report = assess(&legacy, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Da656Iso15118Dash20),
            Status::NotApplicable
        );

        // …but a point that does high-level communication is in scope.
        let mut modern = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        modern.v2g = V2gCommunication {
            pwm: true,
            iso15118_2: true,
            iso15118_20: false,
        };
        let report = assess(&modern, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Da656Iso15118Dash20),
            Status::Failing
        );
    }

    #[test]
    fn a_renovation_pulls_an_old_point_into_a_new_duty() {
        let mut point = ChargePointProfile::bare(evse(), date!(2019 - 01 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(150);

        // Untouched: the payment-instrument duty binds points deployed from
        // 13.04.2024.
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 06 - 01)),
                ObligationId::AfirPaymentInstrument
            ),
            Status::NotApplicable
        );

        // Renovated in 2026: it counts as new from that day.
        point.renovated_on = Some(date!(2026 - 03 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 06 - 01)),
                ObligationId::AfirPaymentInstrument
            ),
            Status::Failing
        );
    }

    #[test]
    fn a_free_charge_point_owes_no_payment_instrument() {
        // "shall not apply to publicly accessible recharging points that do not
        // require payment for the recharging service" [AFIR Art. 5(1)].
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.rated_power_kw = Decimal::from(150);
        point.requires_payment = false;

        let report = assess(&point, date!(2027 - 06 - 01));
        for duty in [
            ObligationId::AfirPaymentInstrument,
            ObligationId::AfirPaymentInstrumentRetrofit,
            ObligationId::AfirEnergyBasedAdHocPrice,
            ObligationId::AfirPriceShownAtStation,
        ] {
            assert_eq!(
                status_of(&report, duty),
                Status::NotApplicable,
                "{duty} must not bind a point that charges nothing"
            );
        }
    }

    #[test]
    fn a_qr_code_satisfies_the_duty_below_fifty_kilowatts_only() {
        // The same equipment on two posts in one car park.
        let mut slow = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        slow.rated_power_kw = Decimal::from(22);
        slow.ad_hoc_payment = AdHocPayment::QrCode;
        assert_eq!(
            status_of(
                &assess(&slow, date!(2026 - 09 - 01)),
                ObligationId::AfirPaymentInstrument
            ),
            Status::Satisfied
        );

        let mut fast = slow.clone();
        fast.rated_power_kw = Decimal::from(150);
        assert_eq!(
            status_of(
                &assess(&fast, date!(2026 - 09 - 01)),
                ObligationId::AfirPaymentInstrument
            ),
            Status::Failing,
            "AFIR Art. 5(1)(c) allows the QR option only below 50 kW"
        );
    }

    #[test]
    fn the_2027_retrofit_reaches_safe_parking_as_well_as_ten_t() {
        let mut point = ChargePointProfile::bare(evse(), date!(2018 - 01 - 01));
        point.rated_power_kw = Decimal::from(350);
        point.ad_hoc_payment = AdHocPayment::QrCode;

        // Neither TEN-T nor a parking area: out of scope.
        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 06 - 01)),
                ObligationId::AfirPaymentInstrumentRetrofit
            ),
            Status::NotApplicable
        );

        // A safe and secure parking area away from the corridor is in scope
        // just as a motorway service station is — easy to miss, and named in
        // the article.
        point.on_safe_secure_parking = true;
        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 06 - 01)),
                ObligationId::AfirPaymentInstrumentRetrofit
            ),
            Status::Failing
        );

        // …and it reaches points deployed long before 13.04.2024, which is the
        // whole purpose of the paragraph.
        assert_eq!(point.commissioned_on, date!(2018 - 01 - 01));
    }

    #[test]
    fn a_fast_charger_may_not_price_by_the_minute_alone() {
        // "the ad hoc price charged by the operator shall be based on the price
        // per kWh" [AFIR Art. 5(4)]. Almost nothing checks this.
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.rated_power_kw = Decimal::from(150);
        point.price_transparency.energy_based = false;

        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirEnergyBasedAdHocPrice
            ),
            Status::Failing
        );

        point.price_transparency.energy_based = true;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirEnergyBasedAdHocPrice
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn the_two_price_display_duties_are_split_at_fifty_kilowatts() {
        let mut fast = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        fast.rated_power_kw = Decimal::from(150);
        let report = assess(&fast, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirPriceShownAtStation),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirPriceComponentsInOrder),
            Status::NotApplicable,
            "the ordered-components duty is the sub-50 kW one"
        );

        let mut slow = fast.clone();
        slow.rated_power_kw = Decimal::from(22);
        let report = assess(&slow, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirPriceComponentsInOrder),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirPriceShownAtStation),
            Status::NotApplicable
        );
    }

    #[test]
    fn automatic_authentication_carries_an_opt_out_duty() {
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirAutomaticAuthenticationOptOut
            ),
            Status::NotApplicable
        );

        point.offers_automatic_authentication = true;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirAutomaticAuthenticationOptOut
            ),
            Status::Failing
        );

        point.automatic_authentication_opt_out_offered = true;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirAutomaticAuthenticationOptOut
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn a_provider_duty_is_out_of_scope_for_a_charge_point() {
        // Art. 5(5) binds the mobility service provider. Judging it against a
        // charge point would be a category error; omitting it from the calendar
        // would let it be forgotten.
        let point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirMspPriceDisclosure
            ),
            Status::DifferentScope
        );
    }

    #[test]
    fn a_point_that_does_not_bill_by_energy_is_outside_eichrecht() {
        let point = ChargePointProfile::bare(evse(), date!(2026 - 01 - 01));
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::EichrechtVerifiableValues),
            Status::NotApplicable
        );
    }

    #[test]
    fn a_fully_equipped_point_is_compliant() {
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(300);
        point.on_ten_t = true;
        point.ad_hoc_payment = AdHocPayment::CardReader;
        point.price_transparency = crate::station::PriceTransparency {
            energy_based: true,
            shown_at_station: true,
            components_in_prescribed_order: true,
        };
        point.v2g = V2gCommunication::both_generations();
        point.offers_plug_and_charge = true;
        point.data = crate::station::DataPublication {
            national_access_point: true,
            datex2: true,
            registered: true,
        };
        point.metering = crate::station::MeteringPosture {
            mid_conformity_assessed: true,
            signed_values: true,
            bills_by_energy: true,
        };
        point.open_to_third_parties = true;

        let report = assess(&point, date!(2027 - 06 - 01));
        assert_eq!(
            report.verdict(),
            Verdict::Compliant,
            "failing: {:?}",
            report
                .failing()
                .map(|f| f.obligation.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_2027_wave_is_visible_in_advance() {
        let upcoming: Vec<_> = starting_between(date!(2026 - 09 - 01), date!(2027 - 12 - 31))
            .map(|o| o.id)
            .collect();
        assert!(upcoming.contains(&ObligationId::Da656Iso15118Dash20));
        assert!(upcoming.contains(&ObligationId::AfirPaymentInstrumentRetrofit));
        assert!(
            !upcoming.contains(&ObligationId::AfirDatex2),
            "DATEX II already started in April 2026"
        );
    }

    #[test]
    fn every_obligation_id_has_a_unique_slug() {
        let mut slugs: Vec<_> = CALENDAR.iter().map(|o| o.id.as_str()).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(before, slugs.len(), "obligation slugs must be unique");
    }

    #[test]
    fn every_obligation_carries_a_citation_and_a_remedy() {
        for o in CALENDAR {
            assert!(
                o.citation.starts_with('[') && o.citation.ends_with(']'),
                "{}: a citation is written [Doc §x]",
                o.id
            );
            assert!(!o.remedy.is_empty(), "{}: needs a remedy", o.id);
            assert!(!o.title.is_empty(), "{}: needs a title", o.id);
        }
    }
}
