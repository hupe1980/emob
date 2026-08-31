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
    /// The ad-hoc price must carry no roaming surcharge.
    AfirNoRoamingSurcharge,
    /// New DC points of at least 50 kW need a payment card reader.
    AfirCardReaderNewDc,
    /// TEN-T points of at least 50 kW must be retrofitted with one.
    AfirCardReaderTenTRetrofit,
    /// The price per kWh must be shown before the session starts.
    AfirPriceTransparency,
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
            Self::AfirNoRoamingSurcharge => "afir-no-roaming-surcharge",
            Self::AfirCardReaderNewDc => "afir-card-reader-new-dc",
            Self::AfirCardReaderTenTRetrofit => "afir-card-reader-ten-t-retrofit",
            Self::AfirPriceTransparency => "afir-price-transparency",
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

/// One regulatory duty: what it demands, of whom, from when, and who says so.
#[derive(Debug, Clone, Copy)]
pub struct Obligation {
    /// The stable identifier.
    pub id: ObligationId,
    /// A one-line statement of the duty.
    pub title: &'static str,
    /// The citation, in the form `specs/README.md` indexes.
    pub citation: &'static str,
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
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.ad_hoc_payment != AdHocPayment::None,
        remedy: "offer a contract-free payment path (card reader, or a web/QR flow)",
    },
    Obligation {
        id: ObligationId::AfirNoRoamingSurcharge,
        title: "The ad-hoc price must carry no roaming surcharge",
        citation: "[AFIR Art. 5(4)]",
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.ad_hoc_price_free_of_roaming_surcharge,
        remedy: "price the ad-hoc tariff without the roaming component",
    },
    Obligation {
        id: ObligationId::AfirPriceTransparency,
        title: "The price per kWh must be shown before the session starts",
        citation: "[AFIR Art. 5(4)]",
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| p.is_public(),
        satisfied: |p| p.price_displayed_before_session,
        remedy: "derive the displayed price from the tariff that rates the session",
    },
    Obligation {
        id: ObligationId::AfirCardReaderNewDc,
        title: "New DC points of at least 50 kW need a payment card reader",
        citation: "[AFIR Art. 5(2)]",
        applies_from: date!(2024 - 04 - 13),
        applies_until: None,
        applicable: |p| {
            p.is_public()
                && p.is_at_least_50_kw()
                && p.effective_installation_date() >= date!(2024 - 04 - 13)
        },
        satisfied: |p| p.ad_hoc_payment == AdHocPayment::CardReader,
        remedy: "fit a contactless payment terminal",
    },
    Obligation {
        id: ObligationId::AfirCardReaderTenTRetrofit,
        title: "TEN-T points of at least 50 kW must be retrofitted with a card reader",
        citation: "[AFIR Art. 5(2)]",
        applies_from: date!(2027 - 01 - 01),
        applies_until: None,
        applicable: |p| p.is_public() && p.is_at_least_50_kw() && p.on_ten_t,
        satisfied: |p| p.ad_hoc_payment == AdHocPayment::CardReader,
        remedy: "retrofit a contactless payment terminal before 01.01.2027",
    },
    Obligation {
        id: ObligationId::AfirNapData,
        title: "Static and dynamic data must reach the National Access Point, free of charge",
        citation: "[AFIR Art. 20]",
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
            let status = if !obligation.in_force_on(on) {
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
        point.v2g = V2gCommunication {
            pwm: true,
            iso15118_2: false,
            iso15118_20: false,
        };
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(150);

        // Untouched: the 2024 card-reader duty binds only new points.
        let before = assess(&point, date!(2026 - 06 - 01));
        assert_eq!(
            status_of(&before, ObligationId::AfirCardReaderNewDc),
            Status::NotApplicable
        );

        // Renovated in 2026: it counts as new from that day.
        point.renovated_on = Some(date!(2026 - 03 - 01));
        let after = assess(&point, date!(2026 - 06 - 01));
        assert_eq!(
            status_of(&after, ObligationId::AfirCardReaderNewDc),
            Status::Failing
        );
    }

    #[test]
    fn the_ten_t_retrofit_is_a_2027_duty_even_for_an_old_point() {
        let mut point = ChargePointProfile::bare(evse(), date!(2018 - 01 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(350);
        point.on_ten_t = true;
        point.ad_hoc_payment = AdHocPayment::DigitalOnly;

        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 12 - 31)),
                ObligationId::AfirCardReaderTenTRetrofit
            ),
            Status::NotYetInForce
        );
        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 01 - 01)),
                ObligationId::AfirCardReaderTenTRetrofit
            ),
            Status::Failing
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
        point.price_displayed_before_session = true;
        point.ad_hoc_price_free_of_roaming_surcharge = true;
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
        assert!(upcoming.contains(&ObligationId::AfirCardReaderTenTRetrofit));
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
