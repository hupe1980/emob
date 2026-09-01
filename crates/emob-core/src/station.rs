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
//!
//! # Two profiles, because the law binds two parties
//!
//! `[AFIR Art. 5]` divides: paragraphs 1, 2, 3, 4, 7, 8 and 10 bind the
//! **operator of a recharging point**, and paragraph 5 binds the **mobility
//! service provider**. Judging a provider duty against a charge point is a
//! category error, so there are two profiles — [`ChargePointProfile`] and
//! [`ProviderProfile`] — and the calendar knows which one each duty reads.
//!
//! Paragraph 11 binds a third party, the **owner** of a point somebody else
//! operates. It is modelled here rather than on a third profile because the
//! fact it turns on — whether the arrangement delivers what the operator needs
//! — is a fact about the point, and because the party that has to act on the
//! finding is the operator holding the contract. See [`Ownership`].

use rust_decimal::Decimal;
use time::Date;

use crate::ids::{EvseId, PartyId};

/// Who may use a charge point.
///
/// AFIR's duties attach almost entirely to *publicly accessible* points
/// `[AFIR Art. 2(48)]`; a depot behind a fence is a different regime, and the
/// 2027 ISO 15118-20 duty is the one place a private point is also bound
/// `[DA-656 Anh. 2.1.3]`.
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
    /// AC — Type 2.
    Ac,
    /// DC — Combo 2.
    Dc,
}

/// The IEC 61851-1 charging mode a point offers.
///
/// Derived rather than stored, because a mode field beside a current-type field
/// is two statements about one thing and they can be made to disagree. It
/// matters for exactly one duty: `[DA-656 Anh. 2.1.3]` asks Mode 2 private
/// points for EN IEC 61851-1 and Mode 3/4 private points for EN ISO 15118-20,
/// so a domestic socket in a garage is not bound by the 2027 duty and the wall
/// box beside it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ChargingMode {
    /// Mode 2 — an ordinary socket with an in-cable control box.
    Mode2,
    /// Mode 3 — AC through a dedicated EVSE.
    Mode3,
    /// Mode 4 — DC.
    Mode4,
}

/// How a driver without a contract can pay.
///
/// These are exactly the three instruments `[AFIR Art. 5(1)]` enumerates, and
/// the distinction between them is load-bearing: the article allows (c) — an
/// internet-connected device such as a QR code — **only** at points below
/// 50 kW. At 50 kW and above, only a card reader or a contactless device
/// satisfies the duty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AdHocPayment {
    /// Nothing: a contract is the only way in.
    None,
    /// `[AFIR Art. 5(1)(c)]` — a device using an internet connection allowing
    /// secure payment, such as one generating a QR code.
    ///
    /// Qualifies **only below 50 kW**. Above it, this is the same as having
    /// nothing, which is the trap this variant exists to make visible.
    QrCode,
    /// `[AFIR Art. 5(1)(a)–(b)]` — a payment card reader, or a contactless
    /// device able to read payment cards.
    ///
    /// Qualifies at any power. A single terminal may serve several points
    /// within one recharging pool, so this is set for every point the terminal
    /// serves, not only the one it is bolted to.
    CardReader,
}

impl AdHocPayment {
    /// Whether this instrument satisfies `[AFIR Art. 5(1)]` at a given power.
    ///
    /// The QR-code option is restricted to points below 50 kW, so the same
    /// equipment is compliant on a 22 kW AC post and non-compliant on the
    /// 150 kW charger beside it.
    #[must_use]
    pub fn satisfies_afir_at(self, rated_power_kw: Decimal) -> bool {
        match self {
            Self::None => false,
            Self::QrCode => rated_power_kw < Decimal::from(50),
            Self::CardReader => true,
        }
    }
}

/// Which vehicle-communication generations a point implements.
///
/// `pwm` alone is the legacy basic signalling of IEC 61851. It is a fact worth
/// carrying, but it is **not** an exemption: `[DA-656 Anh. 2.1.1–2.1.3]` bind
/// points "installed or renovated from" a date, and the exemption for existing
/// low-level-communication points is already in that date. A PWM-only point
/// installed in 2026 is the non-compliant case, not the exempt one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one document a charger either implements or does \
              not, and they are independent: a point can speak DIN and -20 and \
              not -2. An enum would force a total order these do not have"
)]
pub struct V2gCommunication {
    /// Basic signalling only (IEC 61851 PWM).
    pub pwm: bool,
    /// DIN SPEC 70121 — high-level DC communication, EIM only, no TLS.
    ///
    /// Carried because it is most of the European DC fleet commissioned before
    /// roughly 2020, and because it is genuinely **high-level** communication
    /// over PLC rather than a duty cycle — which `pwm` alone cannot say.
    ///
    /// It satisfies none of `[DA-656 Anh. 2.1.1–2.1.3]`: those name EN ISO
    /// 15118 and DIN SPEC 70121 is a different document. A DIN-only point is
    /// therefore in the same position as a PWM-only one for every duty in the
    /// calendar, and in a different position for every question about what the
    /// charger can actually do.
    pub din70121: bool,
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
            din70121: false,
            iso15118_2: false,
            iso15118_20: false,
        }
    }

    /// A DC point that speaks DIN SPEC 70121 and nothing newer.
    ///
    /// The pre-2020 fast charger: high-level communication, external
    /// identification only, and no ISO 15118 duty met.
    #[must_use]
    pub const fn din_only() -> Self {
        Self {
            pwm: true,
            din70121: true,
            iso15118_2: false,
            iso15118_20: false,
        }
    }

    /// A point that speaks both high-level generations.
    #[must_use]
    pub const fn both_generations() -> Self {
        Self {
            pwm: true,
            din70121: false,
            iso15118_2: true,
            iso15118_20: true,
        }
    }

    /// `true` when the point does high-level communication at all.
    ///
    /// DIN SPEC 70121 counts. It is a different question from whether any
    /// `[DA-656]` duty is met — those name EN ISO 15118 and DIN is not it —
    /// and conflating the two would describe most of the pre-2020 DC fleet as
    /// signalling-only, which it is not.
    #[must_use]
    pub const fn is_high_level(self) -> bool {
        self.din70121 || self.iso15118_2 || self.iso15118_20
    }

    /// `true` when the point implements a generation of EN ISO 15118.
    ///
    /// The question every `[DA-656 Anh. 2.1.x]` duty turns on, kept apart from
    /// [`is_high_level`](Self::is_high_level) precisely because DIN SPEC 70121
    /// answers one and not the other.
    #[must_use]
    pub const fn is_iso15118(self) -> bool {
        self.iso15118_2 || self.iso15118_20
    }

    /// The high-level generations this point implements, by their stable
    /// names.
    ///
    /// # The spelling is not ours to choose
    ///
    /// A generation has to be written down — in a CDR, a log line, a database
    /// column, a partner's onboarding form — and the market's vocabulary for
    /// it is actively dangerous. `[DATEX-II-Profil Tab. A.130]` spells the
    /// literal `iso15118` with no generation and *defines* it as -20, and has
    /// no literal for -2 at all; a CPO mapping "we do ISO 15118" onto it
    /// publishes a claim of compliance with a duty from 01.01.2027 that its
    /// points do not meet `[DA-656 Anh. 2.1.2–2.1.3]`, in the official record,
    /// in a document no schema validator will object to.
    ///
    /// So these are the names the `iso15118` crate owns — `din70121`,
    /// `iso15118-2`, `iso15118-20` — unambiguous about the generation on
    /// purpose. This crate takes no dependency on that one: nothing here
    /// decides money with a protocol implementation in its tree, and a
    /// datasheet fact about a charger is not a session. The agreement is a
    /// test rather than a hope, in `tests/the_kits_agree.rs`, which does take
    /// it and asserts these strings are its strings.
    ///
    /// PWM is deliberately absent. It is IEC 61851 duty-cycle signalling, not
    /// high-level communication at all, and giving it a name in this list
    /// would be the same conflation one level down.
    pub fn protocol_names(self) -> impl Iterator<Item = &'static str> {
        [
            (self.din70121, "din70121"),
            (self.iso15118_2, "iso15118-2"),
            (self.iso15118_20, "iso15118-20"),
        ]
        .into_iter()
        .filter_map(|(present, name)| present.then_some(name))
    }
}

/// What is published about a point, and where.
///
/// `[AFIR Art. 20]` splits into three duties that are usually collapsed into
/// one and should not be: the **static** data of Art. 20(2)(a)–(b), the
/// **dynamic** data of Art. 20(2)(c) — which explicitly does not bind a point
/// that charges nothing — and the **API** of Art. 20(3), which is a separate
/// thing an operator has to stand up and register with the national access
/// point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one duty of Article 20, and they have different exemptions"
)]
pub struct DataPublication {
    /// Location, connectors, opening hours, power, contact — `[AFIR Art. 20(2)(a)–(b)]`.
    pub static_data: bool,
    /// Operational status, availability, ad-hoc price, renewable share —
    /// `[AFIR Art. 20(2)(c)]`.
    pub dynamic_data: bool,
    /// A free and unrestricted API, registered with the national access point
    /// — `[AFIR Art. 20(3)]`.
    pub open_api: bool,
    /// The feed speaks the DATEX II Recharging profile the German national
    /// access point requires `[DATEX-II-Profil]`.
    pub datex2: bool,
}

/// Something that happened to a point, and the notice the regulator is owed
/// for it.
///
/// `[LSV26 §4(1)]` names **three** notifiable events, not one, and the
/// calendar used to carry only the first. All three are filed the same way and
/// all three hang off § 5(3), which lets the Bundesnetzagentur forbid the
/// operation of a point whose notice was never made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Notice {
    /// When the event happened.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date"))]
    pub happened_on: Date,
    /// When it was reported to the regulator, if it has been.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub notified_on: Option<Date>,
}

impl Notice {
    /// An event nobody has reported yet.
    #[must_use]
    pub const fn unreported(happened_on: Date) -> Self {
        Self {
            happened_on,
            notified_on: None,
        }
    }

    /// An event reported on a date.
    #[must_use]
    pub const fn reported_on(happened_on: Date, notified_on: Date) -> Self {
        Self {
            happened_on,
            notified_on: Some(notified_on),
        }
    }

    /// Whether the notice was filed inside the window.
    #[must_use]
    pub fn is_timely(&self, window_days: i64) -> bool {
        self.notified_on
            .is_some_and(|on| on <= self.happened_on + time::Duration::days(window_days))
    }

    /// How many days late the notice was, when it was filed at all.
    ///
    /// Negative for a notice filed before the event, which is not an error —
    /// a planned decommissioning may be announced in advance.
    #[must_use]
    pub fn delay_days(&self, window_days: i64) -> Option<i64> {
        self.notified_on
            .map(|on| (on - (self.happened_on + time::Duration::days(window_days))).whole_days())
    }
}

/// A change of operator, which `[LSV26 §4(1) S. 2]` makes **two** notices.
///
/// "Bei einem Betreiberwechsel haben Anzeigen nach Satz 1 durch den bisherigen
/// **und** den neuen Betreiber zu erfolgen." Both sides file, and an incoming
/// operator that files its own and assumes the outgoing one did the same has a
/// point the regulator may forbid the operation of — over a notice it never saw
/// and was not the one who owed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OperatorChange {
    /// When the operator changed.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date"))]
    pub happened_on: Date,
    /// When the outgoing operator filed, if it did.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub notified_by_previous_operator_on: Option<Date>,
    /// When the incoming operator filed, if it did.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub notified_by_new_operator_on: Option<Date>,
}

impl OperatorChange {
    /// Whether **both** notices were filed inside the window.
    #[must_use]
    pub fn both_filed_timely(&self, window_days: i64) -> bool {
        [
            self.notified_by_previous_operator_on,
            self.notified_by_new_operator_on,
        ]
        .into_iter()
        .all(|on| {
            Notice {
                happened_on: self.happened_on,
                notified_on: on,
            }
            .is_timely(window_days)
        })
    }
}

/// The register state of a point under `[LSV26 §4]`.
///
/// The Ladesäulenverordnung 2026 does not ask for a flag, it asks for
/// **notifications within deadlines**, and for evidence on request. A boolean
/// cannot express a late filing, and a late filing is exactly what § 5(3) lets
/// the regulator shut a point down for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Registration {
    /// When commissioning was reported to the regulator, if it has been
    /// `[LSV26 §4(1) Nr. 1]`.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub commissioning_notified_on: Option<Date>,
    /// A decommissioning and its notice `[LSV26 §4(1) Nr. 2]`, when the point
    /// has been taken out of service.
    pub decommissioning: Option<Notice>,
    /// A change of operator and the **two** notices it requires
    /// `[LSV26 §4(1) Nr. 3]`.
    pub operator_change: Option<OperatorChange>,
    /// Whether the operator can produce the documents `[LSV26 §4(2)]` lets the
    /// regulator demand — evidence that the point meets § 3.
    ///
    /// A duty to *be able to prove* is a duty that is failed quietly: nothing
    /// goes wrong until the request arrives, and by then the documents either
    /// exist or they do not. § 5(3) closes a point over this one too.
    pub technical_documentation_available: bool,
}

impl Registration {
    /// The window `[LSV26 §4(1) Nr. 1]` states: two weeks after commissioning.
    pub const NOTIFICATION_DAYS: i64 = 14;

    /// The window the other two notices are judged against.
    ///
    /// `[LSV26 §4(1)]` Nr. 2 and Nr. 3 say **`unverzüglich`** — without undue
    /// delay, `[BGB §121]`'s *ohne schuldhaftes Zögern* — and give no number.
    /// Inventing one would be a rule the text does not support; leaving the
    /// duty untestable would make it a rule nothing enforces.
    ///
    /// So it is the number the legislator itself chose in the **same
    /// paragraph** for the one event it did quantify. That is the reading with
    /// the best support inside the text, and it is a documented choice rather
    /// than a statutory figure — which is why it is a separate constant a
    /// deployment can see and argue with.
    pub const PROMPT_NOTIFICATION_DAYS: i64 = Self::NOTIFICATION_DAYS;

    /// A point whose commissioning was reported on a date.
    #[must_use]
    pub const fn notified_on(date: Date) -> Self {
        Self {
            commissioning_notified_on: Some(date),
            decommissioning: None,
            operator_change: None,
            technical_documentation_available: false,
        }
    }

    /// Whether the commissioning notice was made, and made in time.
    ///
    /// `from` is the date the deadline runs from — normally commissioning, and
    /// the date a point *became publicly accessible* when that is later
    /// `[LSV26 §4(3)]`. Use
    /// [`ChargePointProfile::notifiable_commissioning_date`] rather than
    /// picking one.
    #[must_use]
    pub fn is_timely_for(&self, from: Date) -> bool {
        Notice {
            happened_on: from,
            notified_on: self.commissioning_notified_on,
        }
        .is_timely(Self::NOTIFICATION_DAYS)
    }
}

/// How the ad-hoc price is built and shown.
///
/// `[AFIR Art. 5(4)]` says two different things depending on power, and both
/// are checkable facts about a point rather than opinions about a tariff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PriceTransparency {
    /// The ad-hoc price is based on a price per kWh for the electricity
    /// delivered.
    ///
    /// Mandatory at 50 kW and above: "the ad hoc price charged by the operator
    /// **shall be based on the price per kWh**". A purely per-minute tariff on
    /// a fast charger is unlawful, which is a rule almost nothing checks.
    pub energy_based: bool,
    /// The price per kWh, and any occupancy fee per minute, are shown at the
    /// station before the session starts.
    pub shown_at_station: bool,
    /// Every price component is available before the session starts, in the
    /// order `[AFIR Art. 5(4)]` prescribes — per kWh, per minute, per session,
    /// then anything else.
    pub components_in_prescribed_order: bool,
}

/// Who owns the hardware, and whether the arrangement lets the operator comply.
///
/// `[AFIR Art. 5(11)]` is the paragraph nobody models: "Where the operator of a
/// recharging point is not the owner of that point, the owner shall make
/// available to the operator, in accordance with the arrangements between them,
/// a recharging point with the technical characteristics which enable the
/// operator to comply with the obligations set out in paragraphs 2, 7, 8 and
/// 10."
///
/// It matters because host-owned hardware is the normal case — a hotel, a
/// supermarket or a municipality buys the charger and a CPO operates it — and
/// the operator is then held to duties it cannot meet on equipment it cannot
/// change. The article puts that on the owner, and an operator that has not
/// written it into the contract has a compliance gap it does not control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Ownership {
    /// The operator owns the point, so the duty does not arise.
    #[default]
    OperatorOwned,
    /// A third party owns it and the arrangement delivers the technical
    /// characteristics Art. 5(11) names.
    ThirdPartyEnabling,
    /// A third party owns it and the arrangement does not.
    ThirdPartyWithholding,
}

impl Ownership {
    /// Whether somebody other than the operator owns the point.
    #[must_use]
    pub const fn is_third_party(self) -> bool {
        matches!(self, Self::ThirdPartyEnabling | Self::ThirdPartyWithholding)
    }
}

/// How the operator's prices behave toward providers and end users.
///
/// `[AFIR Art. 5(3)]` is one of exactly **two** paragraphs the Regulation names
/// for regulatory monitoring — "Member States shall … monitor the compliance of
/// operators of recharging points and mobility service providers with
/// paragraphs 3 and 5" `[AFIR Art. 5(6)]`. Paragraph 5 binds the provider and
/// is modelled on [`ProviderProfile`]; paragraph 3 binds the operator, and a
/// calendar carrying one without the other checks half of what the regulator
/// was told to look at.
///
/// The operative limb is the non-discrimination one, because it is the one that
/// is a fact rather than a judgement: "Operators … shall not discriminate,
/// through the prices charged, between end users and mobility service providers
/// or between different mobility service providers. However, the level of
/// prices may be differentiated, but only if the differentiation is
/// proportionate and objectively justified."
///
/// The comparability and transparency limbs of the same paragraph are enforced
/// where they are checkable — the display duties of Art. 5(4), and
/// `emob-tariff`'s shape check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PriceConduct {
    /// Prices differ between end users and mobility service providers, or
    /// between different providers.
    ///
    /// Not discriminating is the compliant default, so this is `false` in a
    /// bare profile — the same shape as
    /// [`ProviderProfile::surcharges_cross_border_roaming`].
    pub differentiates_between_providers: bool,
    /// …and the operator holds a proportionate, objectively justified reason
    /// for it.
    ///
    /// Only consulted when [`Self::differentiates_between_providers`] is set:
    /// the article permits differentiation exactly on this condition.
    pub differentiation_is_justified: bool,
}

impl PriceConduct {
    /// Whether the pricing satisfies `[AFIR Art. 5(3)]`'s non-discrimination
    /// limb.
    #[must_use]
    pub const fn is_non_discriminatory(self) -> bool {
        !self.differentiates_between_providers || self.differentiation_is_justified
    }
}

/// Where in a DC station the energy is measured.
///
/// `[REA 6-A §3.2]` permits two arrangements and they are not equivalent. The
/// ordinary one measures **after** the rectifier, so the value is the energy
/// that reached the vehicle. The other measures the AC side **before** it, so
/// the rectification losses are inside the number the customer is billed for —
/// which the regulation permits only on legacy hardware, and only if the
/// customer is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum EnergyMeasurementPoint {
    /// After the rectifier — the DC energy that reached the vehicle.
    #[default]
    DcAfterRectifier,
    /// Immediately before the rectifier, on the AC side.
    ///
    /// `[REA 6-A §3.2]` allows this only in DC stations "in bis zum 31.
    /// Dezember 2017 in Verkehr gebrachten Gleichstromladestationen mit einer
    /// Nennleistung von **bis zu 50 kW**", and only where the rectification can
    /// be attributed to a single charging session.
    AcBeforeRectifier,
}

/// The Eichrecht posture of a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one Eichrecht fact with its own citation and its own duty"
)]
pub struct MeteringPosture {
    /// The meter carries a conformity assessment under the MID.
    pub mid_conformity_assessed: bool,
    /// The point emits signed measured values (OCMF or equivalent).
    pub signed_values: bool,
    /// Sessions are billed by energy. A point that bills by time or not at all
    /// is a different Eichrecht case.
    pub bills_by_energy: bool,
    /// Where in the energy path the meter sits `[REA 6-A §3.2]`.
    pub measurement_point: EnergyMeasurementPoint,
    /// Whether the rectification a measured value covers belongs to exactly one
    /// charging session.
    ///
    /// The second of `[REA 6-A §3.2]`'s two conditions for AC metering: "die
    /// durchgeführte Gleichrichtung … kann einem einzelnen Ladevorgang
    /// ausschließlich und eindeutig zugeordnet werden." A multi-outlet DC
    /// cabinet sharing one rectifier fails it, and shared rectifiers are the
    /// normal way such cabinets are built — which is what makes this the
    /// condition an operator is most likely to be quietly in breach of.
    pub rectification_attributable_to_one_session: bool,
    /// Whether the customer is told that rectification losses are inside the
    /// measured value.
    ///
    /// "Die von einem Messwert oder einer Rechnung Betroffenen sind in
    /// geeigneter Weise darauf hinzuweisen, dass die … Energie für die
    /// Gleichrichtung … Bestandteil des angegebenen Messwerts ist"
    /// `[REA 6-A §3.2]`. A platform whose central claim is that the customer
    /// can check the value owes them the fact that part of it is loss.
    pub rectification_loss_disclosed: bool,
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
    /// An ordinary socket with an in-cable control box rather than a dedicated
    /// EVSE — IEC 61851-1 Mode 2. Only meaningful for AC.
    pub domestic_socket: bool,
    /// Rated power in kW. `Decimal` rather than a float because the 50 kW
    /// threshold is a legal boundary and `49.999999999999996 >= 50.0` is the
    /// kind of comparison that should never be able to go wrong.
    pub rated_power_kw: Decimal,
    /// When the point was put into service.
    ///
    /// This is what AFIR's "deployed from 13 April 2024" reads. A renovation is
    /// *not* a deployment: only `[DA-656]` and `[AFIR Art. 5(8)]` say
    /// "installed **or renovated**", and conflating the two pulls untouched
    /// 2019 hardware into duties that were written for new equipment.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date"))]
    pub commissioned_on: Date,
    /// When it was last substantially renovated, if ever.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub renovated_on: Option<Date>,
    /// When the station was placed on the market ("in Verkehr gebracht"), if it
    /// is known separately from commissioning.
    ///
    /// `[REA 6-A §3.2]`'s AC-metering allowance turns on it, and it is not the
    /// commissioning date: a station placed on the market in 2017 may have been
    /// commissioned in 2019. Unknown falls back to commissioning, which is the
    /// later of the two and therefore makes the allowance *harder* to claim —
    /// the conservative direction for an exemption.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub placed_on_market_on: Option<Date>,
    /// When an existing point **became** publicly accessible, if that happened
    /// after it was commissioned.
    ///
    /// `[LSV26 §4(3)]` applies the whole notification régime "wenn ein
    /// bestehender Ladepunkt öffentlich zugänglich wird" — so a depot charger
    /// that opens to the public owes a commissioning notice two weeks from
    /// *that* day, not from a day years earlier that nobody had to report.
    /// Reading the deadline off `commissioned_on` alone reports such a point as
    /// hopelessly late the moment it opens.
    #[cfg_attr(feature = "serde", serde(with = "crate::wire::date::option"))]
    pub became_publicly_accessible_on: Option<Date>,
    /// Whether the point sits along the trans-European transport network.
    pub on_ten_t: bool,
    /// Whether the point is on a safe and secure parking area.
    ///
    /// Named alongside TEN-T in the 2027 retrofit duty `[AFIR Art. 5(1)]`, and
    /// easy to miss: a truck parking area away from the corridor is in scope
    /// just as a motorway service station is.
    pub on_safe_secure_parking: bool,
    /// Whether the recharging service is paid for at all.
    ///
    /// `[AFIR Art. 5(1)]` exempts points "that do not require payment for the
    /// recharging service" from the whole payment-instrument régime, and
    /// `[AFIR Art. 20(2)]` exempts them from the dynamic-data duty. A free
    /// workplace or municipal charger owes neither.
    pub requires_payment: bool,
    /// How someone without a contract pays.
    pub ad_hoc_payment: AdHocPayment,
    /// Whether automatic authentication (Plug & Charge, `AutoCharge`) is
    /// offered here.
    pub offers_automatic_authentication: bool,
    /// Whether the right *not* to use automatic authentication is shown
    /// clearly and offered conveniently `[AFIR Art. 5(2)]`.
    pub automatic_authentication_opt_out_offered: bool,
    /// Whether the point is a digitally-connected recharging point
    /// `[AFIR Art. 5(7)]` — able to send and receive information in real time.
    pub digitally_connected: bool,
    /// Whether the point is capable of smart recharging `[AFIR Art. 5(8)]`.
    pub smart_recharging_capable: bool,
    /// Whether a DC point has a fixed (tethered) recharging cable installed
    /// `[AFIR Art. 5(10)]`.
    pub fixed_cable: bool,
    /// Whether the point meets the applicable technical requirements
    /// `[LSV26 §3]` — "insbesondere die Anforderungen an die technische
    /// Sicherheit von Energieanlagen nach § 49 Absatz 1 des
    /// Energiewirtschaftsgesetzes".
    ///
    /// The primary duty of the Ladesäulenverordnung, and the one § 5(1), (2)
    /// and (3) all hang off: the regulator may inspect it, demand a retrofit
    /// for it, and forbid the operation of a point over it. A calendar that
    /// carries the regulator's powers without the duty they are exercised over
    /// names the consequence and omits the cause.
    pub meets_technical_requirements: bool,
    /// How the ad-hoc price is built and shown.
    pub price_transparency: PriceTransparency,
    /// How the operator's prices behave toward providers and end users
    /// `[AFIR Art. 5(3)]`.
    pub price_conduct: PriceConduct,
    /// Who owns the hardware, and whether the arrangement lets the operator
    /// comply `[AFIR Art. 5(11)]`.
    pub ownership: Ownership,
    /// Which vehicle-communication generations it speaks.
    pub v2g: V2gCommunication,
    /// What is published about it, and where.
    pub data: DataPublication,
    /// Its register state under the Ladesäulenverordnung.
    pub registration: Registration,
    /// Its metering posture.
    pub metering: MeteringPosture,
    /// Whether third parties can charge here — a THG-Quote precondition
    /// alongside the register entry `[38k §6]`.
    pub open_to_third_parties: bool,
}

impl ChargePointProfile {
    /// The date the point counts as "installed or renovated" from: its
    /// renovation if it has had one, otherwise its commissioning.
    ///
    /// Used only by the duties whose text actually says "installed or
    /// renovated" — `[DA-656 Anh. 2.1]` and `[AFIR Art. 5(8)]`. The AFIR
    /// payment and price duties say "deployed from", and read
    /// [`Self::commissioned_on`] instead.
    #[must_use]
    pub fn installed_or_renovated_on(&self) -> Date {
        self.renovated_on.unwrap_or(self.commissioned_on)
    }

    /// The date `[REA 6-A §3.2]`'s AC-metering allowance is judged against.
    ///
    /// The stated placing-on-market date, or commissioning when it is unknown.
    /// Commissioning is the later of the two, so the fallback makes the
    /// allowance harder to claim rather than easier — which is the direction an
    /// exemption should fail in.
    #[must_use]
    pub fn placed_on_market_date(&self) -> Date {
        self.placed_on_market_on.unwrap_or(self.commissioned_on)
    }

    /// `true` for a point of **at most** 50 kW.
    ///
    /// Not the negation of [`Self::is_at_least_50_kw`] by accident: both
    /// boundaries are inclusive and they meet at exactly 50 kW, where AFIR's
    /// fast-charger duties begin and `[REA 6-A §3.2]`'s legacy AC-metering
    /// allowance still applies. The same number, two directions, and a 50 kW
    /// station is on the strict side of one and the permissive side of the
    /// other.
    #[must_use]
    pub fn is_at_most_50_kw(&self) -> bool {
        self.rated_power_kw <= Decimal::from(50)
    }

    /// The date the `[LSV26 §4(1) Nr. 1]` deadline runs from.
    ///
    /// Commissioning, or the day the point *became* publicly accessible when
    /// that is later — § 4(3) applies the notification régime afresh to an
    /// existing point that opens to the public, and a rule reading
    /// `commissioned_on` alone reports it as years late on its first day.
    #[must_use]
    pub fn notifiable_commissioning_date(&self) -> Date {
        self.became_publicly_accessible_on
            .filter(|on| *on > self.commissioned_on)
            .unwrap_or(self.commissioned_on)
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

    /// The IEC 61851-1 mode this point offers.
    #[must_use]
    pub fn mode(&self) -> ChargingMode {
        match self.current_type {
            CurrentType::Dc => ChargingMode::Mode4,
            CurrentType::Ac if self.domestic_socket => ChargingMode::Mode2,
            CurrentType::Ac => ChargingMode::Mode3,
        }
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
            domestic_socket: false,
            rated_power_kw: Decimal::from(11),
            commissioned_on,
            renovated_on: None,
            placed_on_market_on: None,
            became_publicly_accessible_on: None,
            on_ten_t: false,
            on_safe_secure_parking: false,
            requires_payment: true,
            ad_hoc_payment: AdHocPayment::None,
            offers_automatic_authentication: false,
            automatic_authentication_opt_out_offered: false,
            digitally_connected: false,
            smart_recharging_capable: false,
            fixed_cable: false,
            meets_technical_requirements: false,
            price_transparency: PriceTransparency::default(),
            // Not discriminating and owning your own hardware are the states
            // in which these duties are met or do not arise, which is what a
            // profile that claims nothing should say.
            price_conduct: PriceConduct::default(),
            ownership: Ownership::default(),
            v2g: V2gCommunication::pwm_only(),
            data: DataPublication::default(),
            registration: Registration::default(),
            metering: MeteringPosture::default(),
            open_to_third_parties: false,
        }
    }
}

/// Everything an obligation may ask about a mobility service provider.
///
/// `[AFIR Art. 5(5)]` is the one paragraph of Article 5 that binds the
/// provider rather than the point, and it is a real, checkable duty rather
/// than a footnote: every price component disclosed before the session,
/// e-roaming costs included, through freely available electronic means — and
/// **no extra charge for cross-border e-roaming** at all.
///
/// Keeping it in the calendar and *assessable* is the difference between a
/// compliance model and a compliance gesture: the same operator normally runs
/// both hats, and the provider half is the half nobody checks.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one regulatory fact"
)]
pub struct ProviderProfile {
    /// Which provider this is.
    pub party: PartyId,
    /// All price information specific to the session is made available before
    /// it starts.
    pub discloses_all_price_components: bool,
    /// …including the e-roaming costs, named separately rather than folded
    /// into the kWh price.
    pub discloses_e_roaming_costs: bool,
    /// The disclosure reaches the driver through freely available, widely
    /// supported electronic means.
    pub discloses_electronically: bool,
    /// Whether the provider adds a surcharge for cross-border e-roaming.
    ///
    /// The article does not permit this to be "reasonable and transparent": it
    /// forbids it outright, so the compliant value is `false`.
    pub surcharges_cross_border_roaming: bool,
}

impl ProviderProfile {
    /// A provider that discloses nothing — the fixture equivalent of
    /// [`ChargePointProfile::bare`].
    #[must_use]
    pub const fn bare(party: PartyId) -> Self {
        Self {
            party,
            discloses_all_price_components: false,
            discloses_e_roaming_costs: false,
            discloses_electronically: false,
            surcharges_cross_border_roaming: false,
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
    fn a_renovation_is_a_second_birth_date_only_where_the_text_says_so() {
        let mut p = ChargePointProfile::bare(evse(), date!(2019 - 05 - 01));
        assert_eq!(p.installed_or_renovated_on(), date!(2019 - 05 - 01));
        p.renovated_on = Some(date!(2026 - 03 - 01));
        assert_eq!(p.installed_or_renovated_on(), date!(2026 - 03 - 01));
        assert_eq!(
            p.commissioned_on,
            date!(2019 - 05 - 01),
            "the deployment date is untouched: AFIR Art. 5(1) says 'deployed from', not 'renovated'"
        );
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
        assert!(
            p.requires_payment,
            "…but it does charge for charging, or half the duties would not bind it"
        );
    }

    #[test]
    fn a_qr_code_qualifies_below_fifty_kilowatts_and_not_above() {
        // The same equipment, compliant on the 22 kW post and non-compliant on
        // the 150 kW charger beside it. [AFIR Art. 5(1)(c)]
        assert!(AdHocPayment::QrCode.satisfies_afir_at(Decimal::from(22)));
        assert!(!AdHocPayment::QrCode.satisfies_afir_at(Decimal::from(150)));
        assert!(!AdHocPayment::QrCode.satisfies_afir_at(Decimal::from(50)));

        // A card reader qualifies everywhere.
        assert!(AdHocPayment::CardReader.satisfies_afir_at(Decimal::from(22)));
        assert!(AdHocPayment::CardReader.satisfies_afir_at(Decimal::from(350)));

        assert!(!AdHocPayment::None.satisfies_afir_at(Decimal::from(22)));
    }

    #[test]
    fn the_mode_follows_from_the_current_and_the_socket() {
        let mut p = ChargePointProfile::bare(evse(), date!(2026 - 01 - 01));
        assert_eq!(p.mode(), ChargingMode::Mode3);
        p.domestic_socket = true;
        assert_eq!(p.mode(), ChargingMode::Mode2);
        p.current_type = CurrentType::Dc;
        assert_eq!(
            p.mode(),
            ChargingMode::Mode4,
            "a DC point is Mode 4 whatever the socket flag says"
        );
    }

    #[test]
    fn the_lsv_notification_deadline_is_two_weeks() {
        let commissioned = date!(2026 - 03 - 01);

        // Never reported at all.
        assert!(!Registration::default().is_timely_for(commissioned));
        // On the day, and on the last day of the window.
        assert!(Registration::notified_on(commissioned).is_timely_for(commissioned));
        assert!(Registration::notified_on(date!(2026 - 03 - 15)).is_timely_for(commissioned));
        // One day late is late — and § 5(3) LSV lets the regulator close the
        // point for it.
        assert!(!Registration::notified_on(date!(2026 - 03 - 16)).is_timely_for(commissioned));
    }

    #[test]
    fn pwm_is_a_fact_rather_than_an_exemption() {
        assert!(!V2gCommunication::pwm_only().is_high_level());
        assert!(V2gCommunication::both_generations().is_high_level());

        // A DIN SPEC 70121 charger talks to the car over PLC. Calling that
        // signalling-only would misdescribe most of the DC fleet built before
        // 2020 — and calling it ISO 15118 would credit it with a duty it does
        // not meet. Both questions, separately.
        let din = V2gCommunication::din_only();
        assert!(din.is_high_level());
        assert!(!din.is_iso15118());
        assert!(V2gCommunication::both_generations().is_iso15118());
    }
}
