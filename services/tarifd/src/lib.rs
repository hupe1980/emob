//! `tarifd` — the service that publishes a tariff version.
//!
//! # What it decides, and what it must not
//!
//! Three audiences are owed a charge point's ad-hoc price, and each is a duty
//! with its own citation: the **driver at the point** before they start
//! `[AFIR Art. 5(4)]`, over OCPP 2.1's *Tariff and Cost* block; the **roaming
//! partner** that will settle against it, over OCPI; and the **national access
//! point** `[AFIR Art. 20(2)(c)]`, over DATEX II.
//!
//! Almost every stack computes that price three times, in three systems, and
//! reconciles none of them against the invoice. This one computes it **none**
//! times: each payload is built by the crate that already owns the crossing —
//! [`emob_ocpp::to_ocpp`], [`emob_roam::ocpi::tariff::to_ocpi`],
//! [`emob_poi::rate::publish`] — from the one [`Tariff`] that rates the CDR. The
//! service decides *when* a version takes effect and *whether it went out*. It
//! has no vocabulary for what a price is.
//!
//! # Publishing is not what makes a version effective
//!
//! The version in force is decided by its own window — [`TariffHistory`] — and a
//! CDR is priced with whichever version covered the instant the session started.
//! Publication is a **duty about** that fact, not the fact itself, and the duty
//! runs *ahead* of it: `[AFIR Art. 5(4)]` requires the price to be "known to end
//! users **before they initiate** a recharging session".
//!
//! So a publication that happens when a version takes effect is already late.
//! The driver standing at the point at that instant was shown the old price and
//! will be billed the new one, which is the display-versus-bill drift this
//! workspace exists to make unrepresentable — arriving through the *schedule*
//! rather than through the arithmetic.
//!
//! [`Tarifd::due`] therefore looks **forward** by a lead time, and
//! [`Tarifd::late`] is the separate, sharper question: which versions are in
//! force right now that some audience never confirmed. The first is work; the
//! second is a breach with a name.
//!
//! # All three audiences, or none
//!
//! [`Tarifd::prepare`] builds every payload before any of them is sent, and
//! fails as a whole. That is the one design decision here worth arguing about,
//! and the argument is the same one the crossings make individually: OCPP 2.1
//! **refuses** a tariff it cannot state without widening the price against the
//! driver — an hourly rate with no exact per-minute spelling, a dimension at two
//! VAT rates. Publishing the other two anyway would leave the national access
//! point and every roaming partner quoting a price the estate's own stations do
//! not charge, which is worse than publishing nothing: a driver comparing on a
//! map is then misled by a document this operator signed off.
//!
//! # No I/O
//!
//! Nothing here opens a socket. [`Tarifd::prepare`] returns the payloads and the
//! daemon sends them; [`Tarifd::confirm`] records a delivery that **succeeded**.
//! An audience that was not confirmed stays unconfirmed, so a failed push shows
//! up in [`Tarifd::late`] rather than being forgotten — which is the whole
//! reason the record is separate from the send.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use std::collections::{BTreeMap, BTreeSet};

use emob_core::{Crossing, Note, PartyId, TariffId};
use emob_tariff::{Tariff, TariffFingerprint, TariffHistory};

/// Who is owed a price, and under which duty.
///
/// Three, because the Regulation names three and each is a different document.
/// A fourth would be a fourth crossing in a domain crate, not a branch here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Audience {
    /// The driver at the point, over OCPP 2.1's *Tariff and Cost* block —
    /// `[AFIR Art. 5(4)]`, the screen the article actually regulates.
    Station,
    /// The national access point, over DATEX II — `[AFIR Art. 20(2)(c)]`.
    NationalAccessPoint,
    /// The roaming partner that will settle against it, over OCPI.
    Partner,
}

impl Audience {
    /// Every audience a version is owed to.
    pub const ALL: [Self; 3] = [Self::Station, Self::NationalAccessPoint, Self::Partner];

    /// The duty this audience is owed under.
    #[must_use]
    pub const fn citation(self) -> &'static str {
        match self {
            Self::Station => "[AFIR Art. 5(4)]",
            Self::NationalAccessPoint => "[AFIR Art. 20(2)(c)]",
            // A settlement obligation between two companies rather than a
            // regulatory one, and the only one of the three with no article.
            Self::Partner => "the contract with the partner",
        }
    }
}

impl core::fmt::Display for Audience {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Station => "the charge points",
            Self::NationalAccessPoint => "the national access point",
            Self::Partner => "the roaming partner",
        })
    }
}

/// Everything one version has to be sent as, built together.
///
/// Held rather than sent: this crate opens no socket, and the daemon that does
/// calls [`Tarifd::confirm`] for each audience that **accepted** it.
#[derive(Debug, Clone)]
pub struct Publication {
    /// Which tariff.
    pub tariff_id: TariffId,
    /// Which version of it, by content — so a redeployment of the same numbers
    /// is the same publication and a silent edit is not.
    pub fingerprint: TariffFingerprint,
    /// The first instant this version is in force. `None` for a version that
    /// has always been.
    pub effective_at: Option<time::OffsetDateTime>,
    /// The structured tariff a 2.1 station displays and selects prices from.
    pub station: ocpp_kit::v2_1::Tariff,
    /// The rate the national access point feed publishes.
    pub national_access_point: emob_poi::Rate,
    /// The tariff a roaming partner re-rates against.
    pub partner: ocpi_kit::v2_3_0::Tariff,
    /// What every crossing could not carry exactly, in one account.
    ///
    /// Three seams, one report: an operator reading why a station cannot show a
    /// tier and why OCPI rounded a bound is reading one document.
    pub notes: Vec<Note>,
}

/// Why a version could not be prepared for publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublishError {
    /// The tariff is not one this service holds.
    #[error("no tariff {tariff_id} is published by this service")]
    UnknownTariff {
        /// Which one was asked for.
        tariff_id: String,
    },

    /// A charge point cannot be given this tariff without widening the price
    /// against the driver.
    ///
    /// Fatal to the **whole** publication, not just to this audience. The other
    /// two would otherwise quote a price the estate's own stations do not
    /// charge, and a driver comparing on a map would be misled by a document
    /// this operator published. See the module documentation.
    #[error(
        "the charge points cannot be given this version ({0}), so no audience is: publishing the \
         other two would quote a price the estate does not charge"
    )]
    Station(#[source] emob_ocpp::SeamError),

    /// A roaming partner cannot be given this tariff.
    #[error("a roaming partner cannot be given this version ({0}), so no audience is")]
    Partner(#[source] emob_roam::RoamError),
}

/// What one audience knows about one version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Confirmed {
    at: time::OffsetDateTime,
}

/// A version that is in force and that somebody was never told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Late {
    /// Which tariff.
    pub tariff_id: TariffId,
    /// Which version, by content.
    pub fingerprint: TariffFingerprint,
    /// When it took effect.
    pub effective_at: Option<time::OffsetDateTime>,
    /// Who was never told.
    pub audience: Audience,
}

impl core::fmt::Display for Late {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "tariff {} version {} took effect{} and {} were never told: {} requires the price to \
             be known before a session starts, so every session since has been shown one price \
             and billed another",
            self.tariff_id,
            self.fingerprint.short(),
            self.effective_at
                .map_or_else(String::new, |at| format!(" at {at}")),
            self.audience,
            self.audience.citation(),
        )
    }
}

/// The service: which tariffs it publishes, and what each audience has been
/// told.
#[derive(Debug, Default)]
pub struct Tarifd {
    histories: BTreeMap<TariffId, TariffHistory>,
    /// Keyed by content, so a version that is redeployed unchanged is already
    /// published and one that was edited under the same id is not.
    told: BTreeMap<(TariffId, TariffFingerprint), BTreeMap<Audience, Confirmed>>,
}

impl Tarifd {
    /// A service publishing nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take on a tariff's history.
    ///
    /// The history is what decides which version governs a session, and it
    /// already refuses what this service must never publish: overlapping
    /// windows, an empty one, and a change that does not land on a
    /// settlement-period boundary `[PTB-A 50.7 §3.1.7.2]`. So there is nothing
    /// left here to validate — which is the point.
    pub fn publish(&mut self, history: TariffHistory) {
        self.histories.insert(history.id().clone(), history);
    }

    /// The versions that take effect within `lead` of `now` and that some
    /// audience has not confirmed.
    ///
    /// **Forward-looking, deliberately.** `[AFIR Art. 5(4)]` requires the price
    /// to be known to the driver *before* they initiate a session, so a
    /// publication that goes out when the version takes effect is already late
    /// for every driver standing at a point at that instant. The lead is the
    /// operator's; being ahead of it is the duty.
    #[must_use]
    pub fn due(&self, now: time::OffsetDateTime, lead: time::Duration) -> Vec<&Tariff> {
        let horizon = now + lead;
        self.histories
            .values()
            .flat_map(TariffHistory::versions)
            .filter(|version| {
                // Already over, so nobody is owed it any more.
                version.valid_until.is_none_or(|until| until > now)
                    // …and in force, or about to be.
                    && version.valid_from.is_none_or(|from| from <= horizon)
            })
            .filter(|version| !self.everyone_knows(version))
            .collect()
    }

    /// The versions that are **in force now** and that somebody was never told
    /// about.
    ///
    /// The sharp question, and a different one from [`Self::due`]: this is not
    /// work outstanding, it is a price the estate is charging that one of the
    /// three audiences has never seen. Every session since it took effect was
    /// shown one price and billed another.
    #[must_use]
    pub fn late(&self, now: time::OffsetDateTime) -> Vec<Late> {
        let mut out = Vec::new();
        for (id, history) in &self.histories {
            let Some(version) = history.in_force_at(now) else {
                continue;
            };
            let fingerprint = version.fingerprint();
            let told = self.told.get(&(id.clone(), fingerprint));
            for audience in Audience::ALL {
                if told.is_some_and(|told| told.contains_key(&audience)) {
                    continue;
                }
                out.push(Late {
                    tariff_id: id.clone(),
                    fingerprint,
                    effective_at: version.valid_from,
                    audience,
                });
            }
        }
        out
    }

    /// Build every payload one version has to go out as.
    ///
    /// `at` is the instant the documents are written for — a tariff with
    /// time-of-day elements describes differently at different hours, and a
    /// station provisioned at 21:58 for a price that changes at 22:00 needs the
    /// caller to say which. It is an argument rather than a clock because a
    /// publication replayed two years later has to produce the same bytes.
    ///
    /// # Errors
    ///
    /// [`PublishError::UnknownTariff`], and [`PublishError::Station`] or
    /// [`PublishError::Partner`] where a crossing refuses — which fails the
    /// **whole** publication. See the module documentation for why.
    pub fn prepare(
        &self,
        tariff: &Tariff,
        party: &PartyId,
        at: time::OffsetDateTime,
    ) -> Result<Publication, PublishError> {
        if !self.histories.contains_key(&tariff.id) {
            return Err(PublishError::UnknownTariff {
                tariff_id: tariff.id.to_string(),
            });
        }

        // Every payload is the domain crate's. Nothing in this service reads a
        // price component, and there is therefore no second computation of a
        // number for the three audiences to disagree about.
        let mut account: Crossing<()> = Crossing::lossless(());

        let station = to_ocpp_payload(tariff, at, &mut account)?;
        let partner = to_partner_payload(tariff, party, at, &mut account)?;
        let (national_access_point, rate_notes) =
            emob_poi::rate::publish(tariff, tariff.id.as_str());
        for note in rate_notes {
            account.note("/nap", note.to_string());
        }

        Ok(Publication {
            tariff_id: tariff.id.clone(),
            fingerprint: tariff.fingerprint(),
            effective_at: tariff.valid_from,
            station,
            national_access_point,
            partner,
            notes: account.notes().to_vec(),
        })
    }

    /// Record that one audience **accepted** a version.
    ///
    /// Called after the send, never before it. A push that failed leaves the
    /// audience unconfirmed, so the version turns up in [`Self::late`] the
    /// moment it takes effect — which is the whole reason recording a delivery
    /// is a separate act from attempting one.
    pub fn confirm(
        &mut self,
        publication: &Publication,
        audience: Audience,
        at: time::OffsetDateTime,
    ) {
        self.told
            .entry((publication.tariff_id.clone(), publication.fingerprint))
            .or_default()
            .insert(audience, Confirmed { at });
    }

    /// Which audiences have confirmed a version, in a stable order.
    #[must_use]
    pub fn confirmed(&self, tariff: &Tariff) -> BTreeSet<Audience> {
        self.told
            .get(&(tariff.id.clone(), tariff.fingerprint()))
            .map(|told| told.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Whether every audience has confirmed this exact version.
    fn everyone_knows(&self, version: &Tariff) -> bool {
        let confirmed = self.confirmed(version);
        Audience::ALL
            .iter()
            .all(|audience| confirmed.contains(audience))
    }

    /// The tariffs this service publishes.
    pub fn tariffs(&self) -> impl Iterator<Item = &TariffId> {
        self.histories.keys()
    }
}

/// The station payload, with its account folded in.
fn to_ocpp_payload(
    tariff: &Tariff,
    at: time::OffsetDateTime,
    account: &mut Crossing<()>,
) -> Result<ocpp_kit::v2_1::Tariff, PublishError> {
    let crossed = emob_ocpp::to_ocpp(tariff, at).map_err(PublishError::Station)?;
    account.absorb_notes("/station", crossed.notes().to_vec());
    Ok(crossed.into_value_discarding_notes())
}

/// The partner payload, with its account folded in.
fn to_partner_payload(
    tariff: &Tariff,
    party: &PartyId,
    at: time::OffsetDateTime,
    account: &mut Crossing<()>,
) -> Result<ocpi_kit::v2_3_0::Tariff, PublishError> {
    let crossed =
        emob_roam::ocpi::tariff::to_ocpi(tariff, party, at).map_err(PublishError::Partner)?;
    account.absorb_notes("/partner", crossed.notes().to_vec());
    Ok(crossed.into_value_discarding_notes())
}
