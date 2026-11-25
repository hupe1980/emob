//! Free public contract-price views under `[AFIR Art. 5(5)]`. Reads use public
//! terms and point assignments only. No visitor or page view is journalled.
use crate::{Csmsd, journal::Event, receipt::Window};
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use emob_core::{EvseId, PartyId};
use emob_service::{
    Actor,
    estate::{PublicPlan, RetailOffer, RetailTerms, Station},
    journal::Journal,
    ui::{self, Lang, Shell, escape},
};
use emob_tariff::Dimension;
use rust_decimal::Decimal;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use time::OffsetDateTime;

pub(crate) type PlansByPoint = BTreeMap<String, BTreeMap<(String, String), Arc<PublicPlan>>>;

fn digest<T: Serialize>(value: &T) -> String {
    // All preimages here are ordered structs and tuples with string field names;
    // serialize outside the journal envelope so a timestamp/actor cannot change the id.
    let bytes = serde_json::to_vec(value).expect("public terms serialize as JSON");
    hex::encode(Sha256::digest(bytes))
}

/// Rebuilt entirely from journalled public terms.
#[derive(Debug, Default)]
pub struct Archive {
    snapshots: BTreeMap<String, RetailTerms>,
    fee_versions: BTreeMap<(String, String, String), String>,
}

impl Archive {
    pub(crate) fn has_terms(&self) -> bool {
        !self.snapshots.is_empty()
    }

    pub(crate) fn insert(&mut self, s: RetailTerms) {
        self.fee_versions.insert(
            (
                s.provider.to_string(),
                s.code.clone(),
                s.fees.version.clone(),
            ),
            digest(&s.fees),
        );
        self.snapshots.entry(s.reference()).or_insert(s);
    }
    fn contains(&self, s: &RetailTerms) -> bool {
        self.snapshots.contains_key(&s.reference())
    }
}

impl<J: Journal> Csmsd<J> {
    /// Prepare a bounded lookup of explicitly eligible points before accepting reads.
    pub(crate) fn with_public_plans(mut self, plans: &[PublicPlan]) -> Self {
        for plan in plans {
            let plan = Arc::new(plan.clone());
            for point in &plan.points {
                self.public_plans
                    .entry(point.to_string())
                    .or_default()
                    .insert(
                        (plan.provider.to_string(), plan.code.clone()),
                        Arc::clone(&plan),
                    );
            }
        }
        self
    }

    pub(crate) fn snapshots_for(&self, station: &Station) -> Vec<RetailTerms> {
        if !station.publicly_accessible {
            return Vec::new();
        }
        let Some(plans) = self.public_plans.get(&station.evse.to_string()) else {
            return vec![];
        };
        let Some(history) = self.tariff(&station.tariff) else {
            return vec![];
        };
        let mut snapshots = Vec::new();
        for plan in plans.values() {
            for fees in &plan.schedules {
                for base in history.versions() {
                    let snapshot = RetailTerms {
                        point: station.evse.clone(),
                        provider: plan.provider.clone(),
                        operator: self.party.clone(),
                        code: plan.code.clone(),
                        name: plan.name.clone(),
                        base: base.clone(),
                        fees: fees.clone(),
                        superseded: plan.effective_until(fees).filter(|_| fees.until.is_none()),
                    };
                    if snapshot.until().is_some_and(|end| end <= snapshot.from())
                        || snapshot.retail_tariff().is_err()
                    {
                        continue;
                    }
                    snapshots.push(snapshot);
                }
            }
        }
        snapshots
    }

    pub(crate) fn activate_public_plans(&self) -> anyhow::Result<()> {
        if self.public_plans.is_empty() {
            return Ok(());
        }
        let mut folded = crate::lock(&self.folded);
        // Validate all configured fee identities against earlier publication before
        // writing anything. A removed point must not permit an old fee version to change.
        for plans in self.public_plans.values() {
            for plan in plans.values() {
                for fees in &plan.schedules {
                    let key = (
                        plan.provider.to_string(),
                        plan.code.clone(),
                        fees.version.clone(),
                    );
                    anyhow::ensure!(
                        folded
                            .state()
                            .offer_archive
                            .fee_versions
                            .get(&key)
                            .is_none_or(|old| *old == digest(fees)),
                        "public plan {} redefines immutable fee version {}",
                        plan.code,
                        fees.version
                    );
                }
            }
        }
        let snapshots: Vec<_> = folded
            .state()
            .registry
            .values()
            .flat_map(|s| self.snapshots_for(s))
            .filter(|s| !folded.state().offer_archive.contains(s))
            .collect();
        for snapshot in snapshots {
            folded.record(
                &Actor::Estate,
                Event::PublicOfferPublished {
                    snapshot: Box::new(snapshot),
                },
            )?;
        }
        Ok(())
    }

    fn offer(
        &self,
        evse: &EvseId,
        party: &PartyId,
        code: &str,
        now: OffsetDateTime,
    ) -> Result<RetailOffer, StatusCode> {
        let point = evse.to_string();
        let plan = self
            .public_plans
            .get(&point)
            .and_then(|p| p.get(&(party.to_string(), code.to_owned())))
            .ok_or(StatusCode::NOT_FOUND)?;
        self.with_state(|state| {
            let station = state
                .evse_registry
                .get(&point)
                .and_then(|id| state.registry.get(id))
                .filter(|s| s.publicly_accessible)
                .ok_or(StatusCode::NOT_FOUND)?;
            let base = self
                .tariff(&station.tariff)
                .and_then(|h| h.in_force_at(now))
                .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            let fees = plan
                .schedules
                .iter()
                .filter(|v| v.from <= now)
                .max_by_key(|v| v.from)
                .filter(|v| v.until.is_none_or(|end| now < end))
                .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            let terms = RetailTerms {
                point: evse.clone(),
                operator: self.party.clone(),
                provider: party.clone(),
                code: plan.code.clone(),
                name: plan.name.clone(),
                base: base.clone(),
                fees: fees.clone(),
                superseded: plan.effective_until(fees).filter(|_| fees.until.is_none()),
            };
            let reference = terms.reference();
            if !state.offer_archive.snapshots.contains_key(&reference) {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            Ok(RetailOffer { reference, terms })
        })
    }

    /// The exact offer eligible for a new charging authorization at this point.
    /// This consults the live registry and published archive under the CSMS state lock.
    pub(crate) fn current_retail_reference(
        &self,
        point: &EvseId,
        provider: &PartyId,
        plan: &str,
        at: OffsetDateTime,
    ) -> Option<String> {
        self.offer(point, provider, plan, at)
            .ok()
            .map(|offer| offer.reference)
    }

    pub(crate) fn plan_links(&self, evse: &EvseId) -> Vec<(String, String)> {
        self.public_plans
            .get(&evse.to_string())
            .into_iter()
            .flat_map(|plans| plans.values())
            .map(|p| {
                (
                    format!("/p/{evse}/providers/{}/{}", p.provider, p.code),
                    format!("{} · {}", p.provider, p.name),
                )
            })
            .collect()
    }
}

pub(crate) fn router<J: Journal>(csmsd: Arc<Csmsd<J>>) -> Router {
    Router::new()
        .route("/p/{evse}/providers/{party}/{plan}", get(current::<J>))
        .route("/p/offers/{reference}", get(archived::<J>))
        .route(
            "/v1/csmsd/retail-offers/{point}/{provider}/{plan}",
            get(terms::<J>),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(Window::new(60)),
            budget,
        ))
        .with_state(csmsd)
}

async fn budget(
    State(window): State<Arc<Window>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if request
        .uri()
        .path_and_query()
        .is_some_and(|target| target.as_str().len() > 2048)
    {
        return response(StatusCode::URI_TOO_LONG.into_response());
    }
    if !window.admits() {
        return response(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    response(next.run(request).await)
}

fn response(resp: Response) -> Response {
    let mut resp = ui::hardened(resp, ui::CSP);
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    resp.headers_mut().insert(
        header::VARY,
        axum::http::HeaderValue::from_static("Accept-Language"),
    );
    resp
}

fn language(headers: &HeaderMap, uri: &axum::http::Uri) -> Lang {
    let query = uri
        .query()
        .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("lang=")));
    Lang::negotiate(
        query,
        headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|h| h.to_str().ok()),
    )
}

async fn current<J: Journal>(
    State(csmsd): State<Arc<Csmsd<J>>>,
    path: Result<Path<(String, String, String)>, axum::extract::rejection::PathRejection>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(Path((point, provider, code))) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (Ok(evse), Ok(party)) = (point.parse::<EvseId>(), provider.parse::<PartyId>()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let lang = language(&headers, &uri);
    let now = csmsd.now();
    match csmsd.offer(&evse, &party, &code, now) {
        Ok(offer) => render(&offer.terms, false, lang, now),
        Err(StatusCode::SERVICE_UNAVAILABLE) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lang.pick(
                "Der Vertragspreis ist derzeit nicht verfügbar.",
                "The contract price is currently unavailable.",
            ),
        )
            .into_response(),
        Err(status) => status.into_response(),
    }
}

async fn terms<J: Journal>(
    State(csmsd): State<Arc<Csmsd<J>>>,
    path: Result<Path<(EvseId, PartyId, String)>, axum::extract::rejection::PathRejection>,
) -> Response {
    let Ok(Path((point, provider, plan))) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match csmsd.offer(&point, &provider, &plan, csmsd.now()) {
        Ok(offer) => axum::Json(offer).into_response(),
        Err(status) => status.into_response(),
    }
}

async fn archived<J: Journal>(
    State(csmsd): State<Arc<Csmsd<J>>>,
    path: Result<Path<String>, axum::extract::rejection::PathRejection>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    let Ok(Path(id)) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(s) = csmsd.with_state(|state| state.offer_archive.snapshots.get(&id).cloned()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    render(&s, true, language(&headers, &uri), s.from())
}

fn render(s: &RetailTerms, historical: bool, lang: Lang, at: OffsetDateTime) -> Response {
    let Ok(retail) = s.retail_tariff() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(base) = emob_tariff::describe_gross(&s.base, at) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let number = |n: Decimal| crate::receipt::number(&n.normalize().to_string(), lang);
    let title = lang.pick("Vertragspreis", "Contract price");
    let id = s.reference();
    let wrap_digest = |digest: &str| {
        (0..digest.len())
            .step_by(8)
            .map(|i| &digest[i..i + 8])
            .collect::<Vec<_>>()
            .join("<wbr>")
    };
    let display_id = wrap_digest(&id);
    let mut body = format!(
        "<h1>{title}</h1><p>{} · {} · {}</p><p>{}: {} · {}: {}</p><ul>",
        s.point,
        s.provider,
        escape(&s.name),
        lang.pick("Betreiber", "Operator"),
        s.operator,
        lang.pick("Produkt", "Plan"),
        escape(&s.code)
    );
    for line in &base.lines {
        let label = match line.dimension {
            Dimension::Energy => lang.pick("Energie", "Energy"),
            Dimension::Flat => lang.pick("Sitzung", "Session"),
            Dimension::Time => lang.pick("Ladezeit", "Charging time"),
            Dimension::ParkingTime => lang.pick("Belegung", "Occupancy"),
        };
        let _ = write!(
            body,
            "<li>{} · {label}: {} EUR/{}</li>",
            s.operator,
            number(line.price),
            line.unit()
        );
    }
    let _ = write!(
        body,
        "<li>{} · {}: {} EUR/kWh</li><li>{} · {}: {} EUR/{}</li><li>{} · E-Roaming: {} EUR/{}</li></ul><p>{}: {} EUR</p>",
        s.provider,
        lang.pick("Anbietergebühr", "Provider fee"),
        number(s.fees.energy),
        s.provider,
        lang.pick("Sitzungsgebühr", "Session fee"),
        number(s.fees.session),
        lang.pick("Sitzung", "session"),
        s.provider,
        number(s.fees.roaming),
        lang.pick("Sitzung", "session"),
        lang.pick("Separater Monatsbeitrag", "Separate monthly subscription"),
        number(s.fees.monthly)
    );
    let _ = write!(
        body,
        "<p>{} · {}% {}</p><p>{} {} · {} {}</p>",
        lang.pick("Bruttopreise", "Gross prices"),
        number(s.fees.vat),
        lang.pick("inkl. MwSt.", "VAT included"),
        lang.pick("Gültig ab", "Valid from"),
        s.from(),
        lang.pick("bis", "until"),
        s.until().map_or_else(
            || lang.pick("auf Weiteres", "further notice").to_owned(),
            |end| end.to_string()
        )
    );
    let _ = write!(
        body,
        "<p>{}: <code>{}</code></p><p>{} {}: <code>{}</code></p>",
        lang.pick("Betreiberpreisstand", "Operator tariff version"),
        wrap_digest(&s.base.fingerprint().to_string()),
        lang.pick("Gebührenstand", "Provider fee version"),
        escape(&s.fees.version),
        wrap_digest(&digest(&s.fees)),
    );
    if s.until().is_none_or(|end| {
        at.checked_add(time::Duration::hours(1))
            .is_some_and(|finish| finish <= end)
    }) {
        if let Some(total) = emob_tariff::worked_total(&retail, Decimal::TEN, Decimal::TEN, at) {
            let _ = write!(
                body,
                "<p>{}: 10 kWh · 10 kW · 60 min · {} EUR</p>",
                lang.pick("Beispiel", "Example"),
                crate::receipt::number(&format!("{:.2}", total.amount()), lang)
            );
        }
    } else {
        let _ = write!(
            body,
            "<p>{}</p>",
            lang.pick(
                "Preiswechsel während der Beispielstunde: Gesamtpreis nicht verfügbar.",
                "A price change occurs during the example hour. A total is unavailable."
            )
        );
    }
    if let Some(end) = s.until() {
        let _ = write!(
            body,
            "<p>{} {end}: {}</p>",
            lang.pick("Preiswechsel", "Price boundary"),
            lang.pick(
                "Für eine übergreifende Sitzung ist kein verbindlicher Gesamtpreis verfügbar.",
                "A price for a session spanning this boundary is unavailable."
            )
        );
    }
    let _ = write!(
        body,
        "<p>{}</p><p><a href=\"/p/offers/{id}\">{}</a> <code>{display_id}</code></p><p class=\"back\"><a href=\"/p/{}\">{}</a></p>",
        if historical {
            lang.pick(
                "Archivierter Preisstand. Die Gültigkeitsdaten beziehen sich auf die Preise. Der Ladepunkt kann früher zurückgezogen oder neu zugeordnet werden. Kein Nachweis einer persönlichen Zustimmung.",
                "Archived terms. The validity dates apply to the prices. The point can be withdrawn or reassigned earlier. This record does not attest to individual acceptance.",
            )
        } else {
            lang.pick(
                "Öffentlich angebotene Vertragskonditionen.",
                "Publicly offered contract terms.",
            )
        },
        lang.pick("Preisstand", "Offer reference"),
        s.point,
        lang.pick("Ad-hoc-Preis", "Ad-hoc price")
    );
    ui::html(ui::page(
        &Shell {
            title,
            place: "emob",
            lang,
            reading: true,
            script: None,
        },
        &body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorize::{Answer, Authorize};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use emob_service::journal::File;
    use std::path::Path as FsPath;
    use time::macros::datetime;
    use tower_service::Service as _;

    struct TestAuthority;
    impl Authorize for TestAuthority {
        fn ask<'a>(
            &'a self,
            uid: &'a str,
            _evse: Option<&'a emob_core::EvseId>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Answer> + Send + 'a>> {
            assert_eq!(uid, "load-test-card", "a price read reached authorisation");
            Box::pin(async move {
                Answer {
                    status: crate::authorize::Status::Accepted,
                    reason: "accepted".into(),
                    token: Some(crate::journal::Presented {
                        issuer: "DE*MSP".parse().unwrap(),
                        uid: uid.into(),
                        token_type: emob_roam::TokenType::Rfid,
                        contract_id: "DE-MSP-C00000001-W".parse().unwrap(),
                    }),
                    retail: None,
                    reference: Some("a".repeat(64)),
                }
            })
        }
    }
    fn start(
        estate: &crate::estate::Estate,
        path: &FsPath,
        at: OffsetDateTime,
    ) -> Arc<Csmsd<File>> {
        Arc::new(
            Csmsd::open(
                estate.parties.operator.clone(),
                emob_eichrecht::KeyRegistry::new(),
                Arc::new(TestAuthority),
                File::open(path).unwrap(),
                emob_core::TimeZone::utc(),
            )
            .unwrap()
            .with_clock(Arc::new(move || at))
            .pricing(crate::estate::tariffs(estate).unwrap())
            .with_public_plans(&estate.public_plans),
        )
    }
    fn fixture() -> crate::estate::Estate {
        emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml")).unwrap()
    }
    async fn read(routes: &mut Router, path: &str) -> (StatusCode, String, HeaderMap) {
        let response = routes
            .call(
                Request::builder()
                    .uri(path)
                    .header(header::ACCEPT_LANGUAGE, "en")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = String::from_utf8(
            to_bytes(response.into_body(), 100_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        (status, body, headers)
    }

    #[tokio::test]
    async fn public_offer_replays_without_recording_visitors_and_rejects_conflicting_versions() {
        // SR-200 SR-202 SR-203 SR-205. Restore after retirement and check the actual routes.
        let mut estate = fixture();
        let path =
            std::env::temp_dir().join(format!("emob-public-offers-replay-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        estate.public_plans[0].name = "Muster <Plan>".into();
        let boundary = datetime!(2026-11-25 12:00 UTC);
        let mut next = estate.public_plans[0].schedules[0].clone();
        next.version = "v2".into();
        next.energy = Decimal::new(21, 2);
        next.from = boundary;
        next.until = None;
        let current = "/p/DE*ABC*E00001/providers/DE*MSP/standard";
        let id;
        {
            let csmsd = start(&estate, &path, boundary - time::Duration::hours(2));
            csmsd.activate_public_plans().unwrap();
            csmsd.seed(&estate.stations).unwrap();
            let position = csmsd.with_journal(|j| j.records().unwrap().len());
            let mut routes = router(Arc::clone(&csmsd));
            let (status, body, headers) = read(&mut routes, current).await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("6.30 EUR") && body.contains("Muster &lt;Plan&gt;"));
            assert!(!body.contains("Muster <Plan>") && !body.contains("DE-MSP-C00000001"));
            assert_eq!(headers[header::CACHE_CONTROL], "no-store");
            id = body
                .split("/p/offers/")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_string();
            assert_eq!(id.len(), 64);
            let (status, absent, _) =
                read(&mut routes, "/p/DE*ABC*E00001/providers/DE*MSP/secret").await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(absent.is_empty());
            assert_eq!(position, csmsd.with_journal(|j| j.records().unwrap().len()));
        }
        // The original fee version remains immutable when a later start ends its offer.
        estate.public_plans[0].schedules.push(next);
        {
            let csmsd = start(&estate, &path, boundary);
            csmsd.activate_public_plans().unwrap();
            let position = csmsd.with_journal(|j| j.records().unwrap().len());
            csmsd.activate_public_plans().unwrap();
            assert_eq!(position, csmsd.with_journal(|j| j.records().unwrap().len()));
            let mut routes = router(Arc::clone(&csmsd));
            assert!(read(&mut routes, current).await.1.contains("7.30 EUR"));
            csmsd
                .retire(&Actor::Estate, "CP-1", "temporary test retirement")
                .unwrap();
            assert_eq!(read(&mut routes, current).await.0, StatusCode::NOT_FOUND);
            let (status, history, _) = read(&mut routes, &format!("/p/offers/{id}")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                history.contains("Archived terms")
                    && history.contains("6.30 EUR")
                    && history.contains("does not attest")
            );
        }
        {
            let csmsd = start(&estate, &path, boundary);
            csmsd.activate_public_plans().unwrap();
            csmsd.seed(&estate.stations).unwrap();
            let mut routes = router(csmsd);
            assert_eq!(read(&mut routes, current).await.0, StatusCode::NOT_FOUND);
            assert_eq!(
                read(&mut routes, &format!("/p/offers/{id}")).await.0,
                StatusCode::OK
            );
        }
        estate.public_plans[0].schedules[0].energy = Decimal::TEN;
        let csmsd = start(&estate, &path, boundary);
        assert!(csmsd.activate_public_plans().is_err());
        drop(csmsd);
        std::fs::remove_file(path).unwrap();
    }

    // native-driver-enrolment/SR-302. The owner only offers the active, complete public terms.
    #[tokio::test]
    async fn current_enrolment_terms_bind_every_fee_and_the_live_point_assignment() {
        let estate = fixture();
        let point = estate.stations[0].evse.clone();
        let provider = estate.public_plans[0].provider.clone();
        let now = datetime!(2026-11-25 12:00 UTC);
        let path =
            std::env::temp_dir().join(format!("emob-enrolment-offer-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let csmsd = start(&estate, &path, now);
        csmsd.seed(&estate.stations).unwrap();
        let mut routes = router(Arc::clone(&csmsd));
        let endpoint = format!("/v1/csmsd/retail-offers/{point}/{provider}/standard");
        let (status, body, headers) = read(&mut routes, &endpoint).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        let offer: RetailOffer = serde_json::from_str(&body).unwrap();
        let reference = &offer.reference;
        assert!(offer.matches(&point, &provider, "standard", now));
        assert_eq!(
            csmsd
                .current_retail_reference(&point, &provider, "standard", now)
                .as_ref(),
            Some(reference)
        );
        let driver = format!("/p/{point}/providers/{provider}/standard");
        assert!(
            read(&mut routes, &driver)
                .await
                .1
                .contains(&format!("/p/offers/{reference}"))
        );
        let mut changed = offer.clone();
        changed.terms.base.elements[0].components[0].price += Decimal::ONE;
        assert!(!changed.matches(&point, &provider, "standard", now));
        for field in ["energy", "session", "roaming", "monthly", "vat"] {
            let mut wire: serde_json::Value = serde_json::from_str(&body).unwrap();
            let amount = &mut wire["terms"]["fees"][field];
            let decimal: Decimal = amount
                .as_str()
                .expect("exact decimal string")
                .parse()
                .unwrap();
            *amount = serde_json::Value::String((decimal + Decimal::ONE).to_string());
            let modified: RetailOffer = serde_json::from_value(wire).unwrap();
            assert!(
                !modified.matches(&point, &provider, "standard", now),
                "{field}"
            );
        }
        assert!(!offer.matches(&point, &"DE*ALT".parse().unwrap(), "standard", now));
        assert!(!offer.matches(&point, &provider, "other-plan", now));
        let mut invalid = offer.clone();
        invalid.terms.fees.monthly = -Decimal::ONE;
        invalid.reference = invalid.terms.reference();
        assert!(!invalid.matches(&point, &provider, "standard", now));
        assert!(!offer.matches(
            &"DE*ABC*E00002".parse().unwrap(),
            &provider,
            "standard",
            now
        ));
        let mut bounded = offer;
        bounded.terms.superseded = Some(now);
        bounded.reference = bounded.terms.reference();
        assert!(!bounded.matches(&point, &provider, "standard", now));
        assert!(bounded.matches(&point, &provider, "standard", now - time::Duration::SECOND));

        let mut station = estate.stations[0].clone();
        station.publicly_accessible = false;
        csmsd
            .update(&Actor::Estate, station, "point withdrawn", false)
            .unwrap();
        assert_eq!(read(&mut routes, &endpoint).await.0, StatusCode::NOT_FOUND);
        assert!(
            csmsd
                .current_retail_reference(&point, &provider, "standard", now)
                .is_none()
        );
        drop((routes, csmsd));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn public_offer_refuses_a_gap_and_keeps_two_providers_with_the_same_plan_separate() {
        // SR-200 SR-206. A gap is an unavailable public offer; unknown and private
        // points must not leak. An explicitly free fee schedule is still a valid offer.
        let mut estate = fixture();
        let mut other = estate.public_plans[0].clone();
        other.provider = "DE*ALT".parse().unwrap();
        other.schedules[0].energy = Decimal::ZERO;
        other.schedules[0].session = Decimal::ZERO;
        other.schedules[0].roaming = Decimal::ZERO;
        other.schedules[0].monthly = Decimal::new(500, 2);
        estate.public_plans.push(other);
        let time = datetime!(2026-11-25 12:00 UTC);
        estate.public_plans[0].schedules[0].until = Some(time);
        let path =
            std::env::temp_dir().join(format!("emob-public-offers-gap-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let csmsd = start(&estate, &path, time);
        csmsd.seed(&estate.stations).unwrap();
        let mut routes = router(Arc::clone(&csmsd));
        assert_eq!(
            read(&mut routes, "/p/DE*ABC*E00001/providers/DE*MSP/standard")
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let (status, body, _) =
            read(&mut routes, "/p/DE*ABC*E00001/providers/DE*ALT/standard").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("4.90 EUR") && body.contains("Separate monthly subscription: 5 EUR"));
        let mut station = estate.stations[0].clone();
        station.publicly_accessible = false;
        csmsd
            .update(&Actor::Estate, station, "point no longer public", false)
            .unwrap();
        assert_eq!(
            read(&mut routes, "/p/DE*ABC*E00001/providers/DE*ALT/standard")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        drop((routes, csmsd));
        std::fs::remove_file(&path).unwrap();
        estate.public_plans.clear();
        let csmsd = start(&estate, &path, time);
        csmsd.seed(&estate.stations).unwrap();
        let mut routes =
            crate::price::router(Arc::clone(&csmsd), crate::price::Operator::default())
                .merge(router(csmsd));
        let (status, ad_hoc, _) = read(&mut routes, "/p/DE*ABC*E00001").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!ad_hoc.contains("/providers/"));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn public_offer_omits_private_targets_in_responses_and_structured_logs() {
        // SR-200. Exercise the real pricing and common HTTP logging layers together.
        let captured = emob_service::telemetry::Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || writer.clone())
            .finish();
        let _capture = tracing::subscriber::set_default(subscriber);
        let estate = fixture();
        let path =
            std::env::temp_dir().join(format!("emob-public-offers-log-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let csmsd = start(&estate, &path, datetime!(2026-11-25 12:00 UTC));
        csmsd.seed(&estate.stations).unwrap();
        let mut routes = emob_service::Server::new(
            emob_service::Identity::new("test-csmsd", "0"),
            emob_service::Readiness::new(),
            emob_service::Shutdown::new(),
        )
        .with(router(csmsd))
        .router();
        let base = "/p/DE*ABC*E00001/providers/DE*MSP/";
        for target in [
            "missing",
            "%FF",
            "private-plan-canary?customer=private-id-canary&wholesale=private-rate-canary",
        ] {
            let (status, body, _) = read(&mut routes, &format!("{base}{target}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(body.is_empty());
        }
        let logs = captured.text();
        assert!(logs.contains("/providers/{party}/{plan}"));
        for needle in [
            "private-plan-canary",
            "private-id-canary",
            "private-rate-canary",
        ] {
            assert!(!logs.contains(needle));
        }
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn public_offer_budget_rejects_oversized_targets_and_spares_other_routes() {
        // SR-204. A full quote budget has no effect on an independent route.
        let estate = fixture();
        let path =
            std::env::temp_dir().join(format!("emob-public-offers-budget-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let csmsd = start(&estate, &path, datetime!(2026-11-25 12:00 UTC));
        let mut routes = router(Arc::clone(&csmsd));
        let before = csmsd.with_journal(|j| j.records().unwrap().len());
        let base = "/p/offers/";
        let target = format!("{base}{}", "a".repeat(2048));
        assert_eq!(read(&mut routes, &target).await.0, StatusCode::URI_TOO_LONG);
        for _ in 0..60 {
            assert_eq!(
                read(&mut routes, "/p/offers/unknown").await.0,
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            read(&mut routes, "/p/offers/unknown").await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(csmsd.with_journal(|j| j.records().unwrap().len()), before);
        let answer = csmsd
            .authorize(
                &Actor::station("CP-1").unwrap(),
                &crate::Identity::new("CP-1").unwrap(),
                None,
                "load-test-card",
            )
            .await
            .unwrap();
        assert!(answer.is_accepted());
        assert_eq!(
            csmsd.with_journal(|j| j.records().unwrap().len()),
            before + 1
        );
        std::fs::remove_file(path).unwrap();
    }
}
