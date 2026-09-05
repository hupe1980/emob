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
//! # The four properties that make this trustworthy
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
//! common compliance-code bug.
//!
//! **Applicability and satisfaction are separate questions.** A private point
//! is not *failing* the ad-hoc payment duty; the duty does not bind it. Merging
//! the two produces reports that cry wolf, which is how compliance dashboards
//! come to be ignored.
//!
//! **Every duty is assessable against something.** A duty that binds a
//! mobility service provider is judged against a [`ProviderProfile`], not
//! stubbed out against a charge point. `[AFIR Art. 5(5)]` is a real duty with
//! a real test, and the operator wearing both hats is the normal German case.
//!
//! # The dates the text actually gives
//!
//! Three wordings appear in Article 5 and they are not interchangeable, which
//! is where most implementations go wrong:
//!
//! | Wording | Reads | Duties |
//! |---|---|---|
//! | "deployed from 13 April 2024" | [`ChargePointProfile::commissioned_on`] | 5(1), 5(4)¶1–2 |
//! | "built after … **or renovated after** …" | both, with different dates | 5(8) |
//! | "installed **or renovated** from …" | [`ChargePointProfile::installed_or_renovated_on`] | `[DA-656 Anh. 2.1]` |
//! | "nach der Inbetriebnahme", restarted when a point becomes public | [`ChargePointProfile::notifiable_commissioning_date`] | `[LSV26 §4]` |
//!
//! A renovation is not a deployment. Treating it as one drags untouched 2019
//! hardware into duties written for new equipment.
//!
//! [`ProviderProfile`]: crate::station::ProviderProfile

use time::Date;
use time::macros::date;

use crate::station::{
    AdHocPayment, AdditionalPrices, ChargePointProfile, ChargingMode, CurrentType,
    EnergyMeasurementPoint, Ownership, ProviderProfile, Registration, UndertakingProfile,
};

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
    /// Prices must not discriminate between end users and providers, or
    /// between providers.
    AfirNonDiscriminatoryPricing,
    /// At 50 kW and above the ad-hoc price must be based on a price per kWh.
    AfirEnergyBasedAdHocPrice,
    /// At 50 kW and above the price per kWh and any occupancy fee must be shown
    /// at the station.
    AfirPriceShownAtStation,
    /// Below 50 kW every price component must be available, in the prescribed
    /// order.
    AfirPriceComponentsInOrder,
    /// A mobility service provider must disclose every price component before
    /// the session, e-roaming costs included.
    AfirMspPriceDisclosure,
    /// A mobility service provider may not surcharge cross-border e-roaming.
    AfirMspNoCrossBorderSurcharge,
    /// Every publicly accessible point must be digitally connected.
    AfirDigitallyConnected,
    /// New and renovated points must be capable of smart recharging.
    AfirSmartRecharging,
    /// Every publicly accessible DC point must have a fixed cable.
    AfirFixedCableOnDc,
    /// A third-party owner must supply hardware the operator can comply on.
    AfirOwnerEnablesCompliance,
    /// A point must carry the interface Annex II names for its current and
    /// power class.
    AfirAnnexIiConnector,
    /// Static data must be available, free of charge.
    AfirStaticData,
    /// Dynamic data must be available, free of charge.
    AfirDynamicData,
    /// A free and unrestricted API must be published to the national access
    /// point.
    AfirDataApi,
    /// …and the German feed must speak the DATEX II Recharging profile.
    AfirDatex2,
    /// New or renovated public points must implement EN ISO 15118-1…-5.
    Da656Iso15118Dash2,
    /// From 2027, new or renovated public points must implement EN ISO 15118-20.
    Da656Iso15118Dash20Public,
    /// From 2027, new or renovated private Mode 3/4 points must too.
    Da656Iso15118Dash20Private,
    /// A point offering automatic authentication must support both generations.
    Da656AutomaticAuthenticationBothGenerations,
    /// Every point must meet the applicable technical requirements.
    Lsv2026TechnicalRequirements,
    /// …and the operator must be able to prove it on request.
    Lsv2026TechnicalEvidence,
    /// Commissioning must be notified to the regulator within two weeks.
    Lsv2026CommissioningNotice,
    /// Decommissioning must be notified without undue delay.
    Lsv2026DecommissioningNotice,
    /// A change of operator must be notified by **both** operators.
    Lsv2026OperatorChangeNotice,
    /// Energy billing requires a conformity-assessed meter.
    EichrechtConformityAssessedMeter,
    /// …and measured values the customer can verify.
    EichrechtVerifiableValues,
    /// …on a meter whose verification period has not expired.
    EichrechtVerificationPeriod,
    /// …with the records of every intervention on it kept.
    EichrechtInterventionRecords,
    /// Whoever *uses* measured values owes a confirmation from whoever operates
    /// the meter.
    EichrechtValueUserConfirmation,
    /// AC metering in a DC station is only permitted on legacy sub-50 kW
    /// hardware.
    ReaAcMeteringOnLegacyDcOnly,
    /// …and only where the rectification belongs to one session.
    ReaRectificationAttributable,
    /// …and the customer must be told the rectification loss is in the value.
    ReaRectificationLossDisclosed,
    /// The Arbeitspreis must be indicated at or beside the point.
    PangvArbeitspreisAtThePoint,
    /// …and, where a web-based system is used, through it before the session
    /// starts.
    PangvArbeitspreisBeforeStart,
    /// …and any further prices completely, in immediate vicinity.
    PangvAdditionalPricesAdjacent,
    /// THG-Quote requires a register entry and third-party access.
    ThgEligibility,
    /// An undertaking in scope must give the competent authority its details.
    Nis2Registration,
    /// …and take the ten risk-management measures the article enumerates.
    Nis2RiskManagement,
    /// …and be able to send an early warning within twenty-four hours.
    Nis2IncidentEarlyWarning,
    /// The management body must approve the measures and oversee them.
    Nis2ManagementApproval,
    /// …and its members must attend cybersecurity training.
    Nis2ManagementTraining,
    /// A manufacturer must report actively exploited vulnerabilities.
    CraVulnerabilityReporting,
    /// …and place only conformity-assessed products on the market.
    CraEssentialRequirements,
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
            Self::AfirNonDiscriminatoryPricing => "afir-non-discriminatory-pricing",
            Self::AfirEnergyBasedAdHocPrice => "afir-energy-based-ad-hoc-price",
            Self::AfirPriceShownAtStation => "afir-price-shown-at-station",
            Self::AfirPriceComponentsInOrder => "afir-price-components-in-order",
            Self::AfirMspPriceDisclosure => "afir-msp-price-disclosure",
            Self::AfirMspNoCrossBorderSurcharge => "afir-msp-no-cross-border-surcharge",
            Self::AfirDigitallyConnected => "afir-digitally-connected",
            Self::AfirSmartRecharging => "afir-smart-recharging",
            Self::AfirFixedCableOnDc => "afir-fixed-cable-on-dc",
            Self::AfirOwnerEnablesCompliance => "afir-owner-enables-compliance",
            Self::AfirAnnexIiConnector => "afir-annex-ii-connector",
            Self::AfirStaticData => "afir-static-data",
            Self::AfirDynamicData => "afir-dynamic-data",
            Self::AfirDataApi => "afir-data-api",
            Self::AfirDatex2 => "afir-datex2",
            Self::Da656Iso15118Dash2 => "da656-iso15118-2",
            Self::Da656Iso15118Dash20Public => "da656-iso15118-20-public",
            Self::Da656Iso15118Dash20Private => "da656-iso15118-20-private",
            Self::Da656AutomaticAuthenticationBothGenerations => "da656-auto-auth-both-generations",
            Self::Lsv2026TechnicalRequirements => "lsv2026-technical-requirements",
            Self::Lsv2026TechnicalEvidence => "lsv2026-technical-evidence",
            Self::Lsv2026CommissioningNotice => "lsv2026-commissioning-notice",
            Self::Lsv2026DecommissioningNotice => "lsv2026-decommissioning-notice",
            Self::Lsv2026OperatorChangeNotice => "lsv2026-operator-change-notice",
            Self::EichrechtConformityAssessedMeter => "eichrecht-conformity-assessed-meter",
            Self::EichrechtVerifiableValues => "eichrecht-verifiable-values",
            Self::EichrechtVerificationPeriod => "eichrecht-verification-period",
            Self::EichrechtInterventionRecords => "eichrecht-intervention-records",
            Self::EichrechtValueUserConfirmation => "eichrecht-value-user-confirmation",
            Self::ReaAcMeteringOnLegacyDcOnly => "rea-ac-metering-on-legacy-dc-only",
            Self::ReaRectificationAttributable => "rea-rectification-attributable",
            Self::ReaRectificationLossDisclosed => "rea-rectification-loss-disclosed",
            Self::PangvArbeitspreisAtThePoint => "pangv-arbeitspreis-at-the-point",
            Self::PangvArbeitspreisBeforeStart => "pangv-arbeitspreis-before-start",
            Self::PangvAdditionalPricesAdjacent => "pangv-additional-prices-adjacent",
            Self::ThgEligibility => "thg-eligibility",
            Self::Nis2Registration => "nis2-registration",
            Self::Nis2RiskManagement => "nis2-risk-management",
            Self::Nis2IncidentEarlyWarning => "nis2-incident-early-warning",
            Self::Nis2ManagementApproval => "nis2-management-approval",
            Self::Nis2ManagementTraining => "nis2-management-training",
            Self::CraVulnerabilityReporting => "cra-vulnerability-reporting",
            Self::CraEssentialRequirements => "cra-essential-requirements",
        }
    }
}

impl core::fmt::Display for ObligationId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who a duty binds, and what it is judged against.
///
/// Not every obligation in European charging law is about a charge point.
/// `[AFIR Art. 5(5)]` binds the *mobility service provider*. Judging that
/// against a [`ChargePointProfile`] would be a category error; leaving it out
/// of the calendar would let it be forgotten; and stubbing it out to always
/// return `false` is worse than either, because it looks assessable and is not.
///
/// So a rule carries the two functions it needs, typed to the profile it reads.
#[derive(Clone, Copy)]
pub enum Rule {
    /// The duty is a property of one charge point.
    ChargePoint {
        /// Whether the duty binds this point at all.
        applicable: fn(&ChargePointProfile) -> bool,
        /// Whether the point meets it, **on the day asked about**.
        satisfied: fn(&ChargePointProfile, Date) -> bool,
    },
    /// The duty binds a mobility service provider.
    Provider {
        /// Whether the duty binds this provider at all.
        applicable: fn(&ProviderProfile) -> bool,
        /// Whether the provider meets it, on the day asked about.
        satisfied: fn(&ProviderProfile, Date) -> bool,
    },
    /// The duty binds the **undertaking** — the company rather than one of its
    /// points or one of the roles it plays.
    ///
    /// `[NIS2 Anh. I]` names charge point operators in the Energy sector, and
    /// `[CRA Art. 13]` binds whoever places a product with digital elements on
    /// the market. Neither is a fact about a charge point.
    Undertaking {
        /// Whether the duty binds this undertaking at all.
        applicable: fn(&UndertakingProfile) -> bool,
        /// Whether it meets it, on the day asked about.
        satisfied: fn(&UndertakingProfile, Date) -> bool,
    },
}

impl core::fmt::Debug for Rule {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::ChargePoint { .. } => "Rule::ChargePoint",
            Self::Provider { .. } => "Rule::Provider",
            Self::Undertaking { .. } => "Rule::Undertaking",
        })
    }
}

/// Which kind of subject a duty binds — the part of [`Rule`] worth reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum Scope {
    /// A charge point.
    ChargePoint,
    /// A mobility service provider.
    MobilityServiceProvider,
    /// The undertaking itself.
    Undertaking,
}

/// What failing a rule in the calendar actually costs.
///
/// Almost every entry is a **duty**: a regulator can fine an operator, order a
/// retrofit or forbid the operation of a point over it. One is not.
/// `[38k §6(3)]`'s four conditions stand between a public kilowatt-hour and the
/// greenhouse-gas quota it is worth a second time, and an operator meeting none
/// of them has broken no law — its own remedy says so: *"…or forgo the quota"*.
/// In one bucket, an estate that meets every legal duty in Europe and declines a
/// German subsidy reads as failing (D219).
///
/// The distinction earns its place by changing an answer: [`Assessment::verdict`]
/// reads [`Self::Breach`] only, and [`Assessment::forgone`] reports the money
/// left on the table, which is never a compliance finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Consequence {
    /// A regulator can act on it: a fine, a retrofit order, a closed point.
    Breach,
    /// Nothing is unlawful. A benefit the estate was entitled to is forgone.
    ForgoneEntitlement,
}

impl Consequence {
    /// Whether failing this is a breach of law.
    #[must_use]
    pub const fn is_breach(self) -> bool {
        matches!(self, Self::Breach)
    }
}

/// One regulatory duty: what it demands, of whom, from when, and who says so.
#[derive(Debug, Clone, Copy)]
pub struct Obligation {
    /// The stable identifier.
    pub id: ObligationId,
    /// What failing it costs — which is what decides whether an estate that
    /// does not meet it is *unlawful* or merely poorer.
    pub consequence: Consequence,
    /// A one-line statement of the duty.
    pub title: &'static str,
    /// The citation, in the form `specs/README.md` indexes.
    pub citation: &'static str,
    /// The first day the duty binds.
    pub applies_from: Date,
    /// The last day it binds, for duties that are superseded.
    pub applies_until: Option<Date>,
    /// Who it binds, and how to judge them.
    pub rule: Rule,
    /// What to do about it when it is not met.
    pub remedy: &'static str,
}

impl Obligation {
    /// `true` when the duty is in force on `on`.
    #[must_use]
    pub fn in_force_on(&self, on: Date) -> bool {
        on >= self.applies_from && self.applies_until.is_none_or(|until| on <= until)
    }

    /// Which kind of subject this duty binds.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        match self.rule {
            Rule::ChargePoint { .. } => Scope::ChargePoint,
            Rule::Provider { .. } => Scope::MobilityServiceProvider,
            Rule::Undertaking { .. } => Scope::Undertaking,
        }
    }
}

/// How one obligation came out for one subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The duty binds this subject and it is met.
    Satisfied,
    /// The duty binds this subject and it is not met.
    Failing,
    /// The duty does not bind this subject — wrong accessibility, wrong power
    /// class, built before the cut-off.
    NotApplicable,
    /// The duty has not started binding yet on the date asked about — work to
    /// plan. Kept apart from [`Self::NoLongerInForce`] because a superseded duty
    /// reported as *not yet* in force would be budgeted for.
    NotYetInForce,
    /// The duty bound this subject once and has been superseded. Nothing to do.
    NoLongerInForce,
    /// The duty binds a different kind of subject, so this profile cannot
    /// answer it.
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

/// The overall answer for a subject on a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every duty that binds this subject is met.
    Compliant,
    /// At least one is not.
    Failing,
}

/// Every obligation, judged, for one subject on one date.
#[derive(Debug, Clone)]
pub struct Assessment {
    /// What was judged, as text for a report.
    pub subject: String,
    /// Which kind of subject it was.
    pub scope: Scope,
    /// Which date the question was asked about.
    pub on: Date,
    /// One finding per obligation in the calendar.
    pub findings: Vec<Finding>,
}

impl Assessment {
    /// Every finding that is not met — breaches and forgone entitlements alike.
    ///
    /// The two are different answers and [`Self::breaches`] and
    /// [`Self::forgone`] are where they are told apart; this is the union, for a
    /// caller that wants the whole list of what an estate does not do.
    pub fn failing(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.status == Status::Failing)
    }

    /// The findings a regulator can act on.
    ///
    /// What [`Self::verdict`] is computed from. See [`Consequence`].
    pub fn breaches(&self) -> impl Iterator<Item = &Finding> {
        self.failing()
            .filter(|f| f.obligation.consequence.is_breach())
    }

    /// The benefits this estate is entitled to and does not take.
    ///
    /// Not a compliance finding. An operator that meets none of `[38k §6(3)]`
    /// has broken no law and is simply poorer, and reporting that as a breach
    /// is a false statement about a lawful estate (D219).
    pub fn forgone(&self) -> impl Iterator<Item = &Finding> {
        self.failing()
            .filter(|f| !f.obligation.consequence.is_breach())
    }

    /// The findings that are satisfied.
    pub fn satisfied(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == Status::Satisfied)
    }

    /// The status of one obligation.
    #[must_use]
    pub fn status_of(&self, id: ObligationId) -> Option<Status> {
        self.findings
            .iter()
            .find(|f| f.obligation.id == id)
            .map(|f| f.status)
    }

    /// Compliant only when nothing that binds this subject is **breached**.
    ///
    /// Breaches, not [`Self::failing`]: an estate that meets every legal duty
    /// and declines to claim the greenhouse-gas quota is lawful, and this used
    /// to call it `Failing` (D219). A forgone entitlement is money, and money
    /// is [`Self::forgone`]'s answer rather than this one's.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if self.breaches().next().is_some() {
            Verdict::Failing
        } else {
            Verdict::Compliant
        }
    }
}

// ── Dates the texts give ────────────────────────────────────────────────────

/// AFIR's general date of application.
const AFIR_APPLIES: Date = date!(2024 - 04 - 13);
/// `[AFIR Art. 5(7)]` — every public point digitally connected.
const AFIR_DIGITAL_CONNECTION: Date = date!(2024 - 10 - 14);
/// `[AFIR Art. 20(2)]` — static and dynamic data available; also
/// `[AFIR Art. 5(10)]`, fixed cables on DC.
const AFIR_DATA_AND_CABLES: Date = date!(2025 - 04 - 14);
/// `[DATEX-II-Profil]` — the German national access point's mandatory format.
const DATEX2_MANDATORY: Date = date!(2026 - 04 - 14);
/// `[DA-656 Art. 2]` — the delegated regulation applies.
///
/// Declared in [`crate::station`] because the interface rule turns on it as well
/// as this calendar does, and one date with two spellings is one date that can
/// be changed in one of them.
use crate::station::DA656_APPLIES;
/// `[DA-656 Anh. 2.1.2, 2.1.3]` — the EN ISO 15118-20 generation.
const DA656_ISO20: Date = date!(2027 - 01 - 01);
/// `[LSV26]` — the Ladesäulenverordnung 2026 in force.
const LSV26_IN_FORCE: Date = date!(2026 - 01 - 01);
/// `[PAngV]` — the Preisangabenverordnung of 12.11.2021 entered into force.
///
/// **The earliest date in this calendar, and it is German rather than
/// European.** `[PAngV §14(2)]` has required the Arbeitspreis at a publicly
/// accessible charge point since this day — two years before `[AFIR Art. 5(4)]`
/// applied, at every power rather than only at 50 kW and above, and with
/// `[PAngV §20]` behind it. The paragraph's cross-reference to the
/// Ladesäulenverordnung has since been renumbered to the 2025 version; the duty
/// itself has run from here.
const PANGV_IN_FORCE: Date = date!(2022 - 05 - 28);

/// The day the Eichrecht duties began being enforced against German charging
/// infrastructure.
///
/// `[MessEG]` itself has bound since 01.01.2015, but the Eichbehörden granted
/// the sector a transition that ran to this date, and a calendar that dated
/// these rows from the statute would report every point in the country in
/// breach for four years in which no authority acted — the failure the NIS2
/// rows avoid one instrument further on (D259).
///
/// One constant rather than a literal on each row: five duties read it, and a
/// date with five spellings is a date that can be changed in four of them.
const EICHRECHT_ENFORCED: Date = date!(2019 - 04 - 01);

/// `[REA 6-A]` — the date the Regelermittlungsausschuss published the
/// e-mobility rules under `[MessEG §46]`.
const REA_6A_PUBLISHED: Date = date!(2017 - 03 - 16);
/// `[REA 6-A §3.2]` — AC metering is allowed only in DC stations placed on the
/// market "bis zum 31. Dezember 2017", so the duty compares against the day
/// after.
const AC_METERING_CUTOFF: Date = date!(2018 - 01 - 01);

/// The day the NIS2 duties began binding an undertaking **in Germany**.
///
/// # Why this is not the Directive's own date
///
/// `[NIS2 Art. 41]` obliges Member States to adopt the rules by 17.10.2024 and
/// to apply them from 18.10.2024. A directive binds nobody directly: what binds
/// an undertaking is the national law, and Germany's — the `NIS2UmsuCG`, which
/// rewrites the BSIG — was promulgated on 05.12.2025 and came into force the
/// following day, with no general transitional period.
///
/// Using the Directive's date would report every German operator in breach for
/// fourteen months during which no German authority could act, which is exactly
/// the failure the ordering in [`judge`] exists to prevent, arriving through
/// the data instead. A calendar for a market judges against the law of that
/// market, and says which one it means.
///
/// The Regulation beside it needs no such note: `[CRA Art. 71]` applies
/// directly in every Member State on the days it names.
const NIS2_DE_IN_FORCE: Date = date!(2025 - 12 - 06);

/// The first day an undertaking already in scope is **in breach** for not having
/// registered.
///
/// # Why this is not [`NIS2_DE_IN_FORCE`]
///
/// The duty attaches when the law comes into force; the *breach* does not. The
/// German transposition gives the registration its own clock — "spätestens drei
/// Monate, nachdem sie erstmals oder erneut als eine der vorgenannten
/// Einrichtungen gelten" — so an undertaking that was already in scope on
/// 06.12.2025 had until 06.03.2026, and one assessed in January 2026 with no
/// registration was doing exactly what the law told it it could.
///
/// This is the same correction as [`NIS2_DE_IN_FORCE`] itself, one level in.
/// That one refused to report a breach during the fourteen months no German
/// authority could act; this one refuses to report it during the three months
/// the German authority itself gave. A window a statute grants is part of the
/// duty, not a grace period somebody may choose to ignore — and the calendar
/// already models the `[LSV26 §4(1)]` two-week notice that way, through
/// `Registration::is_timely_for`, so a registration duty with no window at all
/// was the odd one out (D259).
///
/// Only the registration carries one. The risk-management, incident-warning and
/// governance duties beside it bind from the day the law applies, with no
/// transitional provision — which is what makes the distinction worth a constant
/// rather than a blanket offset.
const NIS2_DE_REGISTRATION_DUE: Date = date!(2026 - 03 - 06);

/// `[CRA Art. 71(2)]` — the reporting duties of Article 14 apply from this day,
/// fifteen months before the rest of the Regulation.
const CRA_REPORTING_APPLIES: Date = date!(2026 - 09 - 11);

/// `[CRA Art. 71(2)]` — "Diese Verordnung gilt ab dem 11. Dezember 2027."
const CRA_APPLIES: Date = date!(2027 - 12 - 11);

/// The calendar itself.
///
/// A `const` table so the whole rule set is visible in one screen and reviewed
/// against the documents it cites, rather than scattered across the services
/// that enforce it.
pub const CALENDAR: &[Obligation] = &[
    // ── AFIR Art. 5(1): ad-hoc access and payment ───────────────────────────
    Obligation {
        id: ObligationId::AfirAdHocAccess,
        consequence: Consequence::Breach,
        title: "Ad-hoc charging must be possible without a contract",
        citation: "[AFIR Art. 5(1)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            // A point that charges nothing is ad-hoc usable by definition: the
            // last subparagraph of Art. 5(1) takes the whole paragraph off it,
            // and demanding a payment instrument of a free charger reports a
            // breach where the article grants an exemption.
            satisfied: |p, _| !p.requires_payment || p.ad_hoc_payment != AdHocPayment::None,
        },
        remedy: "offer a contract-free path at the point, or make the point free of charge",
    },
    Obligation {
        id: ObligationId::AfirPaymentInstrument,
        consequence: Consequence::Breach,
        title: "A point deployed from 13.04.2024 needs a payment instrument widely used in the Union",
        citation: "[AFIR Art. 5(1)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // "publicly accessible recharging points **deployed from**
            // 13 April 2024" — a deployment, not a renovation. And the last
            // subparagraph exempts points that do not require payment.
            applicable: |p| {
                p.is_public() && p.requires_payment && p.commissioned_on >= AFIR_APPLIES
            },
            // A QR-code device satisfies (c) only below 50 kW; at or above it,
            // only (a) a card reader or (b) a contactless device does.
            satisfied: |p, _| p.ad_hoc_payment.satisfies_afir_at(p.rated_power_kw),
        },
        remedy: "fit a card reader or contactless device (a QR flow only qualifies below 50 kW)",
    },
    Obligation {
        id: ObligationId::AfirPaymentInstrumentRetrofit,
        consequence: Consequence::Breach,
        title: "Points of at least 50 kW on TEN-T or a safe and secure parking area must be retrofitted",
        citation: "[AFIR Art. 5(1)]",
        applies_from: date!(2027 - 01 - 01),
        applies_until: None,
        rule: Rule::ChargePoint {
            // "…deployed **before** 13 April 2024…" — the subparagraph binds
            // exactly the population the first one does not, and covers safe
            // and secure parking areas as well as the TEN-T road network.
            //
            // The date limb is load-bearing rather than decorative. A point
            // deployed *from* 13.04.2024 already owes (a) or (b) under the
            // first subparagraph, and `AfirPaymentInstrument` is that duty; a
            // retrofit rule that reached it too would report one missing card
            // reader as two failing findings with one remedy, and would state a
            // duty over a population the Regulation does not put it on. The two
            // entries partition the estate at 13.04.2024 exactly, with no point
            // in both and none in neither (D215).
            applicable: |p| {
                p.is_public()
                    && p.requires_payment
                    && p.is_at_least_50_kw()
                    && p.commissioned_on < AFIR_APPLIES
                    && (p.on_ten_t || p.on_safe_secure_parking)
            },
            // Points (a) or (b) only: at 50 kW and above a QR flow never
            // qualified, so this is the same test as everywhere else.
            satisfied: |p, _| p.ad_hoc_payment.satisfies_afir_at(p.rated_power_kw),
        },
        remedy: "retrofit a card reader or contactless device before 01.01.2027",
    },
    // ── AFIR Art. 5(2): automatic authentication ────────────────────────────
    Obligation {
        id: ObligationId::AfirAutomaticAuthenticationOptOut,
        consequence: Consequence::Breach,
        title: "Where automatic authentication is offered, the right not to use it must be shown",
        citation: "[AFIR Art. 5(2)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public() && p.offers_automatic_authentication,
            satisfied: |p, _| p.automatic_authentication_opt_out_offered,
        },
        remedy: "show the ad-hoc and contract alternatives clearly, and offer them conveniently",
    },
    // ── AFIR Art. 5(3): non-discriminatory pricing ──────────────────────────
    //
    // One of exactly **two** paragraphs the Regulation names for regulatory
    // monitoring: "Member States shall ensure that their authorities regularly
    // monitor … the compliance of operators of recharging points and mobility
    // service providers with paragraphs 3 and 5" `[AFIR Art. 5(6)]`. Paragraph
    // 5 is two entries below, judged against a provider. Carrying one without
    // the other checks half of what the regulator was told to look at.
    Obligation {
        id: ObligationId::AfirNonDiscriminatoryPricing,
        consequence: Consequence::Breach,
        title: "Prices must not discriminate between end users and providers, or between providers",
        citation: "[AFIR Art. 5(3)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            // "However, the level of prices may be differentiated, but only if
            // the differentiation is proportionate and objectively justified."
            // So differentiation is not itself the breach — unjustified
            // differentiation is, and the two are different findings.
            satisfied: |p, _| p.price_conduct.is_non_discriminatory(),
        },
        remedy: "charge providers and end users alike, or record the proportionate, objectively justified reason the article requires",
    },
    // ── AFIR Art. 5(4): price shape and price transparency ──────────────────
    //
    // All three duties below are gated on `requires_payment`, and **not** by the
    // exemption in Art. 5(1). That one is worded "the requirements laid down in
    // *this paragraph*" and reaches paragraph 1 and nothing else; copying it
    // here would be exactly the misreading this calendar exists to prevent.
    //
    // The argument is one level further back, in the definitions. Every duty in
    // paragraph 4 is a duty about "the ad hoc price", and `[AFIR Art. 2(2)]`
    // defines that as "the price **charged** by the operator of a recharging
    // point to an end user for recharging on an ad hoc basis". A point that
    // charges nothing has no such price, so the duties have no subject rather
    // than an unmet one — the same distinction between *applicable* and
    // *satisfied* the whole module turns on.
    Obligation {
        id: ObligationId::AfirEnergyBasedAdHocPrice,
        consequence: Consequence::Breach,
        title: "At 50 kW and above the ad-hoc price must be based on a price per kWh",
        citation: "[AFIR Art. 5(4)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // The fourth subparagraph limits the first two to "all recharging
            // points deployed from 13 April 2024".
            applicable: |p| {
                p.is_public()
                    && p.requires_payment
                    && p.is_at_least_50_kw()
                    && p.commissioned_on >= AFIR_APPLIES
            },
            // A purely per-minute tariff on a fast charger is unlawful. An
            // occupancy fee per minute is permitted *in addition* to the kWh
            // price, never instead of it.
            satisfied: |p, _| p.price_transparency.energy_based,
        },
        remedy: "price the ad-hoc tariff per kWh; an occupancy fee per minute may only be added to it",
    },
    Obligation {
        id: ObligationId::AfirPriceShownAtStation,
        consequence: Consequence::Breach,
        title: "At 50 kW and above the price per kWh and any occupancy fee must be shown at the station",
        citation: "[AFIR Art. 5(4)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| {
                p.is_public()
                    && p.requires_payment
                    && p.is_at_least_50_kw()
                    && p.commissioned_on >= AFIR_APPLIES
            },
            satisfied: |p, _| p.price_transparency.shown_at_station,
        },
        remedy: "show the price before the session starts, derived from the tariff that rates it",
    },
    Obligation {
        id: ObligationId::AfirPriceComponentsInOrder,
        consequence: Consequence::Breach,
        title: "Below 50 kW every price component must be available, in the prescribed order",
        citation: "[AFIR Art. 5(4)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // The third subparagraph, and the one the fourth does *not* limit
            // to points deployed from 13.04.2024 — so it reaches the whole
            // installed base below 50 kW. The article prescribes the *order*:
            // per kWh, per minute, per session, then anything else.
            applicable: |p| p.is_public() && p.requires_payment && !p.is_at_least_50_kw(),
            satisfied: |p, _| p.price_transparency.components_in_prescribed_order,
        },
        remedy: "present the components as kWh, then minute, then session, then the rest",
    },
    // ── AFIR Art. 5(5): the provider's duties ───────────────────────────────
    Obligation {
        id: ObligationId::AfirMspPriceDisclosure,
        consequence: Consequence::Breach,
        title: "A provider must disclose every price component before the session, e-roaming costs included",
        citation: "[AFIR Art. 5(5)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::Provider {
            applicable: |_| true,
            satisfied: |p, _| {
                p.discloses_all_price_components
                    && p.discloses_e_roaming_costs
                    && p.discloses_electronically
            },
        },
        remedy: "publish the full component breakdown, e-roaming costs named separately, before the session starts",
    },
    Obligation {
        id: ObligationId::AfirMspNoCrossBorderSurcharge,
        consequence: Consequence::Breach,
        title: "A provider may not apply any extra charge for cross-border e-roaming",
        citation: "[AFIR Art. 5(5)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::Provider {
            applicable: |_| true,
            // The article does not ask for this to be reasonable and
            // transparent; it forbids it outright.
            satisfied: |p, _| !p.surcharges_cross_border_roaming,
        },
        remedy: "remove the cross-border surcharge: the article forbids it outright, not merely caps it",
    },
    Obligation {
        id: ObligationId::EichrechtValueUserConfirmation,
        consequence: Consequence::Breach,
        title: "A provider billing on somebody else's meter owes a confirmation from its operator",
        citation: "[MessEG §33(2)]",
        applies_from: EICHRECHT_ENFORCED,
        applies_until: None,
        rule: Rule::Provider {
            // "Wer Messwerte verwendet" — every provider does, on every
            // roaming session it re-bills. The statute does not excuse the
            // party that did not measure: it asks for a confirmation *from the
            // operator*, which lives in a roaming agreement rather than in
            // anything a CDR carries.
            applicable: |_| true,
            satisfied: |p, _| p.holds_meter_operator_confirmation,
        },
        remedy: "put the [MessEG §33(2)] confirmation in the roaming agreement: verifying every signed record proves the values are the meter's and says nothing about whether that meter's operator is meeting its own duties",
    },
    // ── AFIR Art. 5(7), (8), (10): the duties the BNetzA actually audits ────
    //
    // `[LSV26 §5]` names Art. 5(1), (2), (7), (8) and (10) — **and Anhang II**
    // — as the requirements the regulator may inspect, demand a retrofit for,
    // and close a point over. The Annex is the limb every summary drops, and
    // dropping it here dropped a duty: `AfirAnnexIiConnector`, below, is that
    // half.
    //
    // These three are the ones almost no compliance model carries.
    Obligation {
        id: ObligationId::AfirDigitallyConnected,
        consequence: Consequence::Breach,
        title: "Every publicly accessible point must be a digitally-connected recharging point",
        citation: "[AFIR Art. 5(7)]",
        applies_from: AFIR_DIGITAL_CONNECTION,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            satisfied: |p, _| p.digitally_connected,
        },
        remedy: "connect the point to a CSMS: without it neither the data nor the smart-charging duties can be met",
    },
    Obligation {
        id: ObligationId::AfirSmartRecharging,
        consequence: Consequence::Breach,
        title: "Points built after 13.04.2024 or renovated after 14.10.2024 must be capable of smart recharging",
        citation: "[AFIR Art. 5(8)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // The one duty with two different dates in one sentence — "built
            // after 13 April 2024 **or renovated after 14 October 2024**" —
            // and both are strict, so a point commissioned exactly on
            // 13.04.2024 is outside it.
            applicable: |p| {
                p.is_public()
                    && (p.commissioned_on > AFIR_APPLIES
                        || p.renovated_on
                            .is_some_and(|on| on > AFIR_DIGITAL_CONNECTION))
            },
            satisfied: |p, _| p.smart_recharging_capable,
        },
        remedy: "accept and follow charging profiles (OCPP smart charging); a fixed-current point does not qualify",
    },
    Obligation {
        id: ObligationId::AfirFixedCableOnDc,
        consequence: Consequence::Breach,
        title: "Every publicly accessible DC point must have a fixed recharging cable installed",
        citation: "[AFIR Art. 5(10)]",
        applies_from: AFIR_DATA_AND_CABLES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public() && p.current_type == CurrentType::Dc,
            satisfied: |p, _| p.fixed_cable,
        },
        remedy: "fit a tethered cable: a DC point with a socket only is out of compliance",
    },
    Obligation {
        id: ObligationId::AfirOwnerEnablesCompliance,
        consequence: Consequence::Breach,
        title: "A third-party owner must supply a point whose characteristics let the operator comply with 5(2), (7), (8) and (10)",
        citation: "[AFIR Art. 5(11)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // Host-owned hardware is the normal case — a hotel, a supermarket
            // or a municipality buys the charger and a CPO operates it — and
            // the operator is then held to duties it cannot meet on equipment
            // it cannot change. The article puts that on the owner, which is
            // only any use to an operator that wrote it into the contract.
            applicable: |p| p.is_public() && p.ownership.is_third_party(),
            satisfied: |p, _| p.ownership == Ownership::ThirdPartyEnabling,
        },
        remedy: "write the Art. 5(11) characteristics into the arrangement with the owner: the duties are still enforced against the operator",
    },
    // ── AFIR Art. 21(1) + Annex II: the interface, which is a duty ─────────
    //
    // "The technical specifications set out in Annex II shall apply." One
    // sentence, and it turns the connector list from a description into a
    // requirement — the one `[LSV26 §5]` names beside Article 5 and the one
    // `[LSV26 §5(3)]` lets the regulator **forbid the operation of a point**
    // over. A compliance model that carries Article 5 and stops has left out
    // the half that closes stations.
    //
    // One row rather than two, and the reason is where the amendment put its
    // own date. `[DA-656]` replaces Annex II points 1.1 to 1.4 from 08.01.2026,
    // and the two things it changes — the Mode 2 exemption and the L-category
    // power split — are written into limbs reading "installed or renovated from
    // 8 January 2026". So the population, not the assessment date, decides which
    // sentence a point answers to, and that is a fact about the profile:
    // `afir_annex_ii_satisfied` reads `installed_or_renovated_on`, exactly as
    // the DA-656 vehicle-communication duties below do.
    Obligation {
        id: ObligationId::AfirAnnexIiConnector,
        consequence: Consequence::Breach,
        title: "A point must carry the interface Annex II names for its current, power and vehicle class",
        citation: "[AFIR Art. 21(1)]",
        applies_from: AFIR_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // Annex II states no accessibility limb of its own — it is a
            // technical specification, not an operator duty — so what decides
            // the population here is the route by which it is enforced.
            // `[LSV26 §1(1)]` scopes the German regime to publicly accessible
            // points for class M and N vehicles, and `[LSV26 §5]` is the power
            // exercised over them. A private wall box is outside that, and
            // reporting it as in breach would be a finding no authority can act
            // on.
            applicable: |p| p.is_public(),
            satisfied: |p, _| p.afir_annex_ii_satisfied(),
        },
        remedy: "fit the Annex II interface for the point's current — Type 2 on AC, CCS Combo 2 on DC — or one of the two the Annex admits in its own bands: Type 3A or an IEC 60884-1 socket on an L-category point up to 3.7 kW [DA-656 Anh. 1.3.1], and an IEC 60884-1 socket on a Mode 2 point up to 3.7 kW installed or renovated from 08.01.2026 [DA-656 Anh. 1.1]. A CHAdeMO-only DC post does not qualify however many vehicles it serves",
    },
    // ── AFIR Art. 20: the data duties, kept apart ──────────────────────────
    Obligation {
        id: ObligationId::AfirStaticData,
        consequence: Consequence::Breach,
        title: "Static data must be available free of charge",
        citation: "[AFIR Art. 20(2)]",
        applies_from: AFIR_DATA_AND_CABLES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            satisfied: |p, _| p.data.static_data,
        },
        remedy: "publish location, connectors, current type, power, opening hours and contact",
    },
    Obligation {
        id: ObligationId::AfirDynamicData,
        consequence: Consequence::Breach,
        title: "Dynamic data — status, availability, ad-hoc price, renewable share — must be available free of charge",
        citation: "[AFIR Art. 20(2)]",
        applies_from: AFIR_DATA_AND_CABLES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // "The requirements laid down in point (c) shall not apply to
            // publicly accessible recharging points that do not require payment
            // for the recharging service." The static duty has no such
            // exemption, which is why the two are separate obligations.
            applicable: |p| p.is_public() && p.requires_payment,
            satisfied: |p, _| p.data.dynamic_data,
        },
        remedy: "publish operational status, availability, the ad-hoc price and the renewable flag",
    },
    Obligation {
        id: ObligationId::AfirDataApi,
        consequence: Consequence::Breach,
        title: "An API giving free and unrestricted access to the data must be registered with the national access point",
        citation: "[AFIR Art. 20(3)]",
        applies_from: AFIR_DATA_AND_CABLES,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            satisfied: |p, _| p.data.open_api,
        },
        remedy: "stand up the API and submit it to the national access point — publishing the data is not enough",
    },
    Obligation {
        id: ObligationId::AfirDatex2,
        consequence: Consequence::Breach,
        title: "The national-access-point feed must use the DATEX II Recharging profile",
        citation: "[DATEX-II-Profil]",
        applies_from: DATEX2_MANDATORY,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            satisfied: |p, _| p.data.datex2,
        },
        remedy: "switch the Mobilithek feed to the DATEX II Recharging profile",
    },
    // ── DA-656: vehicle-to-grid communication ───────────────────────────────
    Obligation {
        id: ObligationId::Da656Iso15118Dash2,
        consequence: Consequence::Breach,
        title: "Public points installed or renovated from 08.01.2026 must implement EN ISO 15118-1…-5",
        citation: "[DA-656 Anh. 2.1.1]",
        applies_from: DA656_APPLIES,
        applies_until: None,
        rule: Rule::ChargePoint {
            // The exemption for existing low-level-communication points is
            // already in the date: 2.1.1 binds only points "installed or
            // renovated from 8 January 2026". Testing for PWM here as well —
            // which this calendar once did — would exempt a point *because* it
            // fails the duty, which is exactly backwards.
            applicable: |p| p.is_public() && p.installed_or_renovated_on() >= DA656_APPLIES,
            satisfied: |p, _| p.v2g.iso15118_2,
        },
        remedy: "deploy firmware implementing EN ISO 15118-1…-5",
    },
    Obligation {
        id: ObligationId::Da656Iso15118Dash20Public,
        consequence: Consequence::Breach,
        title: "Public points installed or renovated from 01.01.2027 must implement EN ISO 15118-20",
        citation: "[DA-656 Anh. 2.1.2]",
        applies_from: DA656_ISO20,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public() && p.installed_or_renovated_on() >= DA656_ISO20,
            satisfied: |p, _| p.v2g.iso15118_20,
        },
        remedy: "TLS 1.3 and the larger certificates of -20 usually mean a hardware refresh",
    },
    Obligation {
        id: ObligationId::Da656Iso15118Dash20Private,
        consequence: Consequence::Breach,
        title: "Private Mode 3/4 points installed or renovated from 01.01.2027 must implement EN ISO 15118-20",
        citation: "[DA-656 Anh. 2.1.3]",
        applies_from: DA656_ISO20,
        applies_until: None,
        rule: Rule::ChargePoint {
            // The one duty that reaches behind the fence — and only for Mode 3
            // and Mode 4. A domestic socket with an in-cable box is Mode 2, and
            // 2.1.3(a) asks it for EN IEC 61851-1 instead.
            applicable: |p| {
                !p.is_public()
                    && p.mode() != ChargingMode::Mode2
                    && p.installed_or_renovated_on() >= DA656_ISO20
            },
            satisfied: |p, _| p.v2g.iso15118_20,
        },
        remedy: "a depot or workplace wall box installed from 2027 is in scope: plan the firmware generation now",
    },
    Obligation {
        id: ObligationId::Da656AutomaticAuthenticationBothGenerations,
        consequence: Consequence::Breach,
        title: "A point offering automatic authentication must support both EN ISO 15118-2 and -20",
        citation: "[DA-656 Anh. 2.1.2]",
        applies_from: DA656_ISO20,
        applies_until: None,
        rule: Rule::ChargePoint {
            // "Where **such** recharging points offer automatic authentication
            // and authorisation services, such as plug-and-charge" — "such"
            // being the 2.1.2 points, so the same public/date test applies. And
            // it is automatic authentication generally, not Plug & Charge
            // specifically: AutoCharge counts.
            applicable: |p| {
                p.is_public()
                    && p.offers_automatic_authentication
                    && p.installed_or_renovated_on() >= DA656_ISO20
            },
            satisfied: |p, _| p.v2g.iso15118_2 && p.v2g.iso15118_20,
        },
        remedy: "a point doing automatic authentication may not drop -2: vehicles on both generations must be served",
    },
    // ── National law ────────────────────────────────────────────────────────
    // ── LSV 2026: the duty, the evidence, and three notices ────────────────
    //
    // § 5(1)–(3) let the regulator inspect, demand a retrofit for, and forbid
    // the operation of a point over "eine technische Anforderung nach § 3" —
    // so a calendar carrying the powers without the duty they are exercised
    // over names the consequence and omits the cause.
    Obligation {
        id: ObligationId::Lsv2026TechnicalRequirements,
        consequence: Consequence::Breach,
        title: "Every publicly accessible point must meet the applicable technical requirements",
        citation: "[LSV26 §3]",
        applies_from: LSV26_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            // § 1(1): the Verordnung governs publicly accessible points for
            // class M and N vehicles.
            applicable: |p| p.is_public(),
            satisfied: |p, _| p.meets_technical_requirements,
        },
        remedy: "meet § 49(1) EnWG and the applicable technical rules; § 5(2) lets the regulator demand the retrofit and § 5(3) lets it close the point",
    },
    Obligation {
        id: ObligationId::Lsv2026TechnicalEvidence,
        consequence: Consequence::Breach,
        title: "The operator must be able to prove compliance with § 3 on the regulator's request",
        citation: "[LSV26 §4]",
        applies_from: LSV26_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            // § 4(2). A duty to *be able to prove* is failed quietly: nothing
            // goes wrong until the request arrives, and by then the documents
            // either exist or they do not.
            satisfied: |p, _| p.registration.technical_documentation_available,
        },
        remedy: "keep the § 3 conformity documentation retrievable per point; § 5(3) closes a point whose compliance is not evidenced",
    },
    Obligation {
        id: ObligationId::Lsv2026CommissioningNotice,
        consequence: Consequence::Breach,
        title: "Commissioning must be notified to the regulator at the latest two weeks afterwards",
        citation: "[LSV26 §4]",
        applies_from: LSV26_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            // A deadline, not a flag: § 5(3) lets the Bundesnetzagentur forbid
            // the operation of a point whose notice was never filed. And the
            // deadline runs from the day the point became *publicly
            // accessible* when that is later than commissioning — § 4(3)
            // applies the régime afresh to an existing point that opens to the
            // public, which a rule reading `commissioned_on` alone reports as
            // years late on its first day.
            satisfied: |p, _| {
                p.registration
                    .is_timely_for(p.notifiable_commissioning_date())
            },
        },
        remedy: "file the electronic Inbetriebnahme notice within two weeks of commissioning — or of the day the point became publicly accessible; a late filing can close the point",
    },
    Obligation {
        id: ObligationId::Lsv2026DecommissioningNotice,
        consequence: Consequence::Breach,
        title: "Decommissioning must be notified to the regulator without undue delay",
        citation: "[LSV26 §4]",
        applies_from: LSV26_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            // § 4(1) Nr. 2. Only a point that has actually been taken out of
            // service owes this, which is why the applicability reads the
            // event rather than a flag.
            applicable: |p| p.is_public() && p.registration.decommissioning.is_some(),
            satisfied: |p, _| {
                p.registration
                    .decommissioning
                    .is_some_and(|notice| notice.is_timely(Registration::PROMPT_NOTIFICATION_DAYS))
            },
        },
        remedy: "file the Außerbetriebnahme notice: a point the register still shows as live is one the operator is still answerable for",
    },
    Obligation {
        id: ObligationId::Lsv2026OperatorChangeNotice,
        consequence: Consequence::Breach,
        title: "A change of operator must be notified by both the outgoing and the incoming operator",
        citation: "[LSV26 §4]",
        applies_from: LSV26_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public() && p.registration.operator_change.is_some(),
            // "Bei einem Betreiberwechsel haben Anzeigen nach Satz 1 durch den
            // bisherigen **und** den neuen Betreiber zu erfolgen." Two notices,
            // and an incoming operator that files its own and assumes the
            // outgoing one did the same has a point the regulator may forbid
            // the operation of, over a notice it never saw and did not owe.
            satisfied: |p, _| {
                p.registration.operator_change.is_some_and(|change| {
                    change.both_filed_timely(Registration::PROMPT_NOTIFICATION_DAYS)
                })
            },
        },
        remedy: "both operators file: confirm the outgoing operator's notice rather than assuming it, because the point is closed over the pair and not over yours",
    },
    Obligation {
        id: ObligationId::EichrechtConformityAssessedMeter,
        consequence: Consequence::Breach,
        title: "Billing by energy requires a conformity-assessed meter",
        citation: "[MessEG §33]",
        applies_from: EICHRECHT_ENFORCED,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.metering.bills_by_energy,
            satisfied: |p, _| p.metering.mid_conformity_assessed,
        },
        remedy: "a point without an assessed meter may not bill by kWh at all",
    },
    Obligation {
        id: ObligationId::EichrechtVerifiableValues,
        consequence: Consequence::Breach,
        title: "The customer must be able to verify the billed measured value",
        citation: "[PTB-A 50.7]",
        applies_from: EICHRECHT_ENFORCED,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.metering.bills_by_energy,
            satisfied: |p, _| p.metering.signed_values,
        },
        remedy: "emit OCMF-signed values and retain them with the session",
    },
    // ── MessEG § 31 and § 33: the duties of the parties, not of the device ──
    //
    // `[MessEG §31(2)]` puts four duties on whoever **uses** a meter and
    // `[MessEG §33(2)]` one on whoever uses its **values**. Between them they
    // are the half of the Eichrecht that is about dates and paperwork rather
    // than about signatures, and it is the half a platform that verifies every
    // record can still fail completely.
    Obligation {
        id: ObligationId::EichrechtVerificationPeriod,
        consequence: Consequence::Breach,
        title: "The meter's verification period must not have expired",
        citation: "[MessEG §37(1)]",
        applies_from: EICHRECHT_ENFORCED,
        applies_until: None,
        rule: Rule::ChargePoint {
            // "Messgeräte dürfen nicht ungeeicht verwendet werden, nachdem die
            // … bestimmte Eichfrist abgelaufen ist." The duty is about using
            // the meter, so it binds any point that bills by energy — the same
            // population `[MessEG §33]` reaches.
            applicable: |p| p.metering.bills_by_energy,
            // **The one duty in this calendar that is a fact about the day it
            // is asked on.** Everything else is a property of the point; this
            // is a date passing. See `ChargePointProfile::verification_expires_on`
            // for the three sentences that decide it — where the clock starts
            // `[MessEG §37(1) S. 2]`, the eight years
            // `[MessEV Anl. 7 Nr. 6.7]` gives charging equipment, and the end
            // of the calendar year `[MessEV §34(2)]` adds to it.
            satisfied: ChargePointProfile::verification_valid_on,
        },
        remedy: "have the meter re-verified, or replace it: every kilowatt-hour billed after the period expires is a measured value used without a properly used meter [MessEG §33(1)], whatever else the record carries",
    },
    Obligation {
        id: ObligationId::EichrechtInterventionRecords,
        consequence: Consequence::Breach,
        title: "Records of every intervention on the meter — firmware included — must be kept",
        citation: "[MessEG §31(2)]",
        applies_from: EICHRECHT_ENFORCED,
        applies_until: None,
        rule: Rule::ChargePoint {
            // Nr. 4 binds whoever uses the meter, which is any point metering
            // for money. The clause that reaches a *platform* is its last:
            // "einschließlich solcher durch elektronisch vorgenommene
            // Maßnahmen" — a remote firmware push is an Eingriff whose record
            // the operator owes, and a CSMS is what performs it.
            applicable: |p| p.metering.bills_by_energy,
            satisfied: |p, _| p.metering.intervention_records_retained,
        },
        remedy: "keep the maintenance, repair and firmware-update record for three months past the verification period and at most five years — the one retention figure the German metrology texts actually give",
    },
    // ── REA 6-A: AC metering inside a DC station ───────────────────────────
    //
    // `[REA 6-A §3.2]` permits a DC station to meter on the AC side, before the
    // rectifier — but only on legacy hardware, only where the rectification
    // belongs to one session, and only if the customer is told that the losses
    // are inside the number they are billed for. Three conditions, and a
    // platform whose central claim is that the customer can check the value
    // owes them the third.
    Obligation {
        id: ObligationId::ReaAcMeteringOnLegacyDcOnly,
        consequence: Consequence::Breach,
        title: "AC metering before the rectifier is only permitted in DC stations placed on the market before 2018 and rated at most 50 kW",
        citation: "[REA 6-A]",
        applies_from: REA_6A_PUBLISHED,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| {
                p.current_type == CurrentType::Dc
                    && p.metering.measurement_point == EnergyMeasurementPoint::AcBeforeRectifier
            },
            // Note the direction of the threshold: **at most** 50 kW. AFIR's
            // fast-charger duties begin at the same number counting the other
            // way, so a 50 kW station is on the strict side of one rule and the
            // permissive side of this one.
            satisfied: |p, _| {
                p.placed_on_market_date() < AC_METERING_CUTOFF && p.is_at_most_50_kw()
            },
        },
        remedy: "meter after the rectifier: on anything newer or larger, the AC-side allowance does not exist",
    },
    Obligation {
        id: ObligationId::ReaRectificationAttributable,
        consequence: Consequence::Breach,
        title: "AC metering requires the rectification to belong to exactly one charging session",
        citation: "[REA 6-A]",
        applies_from: REA_6A_PUBLISHED,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| {
                p.current_type == CurrentType::Dc
                    && p.metering.measurement_point == EnergyMeasurementPoint::AcBeforeRectifier
            },
            // The condition an operator is most likely to be quietly in breach
            // of: a multi-outlet DC cabinet sharing one rectifier fails it, and
            // shared rectifiers are the normal way such cabinets are built.
            satisfied: |p, _| p.metering.rectification_attributable_to_one_session,
        },
        remedy: "a shared rectifier cannot be attributed to one session: meter after it, per outlet",
    },
    // The paragraph names *"einem Messwert oder einer Rechnung"*, and the
    // invoice half is discharged by construction: `emob_billing` puts the
    // compensated loss and the sentence on the line stating the measured value,
    // from the station's own signed record (D253). This is the other half — the
    // notice at the point, before a driver who may never see an invoice starts
    // — which nothing in this workspace can evidence and which therefore stays
    // a fact somebody states.
    Obligation {
        id: ObligationId::ReaRectificationLossDisclosed,
        consequence: Consequence::Breach,
        title: "The customer must be told at the point that rectification losses are part of the measured value",
        citation: "[REA 6-A]",
        applies_from: REA_6A_PUBLISHED,
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| {
                p.current_type == CurrentType::Dc
                    && p.metering.measurement_point == EnergyMeasurementPoint::AcBeforeRectifier
            },
            satisfied: |p, _| p.metering.rectification_loss_disclosed,
        },
        remedy: "state on the receipt and at the point that the value includes the energy the rectification consumed: a value the customer cannot interpret is one they cannot check",
    },
    // ── PAngV § 14: the German price indication, older and wider than AFIR ──
    //
    // `[AFIR Art. 5(4)]` splits at 50 kW and dates from 13.04.2024.
    // `[PAngV §14(2)]` has bound every publicly accessible point that takes an
    // ad-hoc payment since **28.05.2022**, at any power, and `[PAngV §20]`
    // makes a failure an Ordnungswidrigkeit under the Wirtschaftsstrafgesetz.
    //
    // The two are not the same duty said twice. AFIR asks that the price be
    // *shown* and, above 50 kW, that it be *per kWh*; PAngV asks for a
    // particular figure — the **Arbeitspreis**, which `[PAngV §2 Nr. 1]`
    // defines as the consumption-dependent price per unit *"einschließlich der
    // Umsatzsteuer"* and `[PAngV §14(4)]` fixes at one kilowatt-hour — by one
    // of three named media, at or beside the point. A German 22 kW post priced
    // by the minute alone satisfies AFIR and breaches this, and a post quoting
    // a net price satisfies neither `[PAngV §3(1)]` nor this.
    //
    // The tariff half of that — whether a tariff *has* an Arbeitspreis and
    // whether it has a Gesamtpreis at all — is `emob_tariff::check_pangv`,
    // because it is a property of a price list rather than of a point. What is
    // here is the half that is a fact about the post.
    Obligation {
        id: ObligationId::PangvArbeitspreisAtThePoint,
        consequence: Consequence::Breach,
        title: "The Arbeitspreis must be indicated at or beside every public point taking an ad-hoc payment",
        citation: "[PAngV §14(2)]",
        applies_from: PANGV_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            // "Wer an einem öffentlich zugänglichen Ladepunkt Verbrauchern das
            // punktuelle Aufladen … anbietet, hat **beim Einsatz eines für das
            // punktuelle Aufladen vorgesehenen Bezahlverfahrens** den … für den
            // jeweiligen Ladepunkt geltenden Arbeitspreis … anzugeben."
            //
            // Three limbs, and the third is the one that keeps this off a free
            // charger and off a contract-only post: there has to *be* an ad-hoc
            // payment method in use. `[PAngV §1(1)]` narrows it once more —
            // the whole Verordnung governs prices stated to **consumers**.
            applicable: |p| {
                p.is_public() && p.requires_payment && p.ad_hoc_payment != AdHocPayment::None
            },
            satisfied: |p, _| p.price_transparency.arbeitspreis.is_indicated(),
        },
        remedy: "print the price per kWh on the post, put it on the point's own display, or signpost a registration-free, free-of-charge mobile page from the post — any one of the three answers § 14(2)",
    },
    Obligation {
        id: ObligationId::PangvArbeitspreisBeforeStart,
        consequence: Consequence::Breach,
        title: "A web-based ad-hoc system must state the Arbeitspreis before the session starts",
        citation: "[PAngV §14(2)]",
        applies_from: PANGV_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            // The paragraph's last sentence, and a **second** duty rather than
            // a fourth medium: "Wird für das punktuelle Aufladen von
            // Verbrauchern ein webbasiertes System verwendet, so hat der
            // Anbieter den Arbeitspreis … über dieses webbasierte System
            // spätestens vor dem Start des Ladevorgangs anzugeben."
            //
            // So an app or a QR-code checkout owes the figure *through that
            // system*, whatever is printed on the post — and a point that
            // satisfies the sentence above by a sticker can still fail this one.
            applicable: |p| {
                p.is_public() && p.requires_payment && p.price_transparency.web_based_ad_hoc
            },
            satisfied: |p, _| p.price_transparency.arbeitspreis_stated_before_start,
        },
        remedy: "state the price per kWh in the app or web checkout before the driver can start the session, derived from the tariff that rates it",
    },
    Obligation {
        id: ObligationId::PangvAdditionalPricesAdjacent,
        consequence: Consequence::Breach,
        title: "Any further prices must be stated completely, in immediate vicinity of the Arbeitspreis",
        citation: "[PAngV §14(3)]",
        applies_from: PANGV_IN_FORCE,
        applies_until: None,
        rule: Rule::ChargePoint {
            // "Wer in den Fällen des Absatzes 1 oder 2 **zusätzlich**
            // leistungsabhängige oder nicht verbrauchsabhängige Preise fordert,
            // hat diese **vollständig in unmittelbarer Nähe** der Angabe des
            // Arbeits- oder Mengenpreises oder des Ladepunktes anzugeben."
            //
            // A blocking fee, a session fee or a power-band surcharge. The duty
            // arises only where such a price is actually charged, which is why
            // `AdditionalPrices` has three answers and not two: "none" and
            // "stated" are both compliant and they are different facts.
            applicable: |p| {
                p.is_public()
                    && p.requires_payment
                    && p.ad_hoc_payment != AdHocPayment::None
                    && p.price_transparency.additional_prices.are_charged()
            },
            satisfied: |p, _| {
                p.price_transparency.additional_prices == AdditionalPrices::StatedAdjacent
            },
        },
        remedy: "put every further price — blocking fee, session fee, power surcharge — beside the price per kWh, in full: a component the driver finds only on the invoice is not indicated",
    },
    Obligation {
        id: ObligationId::ThgEligibility,
        // Not a duty. `[38k §6(3)]` is what stands between a public kilowatt-hour
        // and the quota it is worth a second time, and an operator that meets
        // none of it has broken no law — the remedy below says so in its own
        // last clause.
        consequence: Consequence::ForgoneEntitlement,
        title: "THG-Quote requires a publishable register entry, lawful metering and an issued operator code",
        citation: "[38k §6(3)]",
        applies_from: date!(2022 - 01 - 01),
        applies_until: None,
        rule: Rule::ChargePoint {
            applicable: |p| p.is_public(),
            // Four cumulative conditions, and the notice they presuppose. Nr. 1
            // asks whether the *notified* point is published or publishable, so
            // the Anzeige is a premise of the paragraph rather than a fifth
            // condition — but a point with no notice has nothing to publish.
            satisfied: |p, _| {
                p.registration.commissioning_notified_on.is_some() && p.quota.is_eligible()
            },
        },
        remedy: "publish the register entry or consent to its publication, sign the conformity declaration the authority provides, and obtain an operator identification code — or forgo the quota",
    },
    // ── NIS2: the duties that name this industry by its role ───────────────
    //
    // `[NIS2 Anh. I]`, Energy, Electricity: "Betreiber von Ladepunkten, die für
    // die Verwaltung und den Betrieb eines Ladepunkts zuständig sind und
    // Endnutzern einen Aufladedienst erbringen, auch im Namen und Auftrag eines
    // Mobilitätsdienstleisters". Every duty below binds the **undertaking**,
    // which is why it needed a third profile rather than a field on a point.
    //
    // Articles 20, 21 and 23 bind essential and important entities alike; the
    // classes differ in how they are supervised (`[NIS2 Art. 32]` against
    // `[NIS2 Art. 33]`), not in what they owe.
    Obligation {
        id: ObligationId::Nis2Registration,
        consequence: Consequence::Breach,
        title: "An undertaking in scope must give the competent authority its details",
        citation: "[NIS2 Art. 3(4)]",
        // The duty attaches when the law applies and the breach begins three
        // months later, because the German transposition says so in as many
        // words. See `NIS2_DE_REGISTRATION_DUE`.
        applies_from: NIS2_DE_REGISTRATION_DUE,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: UndertakingProfile::is_in_nis2_scope,
            satisfied: |u, _| u.registered_with_the_authority,
        },
        remedy: "submit name, address, contact details, sector and the Member States served to the competent authority; changes follow within two weeks",
    },
    Obligation {
        id: ObligationId::Nis2RiskManagement,
        consequence: Consequence::Breach,
        title: "…and take all ten cybersecurity risk-management measures",
        citation: "[NIS2 Art. 21(2)]",
        applies_from: NIS2_DE_IN_FORCE,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: UndertakingProfile::is_in_nis2_scope,
            // "shall include **at least** the following" — a conjunction, not a
            // score. Nine of ten is not ninety per cent of a duty.
            satisfied: |u, _| u.risk_management.is_complete(),
        },
        remedy: "close the measures RiskManagement::missing() names: the article lists them as a floor, so each one absent is the duty unmet",
    },
    Obligation {
        id: ObligationId::Nis2IncidentEarlyWarning,
        consequence: Consequence::Breach,
        title: "…and be able to warn the CSIRT within twenty-four hours of a significant incident",
        citation: "[NIS2 Art. 23(4)]",
        applies_from: NIS2_DE_IN_FORCE,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: UndertakingProfile::is_in_nis2_scope,
            satisfied: |u, _| u.can_warn_within_24_hours,
        },
        remedy: "stand up the three-step path the article prescribes: an early warning within 24 h, an incident notification within 72 h, and a final report within a month of it",
    },
    Obligation {
        id: ObligationId::Nis2ManagementApproval,
        consequence: Consequence::Breach,
        title: "The management body must approve the measures and oversee their implementation",
        citation: "[NIS2 Art. 20(1)]",
        applies_from: NIS2_DE_IN_FORCE,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: UndertakingProfile::is_in_nis2_scope,
            satisfied: |u, _| u.management_approved_measures,
        },
        remedy: "put the measures to the management body: the same paragraph makes its members liable for infringements of the article",
    },
    Obligation {
        id: ObligationId::Nis2ManagementTraining,
        consequence: Consequence::Breach,
        title: "…and its members must attend cybersecurity training",
        citation: "[NIS2 Art. 20(2)]",
        applies_from: NIS2_DE_IN_FORCE,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: UndertakingProfile::is_in_nis2_scope,
            satisfied: |u, _| u.management_trained,
        },
        remedy: "train the management body, and offer the same regularly to all employees — the article requires the first and calls for the second",
    },
    // ── CRA: the duties of whoever ships the software ──────────────────────
    //
    // A Regulation rather than a Directive, so `[CRA Art. 71]`'s dates are the
    // dates, in every Member State, with no transposition in between.
    //
    // Whether they bind at all is the one genuinely per-deployment question in
    // this calendar: an operator running somebody else's hardware on somebody
    // else's platform is not a manufacturer, and one that publishes a station
    // firmware or a driver app under its own name is.
    Obligation {
        id: ObligationId::CraVulnerabilityReporting,
        consequence: Consequence::Breach,
        title: "A manufacturer must report an actively exploited vulnerability within twenty-four hours",
        citation: "[CRA Art. 14]",
        applies_from: CRA_REPORTING_APPLIES,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: |u| u.places_digital_products_on_the_market,
            satisfied: |u, _| u.can_report_exploited_vulnerabilities,
        },
        remedy: "open the path to the coordinator CSIRT and ENISA on the single reporting platform: an early warning within 24 h, a vulnerability notification within 72 h, and a final report within 14 days of a corrective measure",
    },
    Obligation {
        id: ObligationId::CraEssentialRequirements,
        consequence: Consequence::Breach,
        title: "…and place only conformity-assessed products with digital elements on the market",
        citation: "[CRA Art. 13]",
        applies_from: CRA_APPLIES,
        applies_until: None,
        rule: Rule::Undertaking {
            applicable: |u| u.places_digital_products_on_the_market,
            // Two halves of one duty: the product meets the essential
            // requirements of Annex I Part I, and the vulnerability handling of
            // Part II is in place for as long as it is supported.
            satisfied: |u, _| {
                u.products_conformity_assessed && u.coordinated_vulnerability_disclosure
            },
        },
        remedy: "assess the product against Annex I Part I, put the Part II vulnerability handling and a coordinated disclosure policy in place, and carry the CE marking",
    },
];

/// Judge one charge point against the whole calendar on one date.
#[must_use]
pub fn assess(point: &ChargePointProfile, on: Date) -> Assessment {
    let findings = CALENDAR
        .iter()
        .map(|obligation| {
            let status = match obligation.rule {
                Rule::Provider { .. } | Rule::Undertaking { .. } => Status::DifferentScope,
                Rule::ChargePoint {
                    applicable,
                    satisfied,
                } => judge(obligation, on, point, applicable, satisfied),
            };
            Finding {
                obligation: *obligation,
                status,
            }
        })
        .collect();

    Assessment {
        subject: point.evse_id.to_string(),
        scope: Scope::ChargePoint,
        on,
        findings,
    }
}

/// Judge one mobility service provider against the whole calendar on one date.
///
/// ```
/// use emob_core::obligation::{assess_provider, ObligationId, Status};
/// use emob_core::station::ProviderProfile;
/// use emob_core::PartyId;
/// use time::macros::date;
///
/// let mut provider = ProviderProfile::bare(PartyId::new("DE", "MSP")?);
/// provider.surcharges_cross_border_roaming = true;
///
/// let report = assess_provider(&provider, date!(2026-09-01));
/// assert_eq!(
///     report.status_of(ObligationId::AfirMspNoCrossBorderSurcharge),
///     Some(Status::Failing),
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn assess_provider(provider: &ProviderProfile, on: Date) -> Assessment {
    let findings = CALENDAR
        .iter()
        .map(|obligation| {
            let status = match obligation.rule {
                Rule::ChargePoint { .. } | Rule::Undertaking { .. } => Status::DifferentScope,
                Rule::Provider {
                    applicable,
                    satisfied,
                } => judge(obligation, on, provider, applicable, satisfied),
            };
            Finding {
                obligation: *obligation,
                status,
            }
        })
        .collect();

    Assessment {
        subject: provider.party.to_string(),
        scope: Scope::MobilityServiceProvider,
        on,
        findings,
    }
}

/// Judge one undertaking against the whole calendar on one date.
///
/// The third subject, and the one that carries the cybersecurity duties. An
/// operator whose every charge point is faultless and whose provider half
/// discloses everything can still be in breach here — `[NIS2 Anh. I]` names
/// charge point operators in the Energy sector, and none of what it asks is a
/// fact about a charge point.
///
/// ```
/// use emob_core::obligation::{assess_undertaking, ObligationId, Status};
/// use emob_core::station::{RiskManagement, UndertakingProfile};
/// use emob_core::PartyId;
/// use time::macros::date;
///
/// let mut operator = UndertakingProfile::bare(PartyId::new("DE", "CPO")?);
/// operator.operates_recharging_points = true;
/// operator.employees = 400;
/// operator.risk_management = RiskManagement::complete();
///
/// let report = assess_undertaking(&operator, date!(2026-09-01));
/// assert_eq!(
///     report.status_of(ObligationId::Nis2RiskManagement),
///     Some(Status::Satisfied),
/// );
/// assert_eq!(
///     report.status_of(ObligationId::Nis2Registration),
///     Some(Status::Failing),
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn assess_undertaking(undertaking: &UndertakingProfile, on: Date) -> Assessment {
    let findings = CALENDAR
        .iter()
        .map(|obligation| {
            let status = match obligation.rule {
                Rule::ChargePoint { .. } | Rule::Provider { .. } => Status::DifferentScope,
                Rule::Undertaking {
                    applicable,
                    satisfied,
                } => judge(obligation, on, undertaking, applicable, satisfied),
            };
            Finding {
                obligation: *obligation,
                status,
            }
        })
        .collect();

    Assessment {
        subject: undertaking.party.to_string(),
        scope: Scope::Undertaking,
        on,
        findings,
    }
}

/// The order the three questions are asked in: in force, then applicable, then
/// satisfied. Getting it wrong is how a duty reports a breach a year before it
/// exists.
fn judge<P>(
    obligation: &Obligation,
    on: Date,
    subject: &P,
    applicable: fn(&P) -> bool,
    satisfied: fn(&P, Date) -> bool,
) -> Status {
    if on < obligation.applies_from {
        Status::NotYetInForce
    } else if !obligation.in_force_on(on) {
        Status::NoLongerInForce
    } else if !applicable(subject) {
        Status::NotApplicable
    } else if satisfied(subject, on) {
        Status::Satisfied
    } else {
        Status::Failing
    }
}

/// Every obligation in force on a date, whatever it binds.
///
/// The planning query: what changes between today and a date, so a fleet
/// programme can be built from it.
pub fn in_force_on(on: Date) -> impl Iterator<Item = &'static Obligation> {
    CALENDAR.iter().filter(move |o| o.in_force_on(on))
}

/// Every obligation that starts binding strictly after `from` and no later
/// than `to`.
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
    use crate::ids::{EvseId, PartyId};
    use crate::station::{
        Accessibility, AdHocPayment, AdditionalPrices, ArbeitspreisIndication, ChargePointProfile,
        ConnectorType, CurrentType, DataPublication, EnergyMeasurementPoint, FurtherIdentifiers,
        MeteringPosture, Nis2Class, Notice, OperatorChange, Ownership, PriceTransparency,
        ProviderProfile, QuotaPosture, RegisterPublication, Registration, RiskManagement,
        UndertakingProfile, V2gCommunication,
    };
    use rust_decimal::Decimal;

    fn evse() -> EvseId {
        "DE*AB7*E840*6487".parse().unwrap()
    }

    fn status_of(assessment: &Assessment, id: ObligationId) -> Status {
        assessment
            .status_of(id)
            .expect("every obligation is judged")
    }

    /// A point that satisfies everything the calendar can ask of it.
    fn compliant_point(commissioned: Date) -> ChargePointProfile {
        let mut point = ChargePointProfile::bare(evse(), commissioned);
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(300);
        point.on_ten_t = true;
        point.ad_hoc_payment = AdHocPayment::CardReader;
        point.digitally_connected = true;
        point.smart_recharging_capable = true;
        point.fixed_cable = true;
        point.price_transparency = PriceTransparency {
            energy_based: true,
            shown_at_station: true,
            components_in_prescribed_order: true,
            arbeitspreis: ArbeitspreisIndication::PointDisplay,
            web_based_ad_hoc: true,
            arbeitspreis_stated_before_start: true,
            additional_prices: AdditionalPrices::StatedAdjacent,
        };
        point.connectors = vec![ConnectorType::Iec62196T2Combo];
        point.v2g = V2gCommunication::both_generations();
        point.offers_automatic_authentication = true;
        point.automatic_authentication_opt_out_offered = true;
        point.data = DataPublication {
            static_data: true,
            dynamic_data: true,
            open_api: true,
            datex2: true,
        };
        point.meets_technical_requirements = true;
        point.registration = Registration {
            technical_documentation_available: true,
            ..Registration::notified_on(commissioned)
        };
        point.metering = MeteringPosture {
            mid_conformity_assessed: true,
            signed_values: true,
            bills_by_energy: true,
            intervention_records_retained: true,
            ..MeteringPosture::default()
        };
        point.quota = QuotaPosture {
            publication: RegisterPublication::Published,
            conformity_declared: true,
            operator_code_assigned: true,
            further_identifiers: FurtherIdentifiers::NoneAnnounced,
        };
        point
    }

    #[test]
    fn an_estate_that_declines_a_subsidy_is_lawful() {
        // `compliant_point` claims the greenhouse-gas quota, and it was the
        // only compliant fixture in the suite — so the one arrangement that
        // tells the two kinds of rule apart was unreachable from anything the
        // tests could build (D219).
        //
        // An operator that meets every legal duty in Europe and simply does not
        // claim a German subsidy has broken no law. `verdict` used to call it
        // `Failing`, which is a false statement about a lawful estate and the
        // headline answer of the whole module.
        let mut point = compliant_point(date!(2027 - 03 - 01));
        point.quota = QuotaPosture::default();

        let report = assess(&point, date!(2027 - 06 - 01));
        assert_eq!(
            report.verdict(),
            Verdict::Compliant,
            "breaches: {:?}",
            report
                .breaches()
                .map(|f| f.obligation.id)
                .collect::<Vec<_>>()
        );

        // The quota is still reported — as money left on the table, which is
        // what it is.
        let forgone: Vec<_> = report.forgone().map(|f| f.obligation.id).collect();
        assert_eq!(forgone, vec![ObligationId::ThgEligibility]);
        // …and `failing` is still the union, for a caller that wants both.
        assert_eq!(report.failing().count(), 1);

        // The mirror: a real breach on the same estate does move the verdict.
        point.digitally_connected = false;
        let breached = assess(&point, date!(2027 - 06 - 01));
        assert_eq!(breached.verdict(), Verdict::Failing);
        assert!(
            breached
                .breaches()
                .any(|f| f.obligation.id == ObligationId::AfirDigitallyConnected)
        );
    }

    /// A date after every `applies_from` in the calendar, so nothing is judged
    /// `NotYetInForce`.
    fn after_everything() -> Date {
        CALENDAR
            .iter()
            .map(|obligation| obligation.applies_from)
            .max()
            .expect("the calendar is not empty")
    }

    /// An undertaking large enough to be in NIS2 scope, doing everything asked.
    fn compliant_undertaking() -> UndertakingProfile {
        let mut u = UndertakingProfile::bare(PartyId::new("DE", "CPO").unwrap());
        u.operates_recharging_points = true;
        u.places_digital_products_on_the_market = true;
        u.employees = 400;
        u.annual_turnover_eur = Decimal::from(60_000_000);
        u.balance_sheet_total_eur = Decimal::from(50_000_000);
        u.registered_with_the_authority = true;
        u.risk_management = RiskManagement::complete();
        u.can_warn_within_24_hours = true;
        u.management_approved_measures = true;
        u.management_trained = true;
        u.can_report_exploited_vulnerabilities = true;
        u.coordinated_vulnerability_disclosure = true;
        u.products_conformity_assessed = true;
        u
    }

    /// A provider disclosing everything and surcharging nothing.
    fn compliant_provider() -> ProviderProfile {
        let mut p = ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap());
        p.discloses_all_price_components = true;
        p.discloses_e_roaming_costs = true;
        p.discloses_electronically = true;
        p.surcharges_cross_border_roaming = false;
        p.holds_meter_operator_confirmation = true;
        p
    }

    /// **No duty in this calendar is decoration, and none of them is inverted.**
    ///
    /// Thirty-four of the forty entries were asserted somewhere in this module,
    /// each by the example that motivated it. Six were not — and an entry
    /// nothing exercises is one whose `satisfied` closure could read a field
    /// that is always true, or the wrong field, or the right one negated, and
    /// the calendar would report a clean estate either way. Rule 1, pointed at
    /// the table rather than at a `match` arm (D256).
    ///
    /// Two statements, and each catches a different way of being wrong:
    ///
    /// 1. **A subject that does everything fails nothing.** An inverted
    ///    `satisfied`, or one reading a field the compliant fixture cannot set,
    ///    shows up here for every duty at once.
    /// 2. **A subject that does nothing fails every duty that binds it.** A
    ///    `satisfied` that is trivially true — a duty nobody can breach — shows
    ///    up here, and that is the shape a decorative entry takes.
    ///
    /// Judged on a date after the last `applies_from`, so the answer is about
    /// the rule rather than about the calendar.
    #[test]
    fn every_duty_can_be_failed_and_every_duty_can_be_met() {
        let on = after_everything();

        // ── 1. Doing everything fails nothing ──────────────────────────────
        for (scope, failing) in [
            (
                "charge point",
                assess(&compliant_point(date!(2027 - 06 - 01)), on)
                    .failing()
                    .map(|f| f.obligation.id)
                    .collect::<Vec<_>>(),
            ),
            (
                "provider",
                assess_provider(&compliant_provider(), on)
                    .failing()
                    .map(|f| f.obligation.id)
                    .collect::<Vec<_>>(),
            ),
            (
                "undertaking",
                assess_undertaking(&compliant_undertaking(), on)
                    .failing()
                    .map(|f| f.obligation.id)
                    .collect::<Vec<_>>(),
            ),
        ] {
            assert!(
                failing.is_empty(),
                "a {scope} that does everything the calendar asks still fails {failing:?}"
            );
        }

        // ── 2. Every duty can be breached by somebody ──────────────────────
        //
        // A bare subject is the obvious witness for a duty phrased as *do this*,
        // and it is the wrong one for a duty phrased as *do not do this*: a
        // point that has not discriminated on price and a provider that has not
        // surcharged a cross-border session both satisfy their duty by doing
        // nothing at all, correctly. So the panel carries a subject that does
        // the forbidden thing as well as ones that omit the required thing, and
        // the statement is about the calendar rather than about any one row:
        // **for every entry, some subject here fails it.**
        //
        // An entry no subject can fail judges nothing, whatever it says.
        // A fast public DC charger on the TEN-T, billing by the kilowatt-hour
        // and metering before its own rectifier — every condition in the
        // calendar met, and none of the duties they turn on done. A bare
        // profile is an 11 kW AC point, so most of AFIR simply does not bind it
        // and "it fails nothing" would be an answer about applicability rather
        // than about the rules.
        let mut fast = ChargePointProfile::bare(evse(), date!(2027 - 06 - 01));
        fast.current_type = CurrentType::Dc;
        fast.rated_power_kw = Decimal::from(300);
        fast.on_ten_t = true;
        fast.on_safe_secure_parking = true;
        fast.offers_automatic_authentication = true;
        fast.ownership = Ownership::ThirdPartyWithholding;
        fast.metering = MeteringPosture {
            bills_by_energy: true,
            measurement_point: EnergyMeasurementPoint::AcBeforeRectifier,
            ..MeteringPosture::default()
        };
        fast.registration = Registration {
            decommissioning: Some(Notice::unreported(date!(2027 - 05 - 01))),
            operator_change: Some(OperatorChange {
                happened_on: date!(2027 - 04 - 01),
                notified_by_previous_operator_on: None,
                notified_by_new_operator_on: None,
            }),
            ..Registration::default()
        };
        fast.price_conduct = crate::station::PriceConduct {
            differentiates_between_providers: true,
            differentiation_is_justified: false,
        };
        // The same, deployed **before** 13.04.2024 — the population the retrofit
        // limb binds and the first subparagraph does not.
        let mut legacy = fast.clone();
        legacy.commissioned_on = date!(2023 - 01 - 01);

        // …and a private wall box installed from 2027, which is the whole
        // subject of the DA-656 duty that reaches behind the fence.
        let mut private = ChargePointProfile::bare(evse(), date!(2027 - 06 - 01));
        private.accessibility = Accessibility::Private;

        // …and a point whose meter has simply aged out. No other subject here
        // reaches it: `bare` commissions the point on the day it is judged, and
        // an eight-year verification period `[MessEV Anl. 7 Nr. 6.7]` cannot
        // have expired on a meter that new.
        let mut aged = compliant_point(date!(2027 - 06 - 01));
        aged.placed_on_market_on = Some(date!(2010 - 01 - 01));

        // …and the post `[PAngV §14]` is about, which no other subject here
        // reaches: one that *does* take a card and *does* charge a blocking
        // fee, and states neither price anywhere. `fast` cannot stand in for
        // it, because a point with no ad-hoc payment method at all is outside
        // § 14(2)'s third limb and the duty never binds it.
        let mut silent = ChargePointProfile::bare(evse(), date!(2027 - 06 - 01));
        silent.ad_hoc_payment = AdHocPayment::CardReader;
        silent.price_transparency = PriceTransparency {
            web_based_ad_hoc: true,
            additional_prices: AdditionalPrices::NotStated,
            ..PriceTransparency::default()
        };

        let mut surcharging = ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap());
        surcharging.surcharges_cross_border_roaming = true;

        let panel = [
            assess(&ChargePointProfile::bare(evse(), date!(2027 - 06 - 01)), on),
            assess(&fast, on),
            assess(&legacy, on),
            assess(&private, on),
            assess(&silent, on),
            assess(&aged, on),
            assess_provider(
                &ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap()),
                on,
            ),
            assess_provider(&surcharging, on),
            assess_undertaking(&undertaking_doing_nothing(), on),
        ];
        let breachable: std::collections::BTreeSet<ObligationId> = panel
            .iter()
            .flat_map(Assessment::failing)
            .map(|finding| finding.obligation.id)
            .collect();

        let missing: Vec<ObligationId> = CALENDAR
            .iter()
            .map(|obligation| obligation.id)
            .filter(|id| !breachable.contains(id))
            .collect();
        assert!(
            missing.is_empty(),
            "no subject in this panel can fail {missing:?}, so those entries judge nothing"
        );
    }

    /// In NIS2 scope by size, and doing none of what the Directive asks.
    fn undertaking_doing_nothing() -> UndertakingProfile {
        let mut u = UndertakingProfile::bare(PartyId::new("DE", "CPO").unwrap());
        u.operates_recharging_points = true;
        u.places_digital_products_on_the_market = true;
        u.employees = 400;
        u.annual_turnover_eur = Decimal::from(60_000_000);
        u.balance_sheet_total_eur = Decimal::from(50_000_000);
        u
    }

    #[test]
    fn exactly_one_rule_in_the_calendar_is_not_a_duty() {
        // A distinction is worth having only when something acts on it, and
        // `verdict` acts on this one. Stated as a test so that adding a second
        // entitlement — a future subsidy, a grant precondition — is a decision
        // somebody makes on purpose rather than a default that spreads.
        let entitlements: Vec<_> = CALENDAR
            .iter()
            .filter(|o| !o.consequence.is_breach())
            .map(|o| o.id)
            .collect();
        assert_eq!(entitlements, vec![ObligationId::ThgEligibility]);

        // …and every remedy for an entitlement says what forgoing it costs,
        // because "or forgo the quota" is the half that makes it optional.
        for obligation in CALENDAR.iter().filter(|o| !o.consequence.is_breach()) {
            assert!(
                obligation.remedy.contains("forgo"),
                "{}: an entitlement's remedy has to say it is one",
                obligation.id
            );
        }
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
    fn a_superseded_duty_does_not_read_as_an_upcoming_one() {
        // The two ends of a window are opposite instructions, and reporting
        // both as "not yet in force" tells a fleet programme to prepare for
        // something that will never bind again. `applies_until` is the field
        // that makes the difference, so this is judged through `judge` on an
        // obligation that carries one — no duty in the calendar is superseded
        // yet, and the first one that is must not be the test.
        let point = ChargePointProfile::bare(evse(), date!(2020 - 01 - 01));
        let superseded = Obligation {
            consequence: Consequence::Breach,
            id: ObligationId::AfirDatex2,
            title: "a duty with both ends",
            citation: "[AFIR Art. 20(2)(c)]",
            applies_from: date!(2026 - 01 - 01),
            applies_until: Some(date!(2026 - 12 - 31)),
            rule: Rule::ChargePoint {
                applicable: |_| true,
                satisfied: |_, _| false,
            },
            remedy: "none",
        };
        let Rule::ChargePoint {
            applicable,
            satisfied,
        } = superseded.rule
        else {
            unreachable!("constructed as a charge-point rule")
        };

        let judged = |on| judge(&superseded, on, &point, applicable, satisfied);
        assert_eq!(judged(date!(2025 - 12 - 31)), Status::NotYetInForce);
        assert_eq!(judged(date!(2026 - 06 - 01)), Status::Failing);
        assert_eq!(
            judged(date!(2026 - 12 - 31)),
            Status::Failing,
            "`applies_until` is the last day it binds, inclusive"
        );
        assert_eq!(judged(date!(2027 - 01 - 01)), Status::NoLongerInForce);
    }

    #[test]
    fn a_private_point_is_not_failing_the_public_duties() {
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.accessibility = Accessibility::Private;
        let report = assess(&point, date!(2026 - 09 - 01));
        for duty in [
            ObligationId::AfirAdHocAccess,
            ObligationId::AfirStaticData,
            ObligationId::AfirDigitallyConnected,
            ObligationId::Lsv2026CommissioningNotice,
        ] {
            assert_eq!(
                status_of(&report, duty),
                Status::NotApplicable,
                "{duty} must not bind a depot"
            );
        }
    }

    #[test]
    fn a_renovation_does_not_deploy_a_point_a_second_time() {
        // The correction that matters most in this table: AFIR Art. 5(1) and
        // 5(4) say "deployed from", not "newly installed or renovated". Reading
        // a renovation as a deployment drags untouched hardware into duties
        // written for new equipment.
        let mut point = ChargePointProfile::bare(evse(), date!(2019 - 01 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(150);
        point.renovated_on = Some(date!(2026 - 03 - 01));

        let report = assess(&point, date!(2026 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirPaymentInstrument),
            Status::NotApplicable,
            "a 2019 point renovated in 2026 was still deployed in 2019"
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirEnergyBasedAdHocPrice),
            Status::NotApplicable
        );

        // …but the duties whose text *does* say "renovated" pick it up.
        assert_eq!(
            status_of(&report, ObligationId::AfirSmartRecharging),
            Status::Failing,
            "Art. 5(8) says 'renovated after 14 October 2024' in as many words"
        );
        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 06 - 01)),
                ObligationId::Da656Iso15118Dash20Public
            ),
            Status::NotApplicable,
            "renovated in March 2026, so the 2027 generation does not reach it"
        );
    }

    #[test]
    fn a_free_charge_point_owes_no_payment_instrument_and_no_dynamic_data() {
        // "shall not apply to publicly accessible recharging points that do not
        // require payment for the recharging service" [AFIR Art. 5(1)], and the
        // same words again for the dynamic data of [AFIR Art. 20(2)(c)].
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.rated_power_kw = Decimal::from(150);
        point.requires_payment = false;

        let report = assess(&point, date!(2027 - 06 - 01));
        for duty in [
            ObligationId::AfirPaymentInstrument,
            ObligationId::AfirPaymentInstrumentRetrofit,
            ObligationId::AfirEnergyBasedAdHocPrice,
            ObligationId::AfirPriceShownAtStation,
            ObligationId::AfirDynamicData,
        ] {
            assert_eq!(
                status_of(&report, duty),
                Status::NotApplicable,
                "{duty} must not bind a point that charges nothing"
            );
        }
        // …and the ad-hoc *access* duty is satisfied rather than failing: a
        // free charger is contract-free by construction.
        assert_eq!(
            status_of(&report, ObligationId::AfirAdHocAccess),
            Status::Satisfied
        );
        // The static-data duty has no such exemption.
        assert_eq!(
            status_of(&report, ObligationId::AfirStaticData),
            Status::Failing
        );
    }

    #[test]
    fn a_qr_code_satisfies_the_duty_below_fifty_kilowatts_only() {
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
        // whole purpose of the subparagraph. Asserting the fixture's own date
        // back at itself proves nothing about the rule; what proves it is that
        // the *other* payment duty does not reach the same point.
        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 06 - 01)),
                ObligationId::AfirPaymentInstrument
            ),
            Status::NotApplicable,
            "the first subparagraph binds points deployed from 13.04.2024"
        );
    }

    #[test]
    fn the_two_payment_duties_partition_the_estate_at_the_regulations_own_date() {
        // Art. 5(1) states the duty twice: once for points deployed **from**
        // 13.04.2024, and once — from 01.01.2027 — for the ≥ 50 kW TEN-T and
        // safe-parking points deployed **before** it. Every fixture for the
        // retrofit was a 2018 point, so the arrangement where the two overlap
        // was unreachable and the date limb could go missing without any test
        // noticing: one missing card reader then reported as two failing
        // findings with one remedy, over a population the Regulation does not
        // put the retrofit on (D215).
        let judged_on = date!(2027 - 06 - 01);
        for (commissioned, expect_first, expect_retrofit) in [
            (
                date!(2018 - 01 - 01),
                Status::NotApplicable,
                Status::Failing,
            ),
            (
                date!(2024 - 04 - 12),
                Status::NotApplicable,
                Status::Failing,
            ),
            (
                date!(2024 - 04 - 13),
                Status::Failing,
                Status::NotApplicable,
            ),
            (
                date!(2026 - 06 - 01),
                Status::Failing,
                Status::NotApplicable,
            ),
        ] {
            let mut point = ChargePointProfile::bare(evse(), commissioned);
            point.rated_power_kw = Decimal::from(350);
            point.ad_hoc_payment = AdHocPayment::QrCode; // never enough at 350 kW
            point.on_ten_t = true;

            let report = assess(&point, judged_on);
            assert_eq!(
                status_of(&report, ObligationId::AfirPaymentInstrument),
                expect_first,
                "first subparagraph, deployed {commissioned}"
            );
            assert_eq!(
                status_of(&report, ObligationId::AfirPaymentInstrumentRetrofit),
                expect_retrofit,
                "2027 retrofit, deployed {commissioned}"
            );

            // Exactly one of the two binds it: no point owes the card reader
            // twice, and none escapes owing it at all.
            let binding = [
                status_of(&report, ObligationId::AfirPaymentInstrument),
                status_of(&report, ObligationId::AfirPaymentInstrumentRetrofit),
            ]
            .into_iter()
            .filter(|status| *status != Status::NotApplicable)
            .count();
            assert_eq!(binding, 1, "deployed {commissioned}");
        }
    }

    #[test]
    fn a_fast_charger_may_not_price_by_the_minute_alone() {
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
    fn the_ordered_components_duty_reaches_the_whole_installed_base() {
        // The fourth subparagraph of Art. 5(4) limits the *first and second*
        // subparagraphs to points deployed from 13.04.2024. The third — the
        // ordered components below 50 kW — carries no such limit.
        let old = ChargePointProfile::bare(evse(), date!(2015 - 01 - 01));
        assert_eq!(
            status_of(
                &assess(&old, date!(2026 - 09 - 01)),
                ObligationId::AfirPriceComponentsInOrder
            ),
            Status::Failing
        );
    }

    #[test]
    fn a_newly_installed_pwm_only_point_fails_da656_rather_than_escaping_it() {
        // The correction this table exists to carry: the DA-656 exemption for
        // legacy low-level-communication points is the *date*, not the
        // technology. Testing for PWM in the applicability let a point escape
        // the duty precisely because it failed it.
        let mut fresh = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        fresh.v2g = V2gCommunication::pwm_only();
        assert_eq!(
            status_of(
                &assess(&fresh, date!(2026 - 09 - 01)),
                ObligationId::Da656Iso15118Dash2
            ),
            Status::Failing,
            "a PWM-only point installed after 08.01.2026 is the non-compliant case"
        );

        // An existing point is out of scope, by date.
        let legacy = ChargePointProfile::bare(evse(), date!(2019 - 01 - 01));
        assert_eq!(
            status_of(
                &assess(&legacy, date!(2026 - 09 - 01)),
                ObligationId::Da656Iso15118Dash2
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn the_iso15118_20_duty_binds_only_new_points_and_reaches_private_ones() {
        // A 2019 point that already speaks -2 is *not* dragged into the 2027
        // duty: 2.1.2 binds points "installed or renovated from 1 January 2027".
        let mut existing = ChargePointProfile::bare(evse(), date!(2019 - 01 - 01));
        existing.v2g = V2gCommunication {
            pwm: true,
            din70121: false,
            iso15118_2: true,
            iso15118_20: false,
        };
        assert_eq!(
            status_of(
                &assess(&existing, date!(2027 - 06 - 01)),
                ObligationId::Da656Iso15118Dash20Public
            ),
            Status::NotApplicable
        );

        // A point installed in 2027 is.
        let fresh = ChargePointProfile::bare(evse(), date!(2027 - 03 - 01));
        assert_eq!(
            status_of(
                &assess(&fresh, date!(2027 - 06 - 01)),
                ObligationId::Da656Iso15118Dash20Public
            ),
            Status::Failing
        );

        // A private wall box installed in 2027 is bound by 2.1.3(b)…
        let mut depot = ChargePointProfile::bare(evse(), date!(2027 - 03 - 01));
        depot.accessibility = Accessibility::Private;
        let report = assess(&depot, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Da656Iso15118Dash20Private),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::Da656Iso15118Dash20Public),
            Status::NotApplicable
        );

        // …and a domestic socket in the same garage is not: that is Mode 2, and
        // 2.1.3(a) asks it for EN IEC 61851-1 instead.
        let mut socket = depot.clone();
        socket.domestic_socket = true;
        assert_eq!(
            status_of(
                &assess(&socket, date!(2027 - 06 - 01)),
                ObligationId::Da656Iso15118Dash20Private
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn automatic_authentication_carries_two_duties() {
        let mut point = ChargePointProfile::bare(evse(), date!(2027 - 03 - 01));
        let report = assess(&point, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirAutomaticAuthenticationOptOut),
            Status::NotApplicable
        );
        assert_eq!(
            status_of(
                &report,
                ObligationId::Da656AutomaticAuthenticationBothGenerations
            ),
            Status::NotApplicable
        );

        // Offering it brings both: the opt-out of Art. 5(2) and the
        // both-generations rule of DA-656 2.1.2. AutoCharge counts as automatic
        // authentication just as Plug & Charge does.
        point.offers_automatic_authentication = true;
        point.v2g = V2gCommunication {
            pwm: true,
            din70121: false,
            iso15118_2: false,
            iso15118_20: true,
        };
        let report = assess(&point, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirAutomaticAuthenticationOptOut),
            Status::Failing
        );
        assert_eq!(
            status_of(
                &report,
                ObligationId::Da656AutomaticAuthenticationBothGenerations
            ),
            Status::Failing,
            "a PnC point may not drop -2"
        );
    }

    #[test]
    fn both_paragraphs_the_regulation_names_for_monitoring_are_assessable() {
        // `[AFIR Art. 5(6)]`: "Member States shall ensure that their
        // authorities regularly monitor … the compliance of operators of
        // recharging points and mobility service providers with paragraphs 3
        // and 5." Two paragraphs, two subjects — and a calendar carrying only
        // the provider half checks half of what the regulator was told to look
        // at.
        let point = compliant_point(date!(2027 - 03 - 01));
        let provider = ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap());

        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 06 - 01)),
                ObligationId::AfirNonDiscriminatoryPricing
            ),
            Status::Satisfied,
            "paragraph 3 binds the operator"
        );
        assert_eq!(
            status_of(
                &assess_provider(&provider, date!(2027 - 06 - 01)),
                ObligationId::AfirMspPriceDisclosure
            ),
            Status::Failing,
            "paragraph 5 binds the provider"
        );
    }

    #[test]
    fn price_differentiation_is_lawful_only_when_it_is_justified() {
        // The article does not forbid differentiation, it conditions it:
        // "the level of prices may be differentiated, but only if the
        // differentiation is proportionate and objectively justified."
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirNonDiscriminatoryPricing
            ),
            Status::Satisfied,
            "not discriminating is the default and the compliant state"
        );

        point.price_conduct.differentiates_between_providers = true;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirNonDiscriminatoryPricing
            ),
            Status::Failing
        );

        point.price_conduct.differentiation_is_justified = true;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirNonDiscriminatoryPricing
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn a_host_owned_charger_puts_a_duty_on_its_owner() {
        // `[AFIR Art. 5(11)]`, the paragraph nobody models. A hotel buys the
        // charger, a CPO operates it, and the CPO is held to 5(2), (7), (8) and
        // (10) on equipment it cannot change — unless the arrangement says
        // otherwise, which is what the article requires and what an operator
        // has to have negotiated.
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirOwnerEnablesCompliance
            ),
            Status::NotApplicable,
            "an operator that owns its own hardware has no counterparty here"
        );

        point.ownership = Ownership::ThirdPartyWithholding;
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirOwnerEnablesCompliance),
            Status::Failing
        );
        assert!(
            report
                .failing()
                .any(|f| f.obligation.remedy.contains("arrangement with the owner")),
            "the remedy has to name the contract, because the fix is not technical"
        );

        point.ownership = Ownership::ThirdPartyEnabling;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirOwnerEnablesCompliance
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn the_three_duties_the_regulator_audits_are_in_the_table() {
        // [LSV26 §5] names Art. 5(1), (2), (7), (8) and (10) as what the
        // Bundesnetzagentur may inspect and close a point over. (7), (8) and
        // (10) are the ones compliance models normally omit.
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 06 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(150);

        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirDigitallyConnected),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirSmartRecharging),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirFixedCableOnDc),
            Status::Failing
        );

        // An AC point owes no fixed cable.
        point.current_type = CurrentType::Ac;
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::AfirFixedCableOnDc
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn the_smart_recharging_duty_has_two_dates_in_one_sentence() {
        // "built after 13 April 2024 or renovated after 14 October 2024".
        let exactly_on_the_day = ChargePointProfile::bare(evse(), date!(2024 - 04 - 13));
        assert_eq!(
            status_of(
                &assess(&exactly_on_the_day, date!(2026 - 09 - 01)),
                ObligationId::AfirSmartRecharging
            ),
            Status::NotApplicable,
            "'after' is strict"
        );

        let day_after = ChargePointProfile::bare(evse(), date!(2024 - 04 - 14));
        assert_eq!(
            status_of(
                &assess(&day_after, date!(2026 - 09 - 01)),
                ObligationId::AfirSmartRecharging
            ),
            Status::Failing
        );

        // An old point renovated between the two dates is *not* caught: the
        // renovation limb runs from 14 October 2024.
        let mut renovated_early = ChargePointProfile::bare(evse(), date!(2015 - 01 - 01));
        renovated_early.renovated_on = Some(date!(2024 - 06 - 01));
        assert_eq!(
            status_of(
                &assess(&renovated_early, date!(2026 - 09 - 01)),
                ObligationId::AfirSmartRecharging
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn the_lsv_notice_is_a_deadline_rather_than_a_flag() {
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 03 - 01));
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Lsv2026CommissioningNotice),
            Status::Failing
        );

        // Filed three weeks late: reported, and still a breach.
        point.registration = Registration::notified_on(date!(2026 - 03 - 22));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026CommissioningNotice
            ),
            Status::Failing,
            "§ 4(1) Nr. 1 LSV gives two weeks, and § 5(3) can close the point over it"
        );

        point.registration = Registration::notified_on(date!(2026 - 03 - 10));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026CommissioningNotice
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn the_lsv_carries_its_primary_duty_and_not_only_the_regulator_s_powers() {
        // § 5(1)–(3) let the regulator inspect, demand a retrofit for and
        // forbid the operation of a point over "eine technische Anforderung
        // nach § 3". The calendar named the powers and omitted the duty.
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 03 - 01));
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Lsv2026TechnicalRequirements),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::Lsv2026TechnicalEvidence),
            Status::Failing,
            "§ 4(2): being compliant and being able to prove it are two duties"
        );

        point.meets_technical_requirements = true;
        point.registration.technical_documentation_available = true;
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Lsv2026TechnicalRequirements),
            Status::Satisfied
        );
        assert_eq!(
            status_of(&report, ObligationId::Lsv2026TechnicalEvidence),
            Status::Satisfied
        );
    }

    #[test]
    fn a_depot_that_opens_to_the_public_owes_its_notice_from_that_day() {
        // `[LSV26 §4(3)]`: the régime applies afresh "wenn ein bestehender
        // Ladepunkt öffentlich zugänglich wird". Reading the deadline off the
        // commissioning date alone reports a 2019 depot charger as seven years
        // late on the day it opens — and then never lets it become compliant.
        let mut point = ChargePointProfile::bare(evse(), date!(2019 - 05 - 01));
        point.became_publicly_accessible_on = Some(date!(2026 - 03 - 01));
        point.registration = Registration::notified_on(date!(2026 - 03 - 10));

        assert_eq!(point.notifiable_commissioning_date(), date!(2026 - 03 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026CommissioningNotice
            ),
            Status::Satisfied,
            "filed nine days after the point became public, which is the event § 4(3) names"
        );

        // A point that was public from the start still reads its commissioning
        // date, and one that opened later and filed late is still late.
        point.registration = Registration::notified_on(date!(2026 - 04 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026CommissioningNotice
            ),
            Status::Failing
        );
    }

    #[test]
    fn an_operator_change_takes_two_notices_and_one_is_not_enough() {
        // "Bei einem Betreiberwechsel haben Anzeigen nach Satz 1 durch den
        // bisherigen **und** den neuen Betreiber zu erfolgen." An incoming
        // operator that files its own and assumes the outgoing one did the
        // same has a point § 5(3) may close, over a notice it never saw.
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 01 - 01));
        point.registration = Registration::notified_on(date!(2026 - 01 - 02));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026OperatorChangeNotice
            ),
            Status::NotApplicable,
            "a point that never changed hands owes nothing here"
        );

        point.registration.operator_change = Some(OperatorChange {
            happened_on: date!(2026 - 06 - 01),
            notified_by_previous_operator_on: None,
            notified_by_new_operator_on: Some(date!(2026 - 06 - 03)),
        });
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::Lsv2026OperatorChangeNotice),
            Status::Failing,
            "the incoming operator filed; the outgoing one did not"
        );
        assert!(
            report
                .failing()
                .any(|f| f.obligation.remedy.contains("rather than assuming it")),
            "the remedy has to say whose notice is missing, because it is not yours"
        );

        point
            .registration
            .operator_change
            .as_mut()
            .unwrap()
            .notified_by_previous_operator_on = Some(date!(2026 - 06 - 02));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026OperatorChangeNotice
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn a_decommissioning_nobody_reported_leaves_a_point_live_in_the_register() {
        let mut point = ChargePointProfile::bare(evse(), date!(2026 - 01 - 01));
        point.registration = Registration::notified_on(date!(2026 - 01 - 02));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026DecommissioningNotice
            ),
            Status::NotApplicable
        );

        point.registration.decommissioning = Some(Notice::unreported(date!(2026 - 06 - 01)));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026DecommissioningNotice
            ),
            Status::Failing
        );

        point.registration.decommissioning = Some(Notice::reported_on(
            date!(2026 - 06 - 01),
            date!(2026 - 06 - 04),
        ));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::Lsv2026DecommissioningNotice
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn a_notice_reports_how_late_it_was_rather_than_only_that_it_was() {
        // "Unverzüglich" has no number in the text, so the window is a
        // documented choice — and the delay is exposed so a deployment with a
        // stricter policy can act on it without re-deriving the arithmetic.
        let filed = Notice::reported_on(date!(2026 - 06 - 01), date!(2026 - 06 - 20));
        assert!(!filed.is_timely(Registration::PROMPT_NOTIFICATION_DAYS));
        assert_eq!(
            filed.delay_days(Registration::PROMPT_NOTIFICATION_DAYS),
            Some(5)
        );

        // A planned decommissioning announced in advance is early, not wrong.
        let early = Notice::reported_on(date!(2026 - 06 - 01), date!(2026 - 05 - 20));
        assert!(early.is_timely(Registration::PROMPT_NOTIFICATION_DAYS));
        assert!(
            early
                .delay_days(Registration::PROMPT_NOTIFICATION_DAYS)
                .is_some_and(|d| d < 0)
        );

        assert_eq!(
            Notice::unreported(date!(2026 - 06 - 01))
                .delay_days(Registration::PROMPT_NOTIFICATION_DAYS),
            None,
            "a notice nobody filed has no lateness, it has an absence"
        );
    }

    #[test]
    fn the_annex_the_regulator_can_close_a_point_over_is_in_the_table() {
        // `[LSV26 §5]` names "Artikel 5 Absatz 1, 2, 7, 8 und 10 **und Anhang
        // II**" three times over — inspection, retrofit order, and the power to
        // forbid the operation of a point. The calendar carried Article 5 and
        // not the Annex, so the one requirement that can shut a station was the
        // one nothing judged.
        let on = date!(2026 - 06 - 01);

        // A DC post that serves every Japanese vehicle on the road and no
        // European one. Lawful hardware, unlawful as the *only* interface:
        // Annex II says "at least … Combo 2".
        let mut chademo_only = compliant_point(date!(2025 - 01 - 01));
        chademo_only.connectors = vec![ConnectorType::Chademo];
        assert_eq!(
            status_of(
                &assess(&chademo_only, on),
                ObligationId::AfirAnnexIiConnector
            ),
            Status::Failing
        );

        // The same post with CCS2 beside it: "at least" is a floor, so the
        // extra interface is not an objection.
        chademo_only.connectors = vec![ConnectorType::Chademo, ConnectorType::Iec62196T2Combo];
        assert_eq!(
            status_of(
                &assess(&chademo_only, on),
                ObligationId::AfirAnnexIiConnector
            ),
            Status::Satisfied
        );

        // An AC post with a Schuko and nothing else — the shape a great many
        // older German kerbside points have.
        let mut schuko = compliant_point(date!(2025 - 01 - 01));
        schuko.current_type = CurrentType::Ac;
        schuko.rated_power_kw = Decimal::from(11);
        schuko.connectors = vec![ConnectorType::DomesticF];
        assert_eq!(
            status_of(&assess(&schuko, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );

        // …and the same socket on a 3.7 kW post **reserved for** L-category
        // vehicles is `[AFIR Anh. II 1.3]`'s own answer, not a breach. Without
        // the applicability limb this row would report a lawful scooter post as
        // failing a rule written for cars.
        let mut scooter = schuko.clone();
        scooter.reserved_for_l_category = true;
        scooter.rated_power_kw = Decimal::from_str_exact("3.7").unwrap();
        assert_eq!(
            status_of(&assess(&scooter, on), ObligationId::AfirAnnexIiConnector),
            Status::Satisfied
        );

        // The ceiling in that sentence is load-bearing: an 11 kW post is not an
        // L-category post however it is reserved, and falls back to Type 2.
        scooter.rated_power_kw = Decimal::from(11);
        assert_eq!(
            status_of(&assess(&scooter, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );

        // An inventory that recorded no interface at all cannot be shown to the
        // regulator `[LSV26 §5(1)]`, so it is unmet rather than vacuously met.
        let mut unrecorded = compliant_point(date!(2025 - 01 - 01));
        unrecorded.connectors.clear();
        assert_eq!(
            status_of(&assess(&unrecorded, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );
    }

    #[test]
    fn the_annex_was_replaced_and_the_replacement_carries_its_own_population() {
        // `[DA-656]` replaces Annex II points 1.1 to 1.4 from 08.01.2026, and
        // the two things it changes are written into limbs reading "installed
        // or renovated from 8 January 2026". Reading the consolidated 2023 text
        // alone gets both wrong, and reading the amended one over the whole
        // estate grants an exemption to a population it does not reach.
        let on = date!(2026 - 06 - 01);

        // A 3 kW public post that is an ordinary household socket with an
        // in-cable control box — Mode 2, the shape a great many kerbside and
        // workplace points have.
        let mut socket = compliant_point(date!(2024 - 06 - 01));
        socket.current_type = CurrentType::Ac;
        socket.domestic_socket = true;
        socket.rated_power_kw = Decimal::from(3);
        socket.connectors = vec![ConnectorType::DomesticF];
        assert_eq!(socket.mode(), ChargingMode::Mode2);

        // Installed in 2024, so the 2023 sentence still governs it: AC points
        // "shall be equipped at least with socket outlets or vehicle connectors
        // of Type 2", with no Mode 2 carve-out anywhere in the text.
        assert_eq!(
            status_of(&assess(&socket, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );

        // The same hardware installed after the amendment applies takes
        // `[DA-656 Anh. 1.1]`'s own exemption — "if their power is less than or
        // equal to 3,7 kW and their primary purpose is the recharging of
        // electric vehicles in Mode 2, with socket-outlets compliant with
        // standard IEC 60884-1:2022".
        socket.commissioned_on = date!(2026 - 03 - 01);
        assert_eq!(
            status_of(&assess(&socket, on), ObligationId::AfirAnnexIiConnector),
            Status::Satisfied
        );

        // …and a renovation reaches it too, because the limb says "installed
        // **or renovated** from" and `[DA-656 Anh. 0(b)]` defines a renovation
        // as "a major or complete replacement of relevant recharging point
        // equipment".
        socket.commissioned_on = date!(2024 - 06 - 01);
        socket.renovated_on = Some(date!(2026 - 03 - 01));
        assert_eq!(
            status_of(&assess(&socket, on), ObligationId::AfirAnnexIiConnector),
            Status::Satisfied
        );

        // The exemption is bounded by its own power band and by its own mode.
        // A 7 kW wall box is a dedicated EVSE in Mode 3 and owes Type 2
        // whatever it was installed with.
        let mut wall_box = socket.clone();
        wall_box.domestic_socket = false;
        wall_box.rated_power_kw = Decimal::from(7);
        assert_eq!(
            status_of(&assess(&wall_box, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );

        // `[DA-656 Anh. 1.3.2]`: an L-category point **above** 3.7 kW is back on
        // Type 2, so the exception is a power band and not a vehicle class.
        let mut scooter = compliant_point(date!(2026 - 03 - 01));
        scooter.current_type = CurrentType::Ac;
        scooter.reserved_for_l_category = true;
        scooter.rated_power_kw = Decimal::from(11);
        scooter.connectors = vec![ConnectorType::Iec62196T3A];
        assert_eq!(
            status_of(&assess(&scooter, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );
        scooter.connectors = vec![ConnectorType::Iec62196T2];
        assert_eq!(
            status_of(&assess(&scooter, on), ObligationId::AfirAnnexIiConnector),
            Status::Satisfied
        );

        // `[DA-656 Anh. 1.3.3]`: a DC L-category point is Combo 2 like every
        // other DC point, so the L-category branch never reaches direct current.
        let mut dc_scooter = compliant_point(date!(2026 - 03 - 01));
        dc_scooter.reserved_for_l_category = true;
        dc_scooter.rated_power_kw = Decimal::from(3);
        dc_scooter.connectors = vec![ConnectorType::DomesticF];
        assert_eq!(
            status_of(&assess(&dc_scooter, on), ObligationId::AfirAnnexIiConnector),
            Status::Failing
        );
    }

    #[test]
    fn the_german_price_indication_is_older_and_wider_than_the_european_one() {
        // The claim this row exists to make: on the day AFIR's price duties did
        // not yet exist, `[PAngV §14(2)]` already bound the same post — and it
        // binds it at 11 kW, where `[AFIR Art. 5(4)]`'s first two subparagraphs
        // never reach.
        let before_afir = date!(2023 - 06 - 01);

        let mut post = compliant_point(date!(2022 - 06 - 01));
        post.current_type = CurrentType::Ac;
        post.rated_power_kw = Decimal::from(11);
        post.connectors = vec![ConnectorType::Iec62196T2];
        post.price_transparency.arbeitspreis = ArbeitspreisIndication::NotIndicated;

        let report = assess(&post, before_afir);
        assert_eq!(
            status_of(&report, ObligationId::PangvArbeitspreisAtThePoint),
            Status::Failing,
            "the Arbeitspreis duty binds an 11 kW post from 28.05.2022"
        );
        // …and the European duty that looks like it does not, on either limb.
        assert_eq!(
            status_of(&report, ObligationId::AfirEnergyBasedAdHocPrice),
            Status::NotYetInForce
        );

        // Any one of the three media answers it. None of them is "better".
        for medium in [
            ArbeitspreisIndication::Printed,
            ArbeitspreisIndication::PointDisplay,
            ArbeitspreisIndication::SignpostedMobilePage,
        ] {
            post.price_transparency.arbeitspreis = medium;
            assert_eq!(
                status_of(
                    &assess(&post, before_afir),
                    ObligationId::PangvArbeitspreisAtThePoint
                ),
                Status::Satisfied,
                "{medium:?} is one of § 14(2)'s three"
            );
        }

        // A point that takes no ad-hoc payment at all is outside the third limb
        // — the duty arises "beim Einsatz eines für das punktuelle Aufladen
        // vorgesehenen Bezahlverfahrens" — and so is a free one `[PAngV §1(1)]`.
        post.price_transparency.arbeitspreis = ArbeitspreisIndication::NotIndicated;
        let mut contract_only = post.clone();
        contract_only.ad_hoc_payment = AdHocPayment::None;
        assert_eq!(
            status_of(
                &assess(&contract_only, before_afir),
                ObligationId::PangvArbeitspreisAtThePoint
            ),
            Status::NotApplicable
        );
        let mut free = post.clone();
        free.requires_payment = false;
        assert_eq!(
            status_of(
                &assess(&free, before_afir),
                ObligationId::PangvArbeitspreisAtThePoint
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn a_sticker_on_the_post_does_not_answer_the_app() {
        // § 14(2)'s last sentence is a **second** duty, not a fourth medium: a
        // point whose price is printed on it still owes the figure through the
        // web system a driver actually pays with, before the session starts.
        let on = date!(2026 - 06 - 01);
        let mut post = compliant_point(date!(2025 - 01 - 01));
        post.price_transparency.arbeitspreis = ArbeitspreisIndication::Printed;
        post.price_transparency.web_based_ad_hoc = true;
        post.price_transparency.arbeitspreis_stated_before_start = false;

        let report = assess(&post, on);
        assert_eq!(
            status_of(&report, ObligationId::PangvArbeitspreisAtThePoint),
            Status::Satisfied,
            "the sticker answers the first sentence"
        );
        assert_eq!(
            status_of(&report, ObligationId::PangvArbeitspreisBeforeStart),
            Status::Failing,
            "…and says nothing about the last one"
        );

        // A post with no web checkout does not owe it.
        post.price_transparency.web_based_ad_hoc = false;
        assert_eq!(
            status_of(
                &assess(&post, on),
                ObligationId::PangvArbeitspreisBeforeStart
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn a_blocking_fee_nobody_displays_is_the_case_the_paragraph_is_about() {
        // § 14(3) has three answers and not two. "No further prices" and "further
        // prices, stated" are both compliant and they are different facts, and
        // an entitlement-shaped `bool` would have collapsed them.
        let on = date!(2026 - 06 - 01);
        let mut post = compliant_point(date!(2025 - 01 - 01));

        post.price_transparency.additional_prices = AdditionalPrices::None;
        assert_eq!(
            status_of(
                &assess(&post, on),
                ObligationId::PangvAdditionalPricesAdjacent
            ),
            Status::NotApplicable,
            "a point charging nothing beyond the Arbeitspreis owes nothing here"
        );

        post.price_transparency.additional_prices = AdditionalPrices::StatedAdjacent;
        assert_eq!(
            status_of(
                &assess(&post, on),
                ObligationId::PangvAdditionalPricesAdjacent
            ),
            Status::Satisfied
        );

        post.price_transparency.additional_prices = AdditionalPrices::NotStated;
        assert_eq!(
            status_of(
                &assess(&post, on),
                ObligationId::PangvAdditionalPricesAdjacent
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
    fn the_verification_period_is_eight_years_and_ends_with_the_calendar_year() {
        // `[MessEV Anl. 7 Nr. 6.7]` gives charging equipment **eight** years
        // where `[MessEV §34(1)]`'s default is two; `[MessEG §37(1) S. 2]`
        // starts the clock at the placing on the market rather than at any
        // Eichung; and `[MessEV §34(2)]` runs it to the end of the year it
        // arithmetically ends in. Each of the three moves the answer, and the
        // third moves it by up to a year.
        let mut point = compliant_point(date!(2019 - 06 - 01));
        point.placed_on_market_on = Some(date!(2018 - 03 - 15));

        assert_eq!(
            point.verification_expires_on(),
            date!(2026 - 12 - 31),
            "eight years from March 2018 is March 2026, and § 34(2) carries it to the year's end"
        );
        assert!(point.verification_valid_on(date!(2026 - 12 - 31)));
        assert!(!point.verification_valid_on(date!(2027 - 01 - 01)));

        // The duty follows the date rather than the point: nothing about the
        // hardware changes between these two assessments.
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 12 - 31)),
                ObligationId::EichrechtVerificationPeriod
            ),
            Status::Satisfied
        );
        assert_eq!(
            status_of(
                &assess(&point, date!(2027 - 01 - 01)),
                ObligationId::EichrechtVerificationPeriod
            ),
            Status::Failing
        );

        // **Calendar years, not 365-day blocks.** Two leap days inside eight
        // years put a day-arithmetic answer on 30 December 2025 where the
        // calendar says 1 January 2026 — and § 34(2) then turns those two days
        // into a whole year.
        let mut new_year = point.clone();
        new_year.placed_on_market_on = Some(date!(2018 - 01 - 01));
        assert_eq!(new_year.verification_expires_on(), date!(2026 - 12 - 31));

        // Commissioning is not the placing on the market, and the fallback is
        // the later of the two — which makes the period end later and is the
        // wrong direction for a permission, so a caller that knows the real
        // date owes it. Stated so the fallback is a decision rather than an
        // accident.
        let mut unknown = point.clone();
        unknown.placed_on_market_on = None;
        assert_eq!(unknown.verification_expires_on(), date!(2027 - 12 - 31));

        // A meter that was actually verified starts its period there
        // `[MessEV §34(1) S. 2]`.
        let mut verified = point.clone();
        verified.metering.last_verified_on = Some(date!(2024 - 05 - 02));
        assert_eq!(verified.verification_expires_on(), date!(2032 - 12 - 31));

        // …and an authority that extended the period for a lot under
        // `[MessEV §35]` is a fact the profile can state rather than a rule
        // this calendar has to invent.
        verified.metering.verification_period_years = Some(12);
        assert_eq!(verified.verification_expires_on(), date!(2036 - 12 - 31));

        // A point that does not bill by energy is outside it, like the rest of
        // the Eichrecht rows.
        let mut by_the_minute = point.clone();
        by_the_minute.metering.bills_by_energy = false;
        assert_eq!(
            status_of(
                &assess(&by_the_minute, date!(2027 - 01 - 01)),
                ObligationId::EichrechtVerificationPeriod
            ),
            Status::NotApplicable
        );
    }

    #[test]
    fn a_firmware_push_is_an_intervention_somebody_has_to_have_recorded() {
        // `[MessEG §31(2) Nr. 4]`'s last clause is the one that reaches a
        // platform: records of interventions "einschließlich solcher durch
        // **elektronisch vorgenommene Maßnahmen**". A CSMS pushing firmware to
        // a charge point is performing one.
        let on = date!(2026 - 06 - 01);
        let mut point = compliant_point(date!(2025 - 01 - 01));
        assert_eq!(
            status_of(
                &assess(&point, on),
                ObligationId::EichrechtInterventionRecords
            ),
            Status::Satisfied
        );

        point.metering.intervention_records_retained = false;
        assert_eq!(
            status_of(
                &assess(&point, on),
                ObligationId::EichrechtInterventionRecords
            ),
            Status::Failing
        );
    }

    #[test]
    fn ac_metering_in_a_dc_station_carries_three_conditions() {
        // `[REA 6-A §3.2]` permits a DC station to meter on the AC side before
        // the rectifier — so the rectification losses sit inside the number the
        // customer pays for. Only on legacy hardware, only where the
        // rectification belongs to one session, and only if they are told.
        let mut point = ChargePointProfile::bare(evse(), date!(2016 - 06 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(22);

        // Metering after the rectifier: none of the three arises.
        let report = assess(&point, date!(2026 - 09 - 01));
        for duty in [
            ObligationId::ReaAcMeteringOnLegacyDcOnly,
            ObligationId::ReaRectificationAttributable,
            ObligationId::ReaRectificationLossDisclosed,
        ] {
            assert_eq!(status_of(&report, duty), Status::NotApplicable, "{duty}");
        }

        point.metering.measurement_point = EnergyMeasurementPoint::AcBeforeRectifier;
        let report = assess(&point, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::ReaAcMeteringOnLegacyDcOnly),
            Status::Satisfied,
            "placed on the market in 2016 at 22 kW"
        );
        assert_eq!(
            status_of(&report, ObligationId::ReaRectificationAttributable),
            Status::Failing,
            "a shared rectifier is the normal way a multi-outlet cabinet is built"
        );
        assert_eq!(
            status_of(&report, ObligationId::ReaRectificationLossDisclosed),
            Status::Failing
        );

        point.metering.rectification_attributable_to_one_session = true;
        point.metering.rectification_loss_disclosed = true;
        let report = assess(&point, date!(2026 - 09 - 01));
        assert!(
            report
                .failing()
                .all(|f| !f.obligation.id.as_str().starts_with("rea-")),
            "{:?}",
            report
                .failing()
                .map(|f| f.obligation.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_fifty_kilowatt_threshold_points_the_other_way_here() {
        // The same number, two inclusive directions. AFIR's fast-charger duties
        // begin **at** 50 kW; `[REA 6-A §3.2]`'s legacy AC-metering allowance
        // still applies **at** 50 kW. A station of exactly 50 kW is on the
        // strict side of one and the permissive side of the other.
        let mut point = ChargePointProfile::bare(evse(), date!(2016 - 06 - 01));
        point.current_type = CurrentType::Dc;
        point.metering.measurement_point = EnergyMeasurementPoint::AcBeforeRectifier;
        point.rated_power_kw = Decimal::from(50);

        assert!(point.is_at_least_50_kw() && point.is_at_most_50_kw());
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::ReaAcMeteringOnLegacyDcOnly
            ),
            Status::Satisfied
        );

        point.rated_power_kw = rust_decimal::Decimal::from_str_exact("50.001").unwrap();
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::ReaAcMeteringOnLegacyDcOnly
            ),
            Status::Failing
        );
    }

    #[test]
    fn placing_on_the_market_is_not_commissioning() {
        // A station placed on the market in 2017 may have been commissioned in
        // 2019, and the allowance turns on the first. Unknown falls back to the
        // second, which is later — so the exemption fails rather than being
        // granted on a date nobody stated.
        let mut point = ChargePointProfile::bare(evse(), date!(2019 - 06 - 01));
        point.current_type = CurrentType::Dc;
        point.rated_power_kw = Decimal::from(22);
        point.metering.measurement_point = EnergyMeasurementPoint::AcBeforeRectifier;

        assert_eq!(point.placed_on_market_date(), date!(2019 - 06 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::ReaAcMeteringOnLegacyDcOnly
            ),
            Status::Failing,
            "unknown falls back to commissioning, and the exemption fails"
        );

        point.placed_on_market_on = Some(date!(2017 - 11 - 01));
        assert_eq!(
            status_of(
                &assess(&point, date!(2026 - 09 - 01)),
                ObligationId::ReaAcMeteringOnLegacyDcOnly
            ),
            Status::Satisfied
        );
    }

    #[test]
    fn a_fully_equipped_point_is_compliant() {
        let point = compliant_point(date!(2027 - 03 - 01));
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
    fn a_provider_duty_is_out_of_scope_for_a_charge_point_and_vice_versa() {
        let point = compliant_point(date!(2027 - 03 - 01));
        let report = assess(&point, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirMspPriceDisclosure),
            Status::DifferentScope
        );

        let provider = ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap());
        let report = assess_provider(&provider, date!(2027 - 06 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirAdHocAccess),
            Status::DifferentScope
        );
    }

    #[test]
    fn the_provider_half_of_article_five_is_a_real_check() {
        // A provider that hides its e-roaming costs inside the kWh price is
        // in breach of Art. 5(5) even though every one of its charge points is
        // faultless.
        let mut provider = ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap());
        let report = assess_provider(&provider, date!(2026 - 09 - 01));
        assert_eq!(
            status_of(&report, ObligationId::AfirMspPriceDisclosure),
            Status::Failing
        );
        assert_eq!(
            status_of(&report, ObligationId::AfirMspNoCrossBorderSurcharge),
            Status::Satisfied,
            "not surcharging is the default"
        );

        provider.discloses_all_price_components = true;
        provider.discloses_electronically = true;
        assert_eq!(
            status_of(
                &assess_provider(&provider, date!(2026 - 09 - 01)),
                ObligationId::AfirMspPriceDisclosure
            ),
            Status::Failing,
            "the e-roaming cost is a component the article names explicitly"
        );

        provider.discloses_e_roaming_costs = true;
        // …and the duty that has nothing to do with Article 5: a provider bills
        // on measured values it did not produce, and `[MessEG §33(2)]` wants a
        // confirmation from whoever operates the meter.
        assert_eq!(
            status_of(
                &assess_provider(&provider, date!(2026 - 09 - 01)),
                ObligationId::EichrechtValueUserConfirmation
            ),
            Status::Failing
        );
        provider.holds_meter_operator_confirmation = true;
        assert_eq!(
            assess_provider(&provider, date!(2026 - 09 - 01)).verdict(),
            Verdict::Compliant
        );

        provider.surcharges_cross_border_roaming = true;
        assert_eq!(
            assess_provider(&provider, date!(2026 - 09 - 01)).verdict(),
            Verdict::Failing,
            "the article forbids a cross-border surcharge outright"
        );
    }

    #[test]
    fn the_2027_wave_is_visible_in_advance() {
        let upcoming: Vec<_> = starting_between(date!(2026 - 09 - 01), date!(2027 - 12 - 31))
            .map(|o| o.id)
            .collect();
        assert!(upcoming.contains(&ObligationId::Da656Iso15118Dash20Public));
        assert!(upcoming.contains(&ObligationId::Da656Iso15118Dash20Private));
        assert!(upcoming.contains(&ObligationId::AfirPaymentInstrumentRetrofit));
        assert!(
            !upcoming.contains(&ObligationId::AfirDatex2),
            "DATEX II already started in April 2026"
        );
    }

    #[test]
    fn in_force_grows_monotonically_over_the_calendar() {
        let early = in_force_on(date!(2024 - 01 - 01)).count();
        let mid = in_force_on(date!(2026 - 06 - 01)).count();
        let late = in_force_on(date!(2027 - 06 - 01)).count();
        assert!(early < mid && mid < late, "{early} {mid} {late}");
        // The last date in the calendar is `[CRA Art. 71(2)]`'s 11.12.2027, so
        // "everything" is a day in 2028 rather than mid-2027.
        assert_eq!(
            in_force_on(date!(2028 - 01 - 01)).count(),
            CALENDAR.len(),
            "everything is in force once the Cyber Resilience Act applies"
        );
    }

    #[test]
    fn every_obligation_is_unique_cited_and_actionable() {
        let mut slugs: Vec<_> = CALENDAR.iter().map(|o| o.id.as_str()).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(before, slugs.len(), "obligation slugs must be unique");

        for o in CALENDAR {
            assert!(
                o.citation.starts_with('[') && o.citation.ends_with(']'),
                "{}: a citation is bracketed, in the form the source table keys on",
                o.id
            );
            assert!(!o.remedy.is_empty(), "{}: needs a remedy", o.id);
            assert!(!o.title.is_empty(), "{}: needs a title", o.id);
            assert!(
                o.applies_until.is_none_or(|until| until >= o.applies_from),
                "{}: the window closes before it opens",
                o.id
            );
        }
    }

    #[test]
    fn every_obligation_is_assessable_by_exactly_one_profile() {
        // The property the `Rule` enum buys: no duty is stubbed out to be
        // unreachable, which is what the old `applicable: |_| false` shape did
        // to Art. 5(5). With three subjects the property is the same one —
        // exactly one of the three answers each duty — and it is the check that
        // catches a fourth subject being added and a duty forgotten.
        let on = date!(2028 - 01 - 01);
        let point = compliant_point(date!(2027 - 03 - 01));
        let provider = ProviderProfile::bare(PartyId::new("DE", "MSP").unwrap());
        let undertaking = UndertakingProfile::bare(PartyId::new("DE", "CPO").unwrap());
        let reports = [
            assess(&point, on),
            assess_provider(&provider, on),
            assess_undertaking(&undertaking, on),
        ];

        for o in CALENDAR {
            let answering = reports
                .iter()
                .filter(|report| status_of(report, o.id) != Status::DifferentScope)
                .count();
            assert_eq!(
                answering, 1,
                "{} must be answerable by exactly one profile",
                o.id
            );
        }
    }

    // ── The third subject ──────────────────────────────────────────────────

    fn operator() -> UndertakingProfile {
        let mut u = UndertakingProfile::bare(PartyId::new("DE", "CPO").unwrap());
        u.operates_recharging_points = true;
        u
    }

    #[test]
    fn the_financial_half_of_the_size_test_is_a_conjunction() {
        // `[NIS2 Art. 2(1)]` reaches an entity that qualifies as medium-sized
        // "or exceeds the ceilings", and the Recommendation defines an SME as
        // fewer than 250 staff **and** (turnover ≤ €50 M **and/or** balance
        // sheet ≤ €43 M). Negated, the financial half becomes an **and** — so
        // an asset-light operator turning over €60 M on a €20 M balance sheet
        // does not exceed the ceilings, and every summary that writes
        // "250 employees or €50 million turnover" puts it in the wrong class.
        let mut light = operator();
        light.employees = 90;
        light.annual_turnover_eur = Decimal::from(60_000_000);
        light.balance_sheet_total_eur = Decimal::from(20_000_000);
        assert!(!light.exceeds_medium_ceilings());
        assert_eq!(light.nis2_class(), Some(Nis2Class::Important));

        // Both above, and it is essential.
        let mut heavy = light.clone();
        heavy.balance_sheet_total_eur = Decimal::from(44_000_000);
        assert!(heavy.exceeds_medium_ceilings());
        assert_eq!(heavy.nis2_class(), Some(Nis2Class::Essential));

        // Headcount alone is enough, on either side of the financial test.
        let mut many = operator();
        many.employees = 250;
        assert_eq!(many.nis2_class(), Some(Nis2Class::Essential));
    }

    #[test]
    fn a_small_operator_is_out_of_scope_and_says_so_rather_than_passing() {
        // Out of scope is `NotApplicable`, not `Satisfied`: a five-person
        // operator that has done nothing is not compliant with NIS2, it is
        // simply not bound — and the two are different answers to an auditor.
        let mut tiny = operator();
        tiny.employees = 5;
        tiny.annual_turnover_eur = Decimal::from(900_000);
        tiny.balance_sheet_total_eur = Decimal::from(400_000);
        assert_eq!(tiny.nis2_class(), None);

        let report = assess_undertaking(&tiny, date!(2026 - 09 - 01));
        assert_eq!(
            report.status_of(ObligationId::Nis2RiskManagement),
            Some(Status::NotApplicable)
        );
        assert_eq!(report.verdict(), Verdict::Compliant);
    }

    #[test]
    fn an_undertaking_that_operates_no_points_is_not_an_annex_one_entity() {
        // A pure mobility service provider is not of an Annex I type: the entry
        // covers operating points "in the name and on behalf of" a provider,
        // which describes the operator. Its Article 5(5) duties are assessed
        // against `ProviderProfile` instead.
        let mut msp = UndertakingProfile::bare(PartyId::new("DE", "MSP").unwrap());
        msp.employees = 4_000;
        assert_eq!(msp.nis2_class(), None);
    }

    #[test]
    fn the_ten_measures_are_a_conjunction_and_the_finding_names_the_gaps() {
        let mut u = operator();
        u.employees = 300;
        u.risk_management = RiskManagement::complete();
        u.risk_management.supply_chain_security = false;
        u.risk_management.cryptography = false;

        assert!(
            !u.risk_management.is_complete(),
            "nine of ten is not the duty"
        );
        assert_eq!(
            u.risk_management.missing(),
            vec!["(d) supply-chain security", "(h) cryptography"]
        );
        assert_eq!(
            assess_undertaking(&u, date!(2026 - 09 - 01))
                .status_of(ObligationId::Nis2RiskManagement),
            Some(Status::Failing)
        );
    }

    #[test]
    fn a_directive_binds_from_the_day_the_member_state_transposed_it() {
        // `[NIS2 Art. 41]` told Member States to apply the rules from
        // 18.10.2024. A directive binds nobody directly, and Germany's
        // transposition came into force on 06.12.2025 — so reporting a breach
        // in between would name fourteen months in which no German authority
        // could act.
        let mut u = operator();
        u.employees = 300;

        assert_eq!(
            assess_undertaking(&u, date!(2025 - 06 - 01))
                .status_of(ObligationId::Nis2RiskManagement),
            Some(Status::NotYetInForce)
        );
        assert_eq!(
            assess_undertaking(&u, date!(2025 - 12 - 06))
                .status_of(ObligationId::Nis2RiskManagement),
            Some(Status::Failing)
        );
    }

    #[test]
    fn a_window_a_statute_grants_is_part_of_the_duty_rather_than_a_grace_period() {
        // The same correction as the transposition date, one level in. The duty
        // attaches when the law applies; the *breach* does not. The German
        // transposition gives the registration three months — "spätestens drei
        // Monate, nachdem sie erstmals … als eine der vorgenannten Einrichtungen
        // gelten" — so an operator already in scope on 06.12.2025 had until
        // 06.03.2026, and reporting it in breach in January named a breach the
        // statute itself said was not one (D259).
        let mut u = operator();
        u.employees = 300;
        u.registered_with_the_authority = false;

        assert_eq!(
            assess_undertaking(&u, date!(2026 - 01 - 15)).status_of(ObligationId::Nis2Registration),
            Some(Status::NotYetInForce),
            "inside the window the statute grants"
        );
        assert_eq!(
            assess_undertaking(&u, date!(2026 - 03 - 06)).status_of(ObligationId::Nis2Registration),
            Some(Status::Failing),
            "and a breach on the day it closes"
        );

        // The duties beside it carry no such window and bind from the day the
        // law applies, which is what makes the distinction worth stating rather
        // than applying as a blanket offset.
        assert_eq!(
            assess_undertaking(&u, date!(2026 - 01 - 15))
                .status_of(ObligationId::Nis2RiskManagement),
            Some(Status::Failing)
        );

        // A registered operator is not failing it on any date.
        u.registered_with_the_authority = true;
        assert_eq!(
            assess_undertaking(&u, date!(2026 - 03 - 06)).status_of(ObligationId::Nis2Registration),
            Some(Status::Satisfied)
        );
    }

    #[test]
    fn the_regulation_beside_it_needs_no_transposition_and_has_two_dates() {
        // `[CRA Art. 71(2)]` applies directly: the reporting duty of Article 14
        // from 11.09.2026, and everything else from 11.12.2027.
        let mut manufacturer = operator();
        manufacturer.employees = 300;
        manufacturer.places_digital_products_on_the_market = true;

        let before = assess_undertaking(&manufacturer, date!(2026 - 09 - 10));
        assert_eq!(
            before.status_of(ObligationId::CraVulnerabilityReporting),
            Some(Status::NotYetInForce)
        );
        let after = assess_undertaking(&manufacturer, date!(2026 - 09 - 11));
        assert_eq!(
            after.status_of(ObligationId::CraVulnerabilityReporting),
            Some(Status::Failing)
        );
        assert_eq!(
            after.status_of(ObligationId::CraEssentialRequirements),
            Some(Status::NotYetInForce),
            "the rest of the Regulation waits fifteen months"
        );
    }

    #[test]
    fn an_operator_that_ships_nothing_owes_the_manufacturer_nothing() {
        // The one genuinely per-deployment question in this calendar. An
        // operator running somebody else's hardware on somebody else's platform
        // is not a manufacturer, and reporting it in breach of the CRA would be
        // a duty applied to the wrong party.
        let mut u = operator();
        u.employees = 300;
        assert_eq!(
            assess_undertaking(&u, date!(2028 - 01 - 01))
                .status_of(ObligationId::CraEssentialRequirements),
            Some(Status::NotApplicable)
        );
    }

    #[test]
    fn an_operator_faultless_at_every_point_can_still_be_in_breach_as_a_company() {
        // The whole argument for a third subject, as a test: the charge points
        // pass, the provider half passes, and the undertaking does not.
        let point = compliant_point(date!(2026 - 01 - 01));
        assert_eq!(
            assess(&point, date!(2026 - 09 - 01)).verdict(),
            Verdict::Compliant
        );

        let mut u = operator();
        u.employees = 300;
        assert_eq!(
            assess_undertaking(&u, date!(2026 - 09 - 01)).verdict(),
            Verdict::Failing
        );

        // …and it becomes compliant only when every one of the five is met.
        u.registered_with_the_authority = true;
        u.risk_management = RiskManagement::complete();
        u.can_warn_within_24_hours = true;
        u.management_approved_measures = true;
        u.management_trained = true;
        let report = assess_undertaking(&u, date!(2026 - 09 - 01));
        assert_eq!(report.verdict(), Verdict::Compliant);
        assert_eq!(report.scope, Scope::Undertaking);
        assert_eq!(report.subject, "DE*CPO");
    }
}
