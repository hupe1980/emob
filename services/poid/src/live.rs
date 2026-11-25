//! Publish only complete CSMS registry reads, with separate static and dynamic acknowledgements.

use crate::{Ledger, Poid, http, push};
use anyhow::{Context as _, Result, ensure};
use emob_poi::datex::{Delivery, PointUpdate, PriceUpdate, Publisher, TablePublication};
use emob_poi::site::Facility;
use emob_poi::status::PointStatus;
use emob_service::journal::Journal;
use emob_service::publication::{PublicPoint, RegistryPage};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

/// Operator evidence for the dynamic offer.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeliveryState {
    /// Number of attempts to send the static table to its registered offer.
    #[serde(default)]
    pub static_attempts: u64,
    /// Latest static table attempt, independent of dynamic delivery.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub static_last_attempt: Option<OffsetDateTime>,
    /// Direct acceptance of the most recent static table or hourly refresh.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub static_accepted_at: Option<OffsetDateTime>,
    /// Last accepted complete dynamic snapshot.
    #[serde(with = "time::serde::rfc3339::option")]
    pub snapshot_at: Option<OffsetDateTime>,
    /// Last direct dynamic receiver acknowledgement.
    #[serde(with = "time::serde::rfc3339::option")]
    pub accepted_at: Option<OffsetDateTime>,
    /// Oldest source change for which a dynamic acknowledgement is still owed.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub pending_since: Option<OffsetDateTime>,
    /// Oldest changed source fact in the latest measured delivery.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_source_change: Option<OffsetDateTime>,
    /// Receiver time paired with the last measured source change.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_measured_at: Option<OffsetDateTime>,
    /// Number of attempts to deliver a dynamic batch, including retries.
    #[serde(default)]
    pub attempts: u64,
    /// Start of the most recent dynamic delivery attempt.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_attempt: Option<OffsetDateTime>,
    /// Number of delta messages since the last snapshot.
    pub changes: usize,
    /// Number of deliveries with a new source change already over one minute old.
    pub missed_deadlines: u64,
    /// Latest source, validation or delivery problem. Safe for the operator view.
    pub problem: Option<String>,
}

/// The source facts acknowledged for one EVSE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    version: u32,
    site_version: u32,
    status: Option<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    status_changed_at: Option<OffsetDateTime>,
    rate: Option<String>,
}

/// Recovery of an accepted feed, including late delivery evidence.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Digest of the configured offer pair and publisher. The journal contains no endpoint credentials.
    pub destinations: Option<[u8; 32]>,
    /// Source positions at the receiver.
    pub points: BTreeMap<String, Cursor>,
    /// Delivery evidence at this position.
    pub delivery: DeliveryState,
}

fn state(word: &str) -> Option<PointStatus> {
    Some(match word {
        "Available" => PointStatus::Available,
        "Charging" => PointStatus::Charging,
        "Occupied" | "Preparing" | "Finishing" | "SuspendedEV" | "SuspendedEVSE" => {
            PointStatus::Occupied
        }
        "Reserved" => PointStatus::Reserved,
        "Faulted" => PointStatus::Faulted,
        "Unavailable" => PointStatus::Unavailable,
        _ => return None,
    })
}

async fn registry(
    client: &reqwest::Client,
    origin: &str,
    token: &emob_service::Secret,
) -> Result<Vec<PublicPoint>> {
    let mut url = reqwest::Url::parse(origin)?.join("/v1/csmsd/public-points")?;
    let mut revision = None;
    let mut points = Vec::new();
    loop {
        let mut response = client
            .get(url.clone())
            .bearer_auth(token.expose())
            .send()
            .await?
            .error_for_status()?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= 1_048_576,
                "registry page exceeds 1 MiB"
            );
            body.extend_from_slice(&chunk);
        }
        let page: RegistryPage = serde_json::from_slice(&body)?;
        ensure!(
            revision.is_none_or(|rev| rev == page.revision) && page.points.len() <= 100,
            "registry page changed"
        );
        revision = Some(page.revision);
        let before = points.len();
        points.extend(page.points);
        let Some(next) = page.next else {
            return Ok(points);
        };
        ensure!(
            next == points.len() && next < 100_000 && next > before,
            "invalid registry cursor"
        );
        url.query_pairs_mut()
            .clear()
            .append_pair("offset", &next.to_string())
            .append_pair("revision", &page.revision.to_string());
    }
}

struct SourceData {
    rates: BTreeMap<emob_core::EvseId, emob_poi::Rate>,
    effective: BTreeMap<emob_core::EvseId, OffsetDateTime>,
    positions: BTreeMap<String, Cursor>,
    incomplete: usize,
}

struct Batch {
    snapshot: bool,
    updates: Vec<PointUpdate>,
    before_refresh: Vec<PointUpdate>,
    oldest: Option<OffsetDateTime>,
}

impl SourceData {
    fn read(
        tariffs: &[emob_service::estate::TariffSpec],
        points: &[PublicPoint],
        now: OffsetDateTime,
    ) -> Result<Self> {
        let histories = emob_service::estate::histories(tariffs)?;
        let mut rates = BTreeMap::new();
        let mut positions = BTreeMap::new();
        let mut effective = BTreeMap::new();
        let mut incomplete = 0;
        for p in points {
            let tariff = histories.get(&p.tariff).and_then(|h| h.in_force_at(now));
            let mut rate_id = None;
            if let Some(t) = tariff {
                let id = format!("{}-ADHOC", p.evse);
                let (rate, notes) = emob_poi::rate::publish(t, &id);
                // An unsupported restriction would turn a conditional price into a false unconditional price.
                if notes.iter().all(|n| {
                    matches!(
                        n,
                        emob_poi::rate::RateNote::OccupancyFeeHasNoPriceType { .. }
                    )
                }) {
                    rates.insert(p.evse.clone(), rate);
                    effective.insert(
                        p.evse.clone(),
                        t.valid_from.unwrap_or(p.changed_at).max(p.changed_at),
                    );
                    rate_id = Some(t.fingerprint().to_string());
                }
            }
            let observation = p
                .observation
                .as_ref()
                .and_then(|obs| state(&obs.status).map(|status| (status, obs.at)));
            incomplete += usize::from(
                rate_id.is_none() || observation.is_none() || p.fully_renewable.is_none(),
            );
            positions.insert(
                p.evse.to_string(),
                Cursor {
                    version: p.version,
                    site_version: p.site_version,
                    status: observation.map(|(status, _)| status.as_profile_str().to_owned()),
                    status_changed_at: p.status_changed_at.or(observation.map(|(_, at)| at)),
                    rate: rate_id,
                },
            );
        }
        Ok(Self {
            rates,
            effective,
            positions,
            incomplete,
        })
    }

    fn updates(
        &self,
        points: &[PublicPoint],
        prev: &Checkpoint,
        table_changed: bool,
        now: OffsetDateTime,
    ) -> Result<Batch> {
        let changed = self
            .positions
            .iter()
            .filter(|(id, pos)| prev.points.get(*id) != Some(pos))
            .count();
        let refresh_due = prev
            .delivery
            .snapshot_at
            .is_some_and(|at| now - at >= Duration::hours(6))
            || prev.delivery.changes + usize::from(changed > 0) >= 3500;
        let snapshot = prev.delivery.snapshot_at.is_none()
            || table_changed
            || refresh_due
            || prev.points.keys().any(|k| !self.positions.contains_key(k))
            || self.positions.iter().any(|(k, p)| {
                prev.points.get(k).is_none_or(|old| {
                    old.version != p.version || old.site_version != p.site_version
                })
            });
        let mut updates = Vec::new();
        let mut before_refresh = Vec::new();
        let mut oldest = prev.delivery.pending_since;
        for p in points {
            let key = p.evse.to_string();
            let pos = &self.positions[&key];
            let old = prev.points.get(&key);
            if !snapshot && Some(pos) == old {
                continue;
            }
            let status_changed = old.is_none_or(|o| {
                o.status != pos.status || o.status_changed_at != pos.status_changed_at
            });
            let price_changed = old.is_none_or(|o| o.rate != pos.rate || o.version != pos.version);
            let point = emob_poi::site::ChargingPoint::new(
                Facility::new(&key).at_version(pos.version),
                p.evse.clone(),
                emob_poi::site::Connector::new(p.connector, p.rated_power_kw),
            );
            // The profile requires a point-level status even for a price-only update.
            // Repeat a current observation with its own time; offline and absent are explicitly unknown.
            let status = p
                .observation
                .as_ref()
                .and_then(|obs| state(&obs.status))
                .unwrap_or(PointStatus::Unknown);
            let report = point.report(status)?;
            let price = self
                .rates
                .get(&p.evse)
                .filter(|_| snapshot || price_changed)
                .map(|r| PriceUpdate {
                    rate_id: r.id.clone(),
                    prices: r.prices.clone(),
                    changed_at: self.effective[&p.evse],
                });
            let source_at = pos.status_changed_at.unwrap_or(p.changed_at);
            for change in pos
                .status_changed_at
                .filter(|_| status_changed)
                .into_iter()
                .chain(
                    self.effective
                        .get(&p.evse)
                        .copied()
                        .filter(|_| price_changed),
                )
            {
                oldest = Some(oldest.map_or(change, |at: OffsetDateTime| at.min(change)));
            }
            let update = PointUpdate {
                site: Facility::new(&p.location.id).at_version(p.site_version),
                station: Facility::new(&p.identity).at_version(pos.version),
                point: point.facility,
                report,
                price,
                changed_at: source_at,
            };
            if snapshot && prev.delivery.snapshot_at.is_some() && Some(pos) != old {
                let mut delta = update.clone();
                if !price_changed {
                    delta.price = None;
                }
                before_refresh.push(delta);
            }
            updates.push(update);
        }
        Ok(Batch {
            snapshot,
            updates,
            before_refresh,
            oldest,
        })
    }
}

/// Preserve the receiver's table version across restart and advance it on a structural change.
fn versioned_table(
    publisher: &mut Poid<'_>,
    held: Option<&crate::Accepted>,
    now: OffsetDateTime,
) -> Result<(TablePublication, bool)> {
    if let Some(accepted) = held {
        publisher.table.version = accepted
            .published
            .version_of(&publisher.table.id)
            .context("accepted table identity changed")?
            .parse()?;
        publisher.restore(accepted.clone());
    }
    let current = publisher.snapshot(now)?;
    let changed = held.is_none_or(|old| old.published != *current.published());
    if changed && held.is_some() && !publisher.sites.is_empty() {
        publisher.table.version = publisher
            .table
            .version
            .checked_add(1)
            .context("table version exhausted")?;
        return Ok((publisher.snapshot(now)?, true));
    }
    Ok((current, changed))
}

/// One publisher owns its clients and both accepted positions.
pub struct Worker<J: Journal> {
    /// Signed delivery journal.
    pub ledger: Ledger<J>,
    /// Static offer identity.
    pub publisher: Publisher,
    /// Registered table identifier.
    pub table: Facility,
    /// Estate endpoints and tariff definitions.
    pub estate: http::Estate,
    /// Each receiver uses its own certificate-bound HTTP client.
    pub receiver: reqwest::Client,
    /// Separate client for the CSMS bearer credential.
    pub source: reqwest::Client,
    /// Credential accepted by the CSMS.
    pub token: emob_service::Secret,
    /// Last committed dynamic receiver state.
    pub checkpoint: Checkpoint,
    /// The last static acceptance.
    pub held: Option<crate::Accepted>,
}

impl<J: Journal> Worker<J> {
    /// One bounded read and, when due, delivery to each registered offer.
    ///
    /// # Errors
    /// An incomplete page sequence or failed push leaves the last acceptance in place.
    #[allow(clippy::too_many_lines)]
    pub async fn round(&mut self, view: &http::Shared) -> Result<()> {
        let cfg = self
            .estate
            .mobilithek
            .as_ref()
            .context("no publication offers configured")?;
        let destination: [u8; 32] = Sha256::digest(format!(
            "{}\0{}\0{}\0{}\0{}",
            cfg.url,
            cfg.dynamic_url,
            self.publisher.national_identifier,
            self.publisher.country,
            self.table.id
        ))
        .into();
        if self.checkpoint.destinations.as_ref() != Some(&destination) {
            let reset = Checkpoint {
                destinations: Some(destination),
                ..Checkpoint::default()
            };
            self.ledger.checkpoint(&reset)?;
            self.checkpoint = reset;
            self.held = None;
        }
        let mut points = registry(&self.source, &self.estate.desk.csmsd, &self.token)
            .await
            .context("read CSMS registry")?;
        let now = OffsetDateTime::now_utc();
        let max_age = Duration::seconds(i64::from(cfg.status_max_age_secs));
        for point in &mut points {
            if let Some(obs) = &point.observation
                && (obs.at > now || now - obs.at >= max_age)
            {
                point.status_changed_at = Some(if obs.at > now { now } else { obs.at + max_age });
                point.observation = None;
            }
        }
        let sites = http::registry_sites(&self.estate.parties, &points)?;
        let empty = sites.is_empty();
        if empty && self.held.is_none() {
            let problem = "No public points to publish".to_owned();
            if self.checkpoint.delivery.problem.as_ref() != Some(&problem) {
                self.checkpoint.delivery.problem = Some(problem);
                self.ledger
                    .progress(&self.checkpoint, &self.checkpoint.delivery, None)?;
            }
            let mut current = http::View::of(&[], None);
            current.dynamic.clone_from(&self.checkpoint.delivery);
            *view
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = current;
            return Ok(());
        }
        let source = SourceData::read(&self.estate.tariffs, &points, now)?;
        let rate_for =
            |point: &emob_poi::site::ChargingPoint| source.rates.get(&point.evse_id).cloned();
        let mut publisher = Poid::new(self.publisher.clone(), self.table.clone(), sites, &rate_for);
        let (publication, changed) = versioned_table(&mut publisher, self.held.as_ref(), now)?;
        let table_changed = !empty && changed;
        let Batch {
            snapshot,
            updates,
            before_refresh,
            oldest,
        } = source.updates(&points, &self.checkpoint, table_changed, now)?;
        if table_changed && self.checkpoint.delivery.pending_since != oldest {
            self.checkpoint.delivery.pending_since = oldest;
            self.ledger
                .progress(&self.checkpoint, &self.checkpoint.delivery, None)?;
        }
        let mut refresh_issue = None;
        if !empty
            && (table_changed
                || self
                    .held
                    .as_ref()
                    .is_none_or(|old| now - old.at >= Duration::hours(1)))
        {
            self.checkpoint.delivery.static_attempts =
                self.checkpoint.delivery.static_attempts.saturating_add(1);
            self.checkpoint.delivery.static_last_attempt = Some(OffsetDateTime::now_utc());
            self.ledger
                .progress(&self.checkpoint, &self.checkpoint.delivery, None)?;
            match push::push(&self.receiver, &cfg.url, &publication).await {
                Ok(()) => {
                    self.ledger.accepted(
                        &emob_service::Actor::Timer,
                        &mut publisher,
                        &publication,
                        OffsetDateTime::now_utc(),
                    )?;
                    self.held = publisher.last_accepted().cloned();
                    self.checkpoint.delivery.static_accepted_at =
                        self.held.as_ref().map(|held| held.at);
                }
                Err(error) if !table_changed => {
                    refresh_issue = Some(format!("Static refresh: {error}"));
                }
                Err(error) => return Err(error).context("Static offer"),
            }
        }
        let prev = &self.checkpoint;
        let mut delivery = prev.delivery.clone();
        let incomplete = source.incomplete;
        delivery.problem = (incomplete > 0).then(|| {
            format!(
                "{incomplete} public points lack a supported price, a timestamped connector state or a renewable-electricity declaration"
            )
        });
        if empty {
            delivery.problem = Some(
                "No public points remain. Withdraw the registered static offer at the Mobilithek."
                    .to_owned(),
            );
        }
        if let Some(issue) = refresh_issue {
            delivery.problem = Some(
                delivery
                    .problem
                    .map_or_else(|| issue.clone(), |other| format!("{issue}. {other}")),
            );
        }
        if snapshot || !updates.is_empty() {
            delivery.attempts = delivery.attempts.saturating_add(1);
            delivery.last_attempt = Some(OffsetDateTime::now_utc());
            self.checkpoint.delivery.attempts = delivery.attempts;
            self.checkpoint.delivery.last_attempt = delivery.last_attempt;
            self.checkpoint.delivery.pending_since = oldest;
            self.ledger
                .progress(&self.checkpoint, &self.checkpoint.delivery, None)?;
            let kind = if snapshot {
                Delivery::Snapshot
            } else {
                Delivery::Delta
            };
            if !before_refresh.is_empty() {
                let delta = publisher
                    .status(Delivery::Delta, &before_refresh, now)?
                    .to_json()?;
                push::post(&self.receiver, &cfg.dynamic_url, delta)
                    .await
                    .context("Dynamic offer")?;
            }
            let doc = publisher.status(kind, &updates, now)?.to_json()?;
            push::post(&self.receiver, &cfg.dynamic_url, doc)
                .await
                .context("Dynamic offer")?;
            let at = OffsetDateTime::now_utc();
            delivery.accepted_at = Some(at);
            delivery.pending_since = None;
            if let Some(source) = oldest {
                delivery.last_source_change = Some(source);
                delivery.last_measured_at = Some(at);
            }
            if snapshot {
                delivery.snapshot_at = Some(at);
                delivery.changes = 0;
            } else {
                delivery.changes += 1;
            }
            if oldest.is_some_and(|source| at - source > Duration::minutes(1)) {
                delivery.missed_deadlines += 1;
            }
            let checkpoint = Checkpoint {
                destinations: self.checkpoint.destinations,
                points: source.positions,
                delivery: delivery.clone(),
            };
            self.ledger
                .progress(&self.checkpoint, &delivery, Some(&checkpoint.points))?;
            self.checkpoint = checkpoint;
        } else if delivery.problem != prev.delivery.problem {
            self.ledger.progress(&self.checkpoint, &delivery, None)?;
            self.checkpoint.delivery = delivery.clone();
        }
        let mut current = http::View::of(&publisher.sites, self.held.clone());
        current.dynamic = delivery;
        *view
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = current;
        Ok(())
    }
}
