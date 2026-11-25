//! Spec 004: Heute, read from the real estate.
//!
//! `empd`, `billd`, `roamd`, `csmsd` and `deskd` start in-process on loopback
//! from the demo estate (`deploy/estate.example.toml`), as in spec 001's
//! `the_estate_runs.rs`. One station charges three times — a good card, a
//! tampered record, a blocked card — and the page is read the way a browser
//! reads it: HTTP Basic with the operator token as its password.
//!
//! The property: the rate's numerator **is** the count of records `billd`
//! received (US2.2, SR-058), because both are the ledger that bills.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use emob_core::Energy;
use emob_service::estate::{self, Env};
use emob_service::{Identity, Shutdown};
use emob_sim::{Fault, Rng, SessionPlan, VirtualStation};
use ocpp_kit::testkit::MockStation;
use ocpp_kit::types::DateTime;
use ocpp_kit::v1_6;
use rust_decimal::Decimal;

const DEMO: &str = include_str!("../../../deploy/estate.example.toml");
const GOOD_CARD: &str = "04A1B2C3D4E5F6";
const BLOCKED_CARD: &str = "04DEADBEEF0001";
const OPERATOR: &str = "operator-token";
const READER: &str = "reader-token";
const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";

fn env() -> Env {
    let values: BTreeMap<&str, &str> = [
        ("EMOB_WEBHOOK_SECRET", SECRET),
        ("EMOB_SERVICE_TOKEN", "service-token"),
        ("EMOB_OPERATOR_TOKEN", OPERATOR),
        ("EMOB_DESK_READ_TOKEN", READER),
        ("EMPD_TOKEN_KEY", "a key nobody outside empd holds"),
        (
            "EMPD_DATA_KEY",
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        ),
        (
            "ROAMD_DATA_KEY",
            "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
        ),
        (
            "CSMSD_DATA_KEY",
            "AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM=",
        ),
        (
            "BILLD_DATA_KEY",
            "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ=",
        ),
        ("EMOB_PARTNER_DE_MSP_TOKEN_A", "token-a-for-de-msp"),
        (
            "EMPD_SIGNING_KEY",
            "ERERERERERERERERERERERERERERERERERERERERERE=",
        ),
        (
            "ROAMD_SIGNING_KEY",
            "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=",
        ),
        (
            "CSMSD_SIGNING_KEY",
            "MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM=",
        ),
        (
            "BILLD_SIGNING_KEY",
            "REREREREREREREREREREREREREREREREREREREREREQ=",
        ),
    ]
    .into_iter()
    .collect();
    Arc::new(move |name| values.get(name).map(|value| (*value).to_owned()))
}

fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

struct Estate {
    dir: PathBuf,
    empd: SocketAddr,
    billd: SocketAddr,
    roamd: SocketAddr,
    csmsd: SocketAddr,
    text: String,
}

impl Estate {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "emob-heute-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let (empd, billd, roamd, csmsd) = (free_port(), free_port(), free_port(), free_port());
        let text = DEMO
            .replace("http://127.0.0.1:9584", &format!("http://{billd}"))
            .replace("http://127.0.0.1:9585", &format!("http://{empd}"))
            .replace("http://127.0.0.1:9583", &format!("http://{roamd}"))
            .replace("http://127.0.0.1:9001", &format!("http://{csmsd}"))
            .replace("retry_secs = 5", "retry_secs = 1")
            // `MockStation` speaks profile 1 over `ws://`, as in spec 001's run.
            .replace("security_profile = 2", "security_profile = 1")
            .replace(r#"tls_cert = "../target/dev-tls/server.pem""#, "")
            .replace(r#"tls_key = "../target/dev-tls/server.key""#, "")
            .replace(r#"client_ca = "../target/dev-tls/ca.pem""#, "");
        Self {
            dir,
            empd,
            billd,
            roamd,
            csmsd,
            text,
        }
    }

    fn sections<T: serde::de::DeserializeOwned>(&self) -> T {
        estate::from_str(&self.text).expect("the demo estate is one every daemon runs on")
    }
}

fn plan() -> SessionPlan {
    let started_at = time::macros::datetime!(2026-11-03 10:00 +1);
    SessionPlan {
        started_at,
        ended_at: started_at + time::Duration::minutes(30),
        energy: Energy::from_kwh(Decimal::from_str_exact("11.250").unwrap()).unwrap(),
        clock_aligned: false,
        connected_before_charging: time::Duration::seconds(20),
    }
}

fn at(instant: time::OffsetDateTime) -> DateTime {
    DateTime::parse(
        &instant
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    )
    .unwrap()
}

fn sampled(record: &str, context: v1_6::ReadingContext) -> v1_6::SampledValue {
    let document = serde_json::json!({
        "signedMeterData": record,
        "signingMethod": "",
        "encodingMethod": "OCMF",
    });
    v1_6::SampledValue::new(document.to_string())
        .with_format(v1_6::ValueFormat::SignedData)
        .with_context(context)
        .with_measurand(v1_6::Measurand::EnergyActiveImportRegister)
}

/// Authorise, start, the signed begin, stop with the signed end.
async fn charge(station: &MockStation, card: &str, faults: &[Fault]) {
    let plan = plan();
    let session = VirtualStation::new(1, &mut Rng::stream(2026, "estate")).charge(&plan, faults);
    let handle = station.handle();
    handle
        .call(v1_6::AuthorizeRequest::new(card))
        .await
        .expect("Authorize");
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            card.to_owned(),
            0,
            at(plan.started_at),
        ))
        .await
        .expect("a start");
    if started.id_tag_info.status != v1_6::AuthorizationStatus::Accepted {
        return;
    }
    let (first, rest) = session.records.split_first().unwrap();
    let (last, middle) = rest.split_last().unwrap();
    let mut values = vec![v1_6::MeterValue::new(
        at(plan.started_at),
        vec![sampled(first, v1_6::ReadingContext::TransactionBegin)],
    )];
    values.extend(middle.iter().map(|record| {
        v1_6::MeterValue::new(
            at(plan.started_at),
            vec![sampled(record, v1_6::ReadingContext::SamplePeriodic)],
        )
    }));
    handle
        .call(v1_6::MeterValuesRequest::new(1, values).with_transaction_id(started.transaction_id))
        .await
        .expect("MeterValues");
    handle
        .call(
            v1_6::StopTransactionRequest::new(0, at(plan.ended_at), started.transaction_id)
                .with_reason(v1_6::Reason::Local)
                .with_transaction_data(vec![v1_6::MeterValue::new(
                    at(plan.ended_at),
                    vec![sampled(last, v1_6::ReadingContext::TransactionEnd)],
                )]),
        )
        .await
        .expect("a stop");
}

fn basic(password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("operator:{password}"))
    )
}

async fn get(url: &str, authorization: Option<&str>) -> (u16, String) {
    let mut request = reqwest::Client::new().get(url);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

/// Exercise the browser's form encoding and the real replacement route. An older
/// or tampered form must not silently turn off a public station.
async fn investigate(page: &str, refusal: &serde_json::Value, heute: &str) {
    let position = refusal["position"].as_u64().unwrap();
    let path = format!("sessions/{position}?lang=en");
    assert!(heute.contains(&format!("href=\"/sessions/{position}?lang=de\"")));
    let url = format!("{page}{path}");
    assert_eq!(get(&url, None).await.0, 401);
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("authorization", basic(OPERATOR))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let body = response.text().await.unwrap();
    assert!(
        body.contains("<h1>Session evidence</h1>")
            && body.contains("Billing decision")
            && body.contains("<time datetime=")
    );
    assert!(body.contains("href=\"/station/CP-1?lang=en\""));
    assert!(!body.contains(GOOD_CARD) && !body.contains(BLOCKED_CARD));
    assert!(body.len() < 150_000);
    let search = format!("{page}lookup?lang=en");
    let input = format!(
        "kind=transaction&value={}",
        refusal["transaction"].as_str().unwrap()
    );
    let post = |site: &str| {
        client
            .post(&search)
            .header("authorization", basic(OPERATOR))
            .header("sec-fetch-site", site)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(input.clone())
    };
    assert_eq!(post("cross-site").send().await.unwrap().status(), 403);
    let response = post("same-origin").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains(&format!("href=\"/{path}\""))
    );
}

async fn desk_post(page: &str, path: &str, token: &str, site: &str, body: &str) -> (u16, String) {
    let response = reqwest::Client::new()
        .post(format!("{page}{path}?lang=en"))
        .header("authorization", basic(token))
        .header("sec-fetch-site", site)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.to_owned())
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    (response.status().as_u16(), response.text().await.unwrap())
}

fn hidden<'a>(html: &'a str, field: &str) -> &'a str {
    let prefix = format!("type=\"hidden\" name=\"{field}\" value=\"");
    html.split_once(&prefix)
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0
}

/// heute-acts/SR-030, heute-acts/SR-032, heute-acts/SR-034: a reader can inspect both owners' evidence. Only a writer
/// can review and confirm on the same origin; the result names that writer.
async fn control_card(page: &str) -> String {
    let site = "same-origin";
    let (code, results) =
        desk_post(page, "lookup", READER, site, "kind=visual&value=MOB-1001").await;
    assert_eq!(code, 200);
    assert!(results.contains("Open card EM-"));
    let reference = hidden(&results, "value");
    let lookup = format!("kind=reference&value={reference}");
    let (_, card) = desk_post(page, "lookup", READER, site, &lookup).await;
    assert!(card.contains("read-only access") && card.contains("No operator changes recorded"));
    assert!(!card.contains("/cards/preview") && !card.contains(GOOD_CARD));
    let (_, card) = desk_post(page, "lookup", OPERATOR, site, &lookup).await;
    assert!(card.contains("Review block"));

    let preview = format!("reference={reference}&blocked=true&reason=lost");
    for (who, origin) in [(READER, site), (OPERATOR, "cross-site")] {
        assert_eq!(
            desk_post(page, "cards/preview", who, origin, &preview)
                .await
                .0,
            403
        );
    }
    let (code, review) = desk_post(page, "cards/preview", OPERATOR, site, &preview).await;
    assert_eq!(code, 200, "{review}");
    assert!(
        review.contains("Confirm block")
            && review.contains("offline station")
            && review.contains("Reported lost")
    );
    let operation = hidden(&review, "operation");
    let confirm = format!("operation={operation}&reference={reference}");
    for (who, origin) in [(READER, site), (OPERATOR, "cross-site")] {
        assert_eq!(
            desk_post(page, "cards/confirm", who, origin, &confirm)
                .await
                .0,
            403
        );
    }
    let (code, applied) = desk_post(page, "cards/confirm", OPERATOR, site, &confirm).await;
    assert_eq!(code, 200, "{applied}");
    let history = applied.split_once("id=\"card-history\"").unwrap().1;
    assert!(applied.contains("Change confirmed") && applied.contains("Review unblock"));
    assert!(
        history.contains(operation)
            && history.contains("operator:demo operator")
            && history.contains("Card blocked")
    );
    assert!(!applied.contains(GOOD_CARD) && !applied.contains(OPERATOR));
    let (code, stale) = desk_post(page, "cards/confirm", OPERATOR, site, &confirm).await;
    assert_eq!(code, 409);
    assert!(stale.contains(operation) && stale.contains("Check the current history"));
    let preview = format!("reference={reference}&blocked=false&reason=recovered");
    let (code, review) = desk_post(page, "cards/preview", OPERATOR, site, &preview).await;
    assert_eq!(code, 200);
    assert!(review.contains("Confirm unblock") && review.contains("contract and credit checks"));
    let confirm = format!(
        "operation={}&reference={reference}",
        hidden(&review, "operation")
    );
    let (code, applied) = desk_post(page, "cards/confirm", OPERATOR, site, &confirm).await;
    assert_eq!(code, 200);
    assert!(applied.contains("Change confirmed") && applied.contains("Review block"));
    let (_, card) = desk_post(page, "lookup", READER, site, &lookup).await;
    let history = card.split_once("id=\"card-history\"").unwrap().1;
    assert!(history.find("Card recovered").unwrap() < history.find("Reported lost").unwrap());
    confirm
}

/// native-driver-enrolment/SR-303 and native-driver-enrolment/SR-305.
/// The signed-in writer reviews the owner's rates, confirms once, and a reader
/// can reconcile the accepted agreement with the original point and fee version.
async fn enrol_card(page: &str) -> String {
    let url = format!("{page}contracts?lang=en");
    let (code, catalogue) = get(&url, Some(&basic(OPERATOR))).await;
    assert_eq!(code, 200, "{catalogue}");
    assert!(
        catalogue.contains("/contracts/preview") && catalogue.contains("DE*ABC*E00001|standard")
    );
    let (code, reader) = get(&url, Some(&basic(READER))).await;
    assert_eq!(code, 200);
    assert!(reader.contains("/contracts/lookup") && !reader.contains("/contracts/preview"));

    let site = "same-origin";
    let intake = "selection=DE*ABC*E00001%7Cstandard&stock=STOCK-1004";
    for (who, origin) in [(READER, site), (OPERATOR, "cross-site")] {
        assert_eq!(
            desk_post(page, "contracts/preview", who, origin, intake)
                .await
                .0,
            403
        );
    }
    let (code, review) = desk_post(page, "contracts/preview", OPERATOR, site, intake).await;
    assert_eq!(code, 200, "{review}");
    for term in [
        "Review driver contract",
        "Confirm enrolment",
        "MSP Standard",
        "DE*ABC*E00001",
        "0.60 EUR/kWh",
        "Monthly subscription",
        "Provider fee version",
    ] {
        assert!(review.contains(term), "review missing {term}");
    }
    let id = hidden(&review, "operation");
    let confirm = format!("operation={id}");
    let lookup = format!("agreement={id}");
    assert_eq!(
        desk_post(page, "contracts/lookup", READER, site, &lookup)
            .await
            .0,
        404
    );
    for (who, origin) in [(READER, site), (OPERATOR, "cross-site")] {
        assert_eq!(
            desk_post(page, "contracts/confirm", who, origin, &confirm)
                .await
                .0,
            403
        );
    }
    let (code, issued) = desk_post(page, "contracts/confirm", OPERATOR, site, &confirm).await;
    assert_eq!(code, 200, "{issued}");
    assert!(
        issued.contains("Agreement issued")
            && issued.contains("operator:demo operator")
            && issued.contains(id)
    );
    assert_eq!(
        desk_post(page, "contracts/confirm", OPERATOR, site, &confirm)
            .await
            .1,
        issued
    );
    assert_eq!(
        desk_post(page, "contracts/lookup", READER, "cross-site", &lookup)
            .await
            .0,
        403
    );
    assert_eq!(
        desk_post(page, "contracts/lookup", READER, site, &lookup)
            .await
            .1,
        issued
    );
    for html in [&review, &issued] {
        for private in ["STOCK-1004", "04AB0000000004", OPERATOR, READER] {
            assert!(!html.contains(private));
        }
        assert!(!html.contains("<script") && html.len() < 150_000);
    }
    confirm
}

async fn edit_publication_settings(page: &str, csmsd: SocketAddr) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let fields = "identity=CP-1&ocpp=1.6&security_profile=1&evse=DE%2AABC%2AE00001&rated_power_kw=150&tariff=ad-hoc&connector=iec62196_t2_combo&location_id=LOC-1&location_name=Depot+Mitte&street=Invalidenstrasse&house_number=1&postal_code=10115&city=Berlin&country=DE&latitude=52.5318&longitude=13.3849&time_zone=Europe%2FBerlin&reason=Verify+electricity+declaration";
    let url = format!("{page}station/CP-1/edit");
    let send = |choices| {
        client
            .post(&url)
            .header("authorization", basic(OPERATOR))
            .header("sec-fetch-site", "same-origin")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!("{fields}&{choices}"))
            .send()
    };
    let missing_choice = send("show_price=false&fully_renewable=false")
        .await
        .unwrap();
    assert_eq!(missing_choice.status().as_u16(), 422);
    let updated = send("publicly_accessible=true&show_price=false&fully_renewable=false")
        .await
        .unwrap();
    let status = updated.status().as_u16();
    assert_eq!(status, 303, "{}", updated.text().await.unwrap());
    let (_, view) = get(
        &format!("http://{csmsd}/v1/csmsd/stations/CP-1"),
        Some(&format!("Bearer {OPERATOR}")),
    )
    .await;
    let station: deskd::station::StationPage = serde_json::from_str(&view).unwrap();
    assert!(station.publicly_accessible);
    assert_eq!(station.fully_renewable, Some(false));
    assert!(!station.show_price);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rate_heute_shows_is_the_count_billd_received() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let estate = Estate::new();
        let empd = empd::daemon::run(empd::daemon::Options {
            http: estate.empd,
            journal: estate.dir.join("empd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("empd", "test"),
        })
        .await
        .expect("empd starts");
        let billd = billd::daemon::run(billd::daemon::Options {
            http: estate.billd,
            journal: estate.dir.join("billd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("billd", "test"),
        })
        .await
        .expect("billd starts");
        let roamd = roamd::daemon::run(roamd::daemon::Options {
            http: estate.roamd,
            journal: estate.dir.join("roamd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("roamd", "test"),
        })
        .await
        .expect("roamd starts");
        let csmsd = csmsd::daemon::run(csmsd::daemon::Options {
            ocpp: "127.0.0.1:0".parse().unwrap(),
            http: estate.csmsd,
            journal: estate.dir.join("csmsd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("csmsd", "test"),
            clock: emob_service::system_clock(),
        })
        .await
        .expect("csmsd starts");
        // The publisher fixture serves only its bounded evidence route. Heute must
        // use the configured origin and its operator token.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let publisher = listener.local_addr().unwrap();
        let routes = axum::Router::new().route(
            "/v1/poid/dynamic",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers["authorization"], format!("Bearer {READER}"));
                axum::Json(serde_json::json!({
                    "accepted_at": "2026-11-25T09:10:33Z", "snapshot_at": "2026-11-25T09:00:00Z",
                    "static_attempts": 4, "static_last_attempt": "2026-11-25T08:51:00Z", "static_accepted_at": "2026-11-25T08:51:12Z",
                    "attempts": 13, "last_attempt": "2026-11-25T09:10:31Z",
                    "changes": 12, "missed_deadlines": 1, "problem": null,
                }))
            }),
        );
        let serving = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let mut desk_estate: deskd::Estate = estate.sections();
        desk_estate.desk.poid = Some(format!("http://{publisher}"));
        let desk = deskd::run(deskd::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            estate: desk_estate,
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("deskd", "test"),
            clock: deskd::system_clock(),
            witness: estate.dir.join("witness.ndjson"),
        })
        .await
        .expect("deskd starts");
        let page = format!("http://{}/", desk.http);

        // SR-098: deskd witnesses every journalled daemon it reads.
        let rounds = desk.watch.round().await;
        assert_eq!(rounds.len(), 4, "{rounds:?}");
        assert!(
            rounds
                .iter()
                .all(|(_, verdict)| verdict.as_ref().is_ok_and(|v| !v.is_alarm())),
            "{rounds:?}"
        );

        // SR-050: no password, no page.
        let (status, body) = get(&page, None).await;
        assert_eq!(status, 401);
        assert!(body.is_empty());
        let (status, _) = get(&page, Some(&basic("not-the-token"))).await;
        assert_eq!(status, 401);
        // SR-056: nothing on this surface changes anything.
        let posted = reqwest::Client::new()
            .post(&page)
            .header("authorization", basic(OPERATOR))
            .send()
            .await
            .unwrap();
        assert_eq!(posted.status().as_u16(), 405);
        // SR-095: a cursor is a number or the page refuses, echoing nothing.
        for query in ["refused=%22%3E%3Cscript%3E", "silent=-1", "invoices=1e3"] {
            let (status, body) = get(&format!("{page}?{query}"), Some(&basic(OPERATOR))).await;
            assert_eq!(status, 400, "{query}");
            assert!(!body.contains('<') && !body.contains("script"), "{body}");
        }
        let (status, _) = get(&format!("{page}?refused=0"), Some(&basic(OPERATOR))).await;
        assert_eq!(status, 200, "a cursor past the oldest is an empty page");

        let station =
            MockStation::connect(&format!("ws://{}/ocpp", csmsd.ocpp), "CP-1").expect("a station");
        station.boot(ocpp_kit::Version::V1_6).await.expect("a boot");
        charge(&station, GOOD_CARD, &[]).await;
        charge(&station, GOOD_CARD, &[Fault::TamperedValue]).await;
        charge(&station, BLOCKED_CARD, &[]).await;

        // The good session reaches billd through csmsd's outbox.
        let billd_view = format!("http://{}/v1/billd/today", estate.billd);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let received = loop {
            let (_, body) = get(&billd_view, Some(&format!("Bearer {OPERATOR}"))).await;
            let view: serde_json::Value = serde_json::from_str(&body).unwrap();
            let received = usize::try_from(view["received"].as_u64().unwrap()).unwrap();
            if received > 0 {
                break received;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "billd never received the record"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // US2.2 / SR-058 / SC-002, at the read model: today's numerator is
        // billd's count — every session here ran today.
        let (_, body) = get(
            &format!("http://{}/v1/csmsd/today", estate.csmsd),
            Some(&format!("Bearer {OPERATOR}")),
        )
        .await;
        let csmsd_view: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            csmsd_view["success"]["today"]["estate"],
            csmsd_view["success"]["week"]["estate"]
        );
        let estate_rate = &csmsd_view["success"]["today"]["estate"];
        assert_eq!(estate_rate["billed"].as_u64().unwrap(), received as u64);
        assert_eq!(estate_rate["attempted"], 3, "{estate_rate}");
        assert_eq!(estate_rate["failed"]["evidence"], 1);
        assert_eq!(estate_rate["failed"]["authorisation"], 1);

        // …and on the page, as an operator reads it.
        let (status, heute) = get(&page, Some(&basic(OPERATOR))).await;
        assert_eq!(status, 200);
        assert!(
            heute.contains(&format!("{received} von 3 Ladevorgängen abgerechnet")),
            "{heute}"
        );
        assert!(heute.contains("Nachweis <span class=\"num\">1</span>"));
        assert!(heute.contains("Autorisierung <span class=\"num\">1</span>"));
        // The tampered session leads the queue, with its reason and paragraph.
        let first_row = heute
            .find("<li id=\"s-")
            .expect("a refused session row");
        assert!(first_row < heute.find("id=\"duties\"").unwrap());
        investigate(&page, &csmsd_view["refused"]["items"][0], &heute).await;
        assert!(
            heute.contains("2026-11-25T09:10:33Z") && heute.contains("Letzte Empfangsbestätigung")
        );
        let (status, asset) = get(
            &format!("{page}station/CP-1?lang=en"),
            Some(&basic(OPERATOR)),
        )
        .await;
        assert_eq!(status, 200);
        assert!(asset.contains("On-site visibility unverified"));
        let (status, edit) = get(
            &format!("{page}station/CP-1/edit?lang=en"),
            Some(&basic(OPERATOR)),
        )
        .await;
        assert_eq!(status, 200);
        assert!(edit.contains("value=\"true\" selected>Public"));
        assert!(edit.contains("value=\"unknown\" selected>Undeclared"));
        let (status, station_read) = get(&format!("{page}station/CP-1?lang=en"), Some(&basic(READER))).await;
        assert_eq!(status, 200);
        assert!(station_read.contains("read-only access") && !station_read.contains("<form method=\"post\""));
        let (status, index) = get(&format!("{page}stations?lang=en"), Some(&basic(READER))).await;
        assert_eq!(status, 200);
        assert!(index.contains("read-only access") && !index.contains("/stations/new"));
        for path in ["station/CP-1/edit", "stations/new"] {
            assert_eq!(get(&format!("{page}{path}?lang=en"), Some(&basic(READER))).await.0, 403);
        }
        let confirmation = control_card(&page).await;
        let enrolment = enrol_card(&page).await;
        edit_publication_settings(&page, estate.csmsd).await;
        serving.abort();
        assert!(
            heute.contains("2026-11-25T09:10:31Z")
                && heute.contains("Dynamische Übertragungsversuche einschließlich Wiederholungen")
                && heute.contains("Statische Tabelle</dt><dd>Versuche 4")
        );
        assert!(heute.contains("Eine Signatur passt nicht"));
        assert!(heute.contains("[MessEG §33]"));
        // SR-057: no card, no contract on the page.
        for absent in [GOOD_CARD, BLOCKED_CARD, "DE-MSP-C00000001", OPERATOR] {
            assert!(!heute.contains(absent), "{absent} is on the page");
        }

        // SR-054: a billd nobody answers for — a deskd reading it at a port
        // nothing listens on — is named; the rest still renders.
        let nobody = free_port();
        let mut blind: deskd::Estate = estate::from_str(&estate.text.replace(
            &format!("http://{}", estate.billd),
            &format!("http://{nobody}"),
        ))
        .unwrap();
        blind.services.empd = format!("http://{nobody}");
        let blind = deskd::run(deskd::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            estate: blind,
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("deskd", "test"),
            // A page's "today" is the clock it was given, not the host's.
            clock: std::sync::Arc::new(|| time::macros::datetime!(2026-06-01 08:00 UTC)),
            witness: estate.dir.join("witness-blind.ndjson"),
        })
        .await
        .expect("a second deskd starts");
        let (_, heute) = get(&format!("http://{}/", blind.http), Some(&basic(OPERATOR))).await;
        assert!(
            heute.contains("<strong>billd</strong> antwortet nicht"),
            "{heute}"
        );
        assert!(heute.contains("Eine Signatur passt nicht"));
        assert!(!heute.contains("class=\"calm"));
        assert!(
            heute.contains("Montag, 01.06.2026")
                && heute.contains("datetime=\"2026-06-01T08:00:00Z\""),
            "the page's today is its clock's: {heute}"
        );
        let (code, unknown) = desk_post(&format!("http://{}/", blind.http), "cards/confirm", OPERATOR, "same-origin", &confirmation).await;
        assert_eq!(code, 502);
        assert!(unknown.contains("may have taken effect") && unknown.contains("Check card history"));
        assert!(unknown.contains(&format!("<code>{}</code>", hidden(&unknown, "operation"))));
        assert!(unknown.contains("Retry this confirmation"));
        let (code, unknown) = desk_post(&format!("http://{}/", blind.http), "contracts/confirm", OPERATOR, "same-origin", &enrolment).await;
        assert_eq!(code,502);
        assert!(unknown.contains("may already exist") && unknown.contains("Check agreement") && unknown.contains("Retry this confirmation"));
        assert_eq!(hidden(&unknown,"operation"),hidden(&unknown,"agreement"));
        blind.kill().await;
        station.shutdown(Duration::from_secs(1)).await;
        empd.kill().await;
        desk.kill().await;
        billd.kill().await;
        csmsd.kill().await;
        roamd.kill().await;
    })
    .await
    .expect("the story ran in under thirty seconds");
}
