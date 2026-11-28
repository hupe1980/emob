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
const FINANCE: &str = "finance-fixture";
const FINANCE_WRITER: &str = "line-review-fixture";
const PRIVACY: &str = "privacy-fixture";
const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";

fn env() -> Env {
    let values: BTreeMap<&str, &str> = [
        (
            "ACCOUNTINGD_DATA_KEY",
            "BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU=",
        ),
        (
            "ACCOUNTINGD_SIGNING_KEY",
            "VVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVU=",
        ),
        ("EMOB_WEBHOOK_SECRET", SECRET),
        ("EMOB_SERVICE_TOKEN", "service-token"),
        ("EMOB_OPERATOR_TOKEN", OPERATOR),
        ("EMOB_DESK_READ_TOKEN", READER),
        ("EMOB_FINANCE_TEST", FINANCE),
        ("EMOB_LINE_REVIEW_TEST", FINANCE_WRITER),
        ("EMOB_PRIVACY_TEST", PRIVACY),
        (
            "EMOB_PORTAL_TEST",
            "078b2d6f021bb6fc3afeaa69ca8c93ed8cd633ad6851ab8a39e93eb5dd7bd095",
        ),
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
            .replace(r#"client_ca = "../target/dev-tls/ca.pem""#, "")
            + "\n[[operators]]\nname=\"Line reviewer\"\ntoken_env=\"EMOB_LINE_REVIEW_TEST\"\nfinance=true\nwrite=true\n"
            + "\n[[operators]]\nname=\"Cash desk\"\ntoken_env=\"EMOB_FINANCE_TEST\"\nfinance=true\n"
            + "\n[[operators]]\nname=\"Privacy desk\"\ntoken_env=\"EMOB_PRIVACY_TEST\"\nprivacy=true\n"
            + "\n[[portal_users]]\nsubject=\"private-fixture\"\nsecret_env=\"EMOB_PORTAL_TEST\"\n";
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

async fn line_credit_forms_require_a_finance_writer_and_same_origin(page: &str) {
    let path = format!("{page}line-credits?lang=en");
    for (token, status) in [
        (READER, 403),
        (OPERATOR, 403),
        (PRIVACY, 403),
        (FINANCE, 403),
        (FINANCE_WRITER, 200),
    ] {
        let (code, body) = get(&path, Some(&basic(token))).await;
        assert_eq!(code, status);
        assert_eq!(body.contains("name=\"agreement\""), status == 200);
    }
    for action in ["lookup", "review", "confirm"] {
        assert_eq!(
            desk_post(
                page,
                &format!("line-credits/{action}"),
                FINANCE_WRITER,
                "cross-site",
                ""
            )
            .await
            .0,
            403
        );
    }
    let input = format!("number=UNKNOWN-INVOICE&agreement={}", "a".repeat(64));
    let (code, body) = desk_post(
        page,
        "line-credits/lookup",
        FINANCE_WRITER,
        "same-origin",
        &input,
    )
    .await;
    assert_eq!(code, 404);
    assert!(body.contains("No matching invoice line"));
}

/// Privacy-only staff can review a subject and reopen the same owner receipt.
async fn erasure_is_reviewed_by_a_named_privacy_operator(page: &str) {
    let path = "customer-erasure";
    let subject = "subject=private-fixture";
    let home = format!("{page}?lang=en");
    let (code, nav) = get(&home, Some(&basic(PRIVACY))).await;
    assert_eq!(code, 200);
    assert!(nav.contains("href=\"/customer-erasure?lang=en\""));
    assert!(!nav.contains("href=\"/customer-access?lang=en\""));
    // fleet-navigation/SR-032. Every deep workspace uses the current operator grants.
    let support = format!("{page}lookup?lang=en&finance=true");
    for (role, portal_access, receivables, erasure) in [
        (READER, false, false, false),
        (OPERATOR, true, false, false),
        (FINANCE, false, true, false),
        (PRIVACY, false, false, true),
    ] {
        let (status, html) = get(&support, Some(&basic(role))).await;
        assert_eq!(status, 200);
        let header = html.split_once("</header>").unwrap().0;
        assert!(header.contains(r#"href="/lookup?lang=en" aria-current="page""#));
        for (path, allowed) in [
            ("customer-access", portal_access),
            ("receivables", receivables),
            ("customer-erasure", erasure),
        ] {
            assert_eq!(
                header.contains(&format!(r#"href="/{path}?lang=en""#)),
                allowed,
                "{path}"
            );
        }
    }
    for role in [READER, OPERATOR, FINANCE] {
        assert!(
            !get(&home, Some(&basic(role)))
                .await
                .1
                .contains("href=\"/customer-erasure?lang=en\"")
        );
        assert_eq!(
            get(&format!("{page}{path}"), Some(&basic(role))).await.0,
            403
        );
        assert_eq!(
            desk_post(
                page,
                "customer-erasure/status",
                role,
                "same-origin",
                subject
            )
            .await
            .0,
            403
        );
    }
    for action in ["review", "status", "confirm"] {
        let (status, page) = desk_post(
            page,
            &format!("{path}/{action}"),
            PRIVACY,
            "cross-site",
            "subject=PRIVATE-MARKER",
        )
        .await;
        assert_eq!(status, 403);
        assert!(!page.contains("PRIVATE-MARKER"));
    }
    let (code, german) = get(&format!("{page}{path}?lang=de"), Some(&basic(PRIVACY))).await;
    assert_eq!(code, 200);
    assert!(german.contains("Portal-Kennung"));
    let (code, current) = desk_post(
        page,
        "customer-erasure/status",
        PRIVACY,
        "same-origin",
        subject,
    )
    .await;
    assert_eq!(code, 200);
    assert!(current.contains("Active customer"));
    let (code, review) = desk_post(
        page,
        "customer-erasure/review",
        PRIVACY,
        "same-origin",
        subject,
    )
    .await;
    assert_eq!(code, 200);
    assert!(
        review.contains("GDPR Article 17")
            && review.contains("German VAT Act")
            && review.contains("Permanently erase portal access")
    );
    let form = format!("{subject}&operation={}", hidden(&review, "operation"));
    let (code, receipt) = desk_post(
        page,
        "customer-erasure/confirm",
        PRIVACY,
        "same-origin",
        &form,
    )
    .await;
    assert_eq!(code, 200);
    assert!(receipt.contains("Erasure receipt") && receipt.contains("Privacy desk"));
    assert!(!receipt.contains("action=\"/customer-erasure/confirm"));
    assert_eq!(
        desk_post(
            page,
            "customer-erasure/confirm",
            PRIVACY,
            "same-origin",
            &form
        )
        .await
        .1,
        receipt
    );
    assert_eq!(
        desk_post(
            page,
            "customer-erasure/status",
            PRIVACY,
            "same-origin",
            subject
        )
        .await
        .1,
        receipt
    );
}

// receivables-allocation/SR-354. Exercise the browser file picker at the HTTP boundary.
async fn finance_file_import(page: &str) {
    let client = reqwest::Client::new();
    let upload = |token: &'static str, site: &'static str, file: &str| {
        client.post(format!("{page}receivables/upload?lang=en"))
            .basic_auth("desk", Some(token))
            .header("sec-fetch-site", site)
            .header("content-type", "multipart/form-data; boundary=fixture")
            .body(format!("--fixture\r\nContent-Disposition: form-data; name=\"file\"; filename=\"statement.json\"\r\nContent-Type: application/json\r\n\r\n{file}\r\n--fixture--\r\n"))
    };
    let statement = serde_json::json!({"version":1,"source":"BANK-1","account":"PAYER-PRIVATE","currency":"EUR","entries":[
        {"reference":"ENTRY-1","amount_minor":500,"status":"booked","direction":"credit"}]}).to_string();
    for (token, site) in [(OPERATOR, "same-origin"), (FINANCE, "cross-site")] {
        assert_eq!(
            upload(token, site, &statement)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(
        upload(FINANCE, "same-origin", &"x".repeat(65_537))
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    let ambiguous = statement.replace(
        r#""amount_minor":500"#,
        r#""amount_minor":700,"amount_minor":500"#,
    );
    assert_eq!(
        upload(FINANCE, "same-origin", &ambiguous)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let (status, bad) = desk_post(
        page,
        "receivables/confirm",
        FINANCE,
        "same-origin",
        "amount_minor=PRIVATE-FIELD",
    )
    .await;
    assert_eq!(status, 400);
    assert!(!bad.contains("PRIVATE-FIELD"));
    let r = upload(FINANCE, "same-origin", &statement)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert!(r.text().await.unwrap().contains("Newly booked</dt><dd>1"));
    let (status, balance) = desk_post(
        page,
        "receivables/balance",
        FINANCE,
        "same-origin",
        "account=PAYER-PRIVATE&currency=EUR",
    )
    .await;
    assert_eq!(status, 200);
    assert!(balance.contains("ENTRY-1") && balance.contains("5.00 EUR"));
    assert!(!balance.contains("action=\"/receivables/review"));
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
    let intake = "selection=DE*ABC*E00001%7Cstandard&stock=STOCK-1004&account=PAYER-FLEET";
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
    let (status, read_receipt) = desk_post(page, "contracts/lookup", READER, site, &lookup).await;
    assert_eq!(status, 200);
    assert!(read_receipt.contains("Agreement issued") && read_receipt.contains(id));
    assert!(!read_receipt.contains("Manage agreement"));
    assert!(issued.contains("Manage agreement"));
    let change = format!("{lookup}&action=replace&stock=STOCK-1006");
    for (who, origin) in [(READER, site), (OPERATOR, "cross-site")] {
        assert_eq!(
            desk_post(page, "contracts/change/preview", who, origin, &change)
                .await
                .0,
            403
        );
        assert_eq!(
            desk_post(page, "contracts/change/confirm", who, origin, &confirm)
                .await
                .0,
            403
        );
    }
    let (status, pending) =
        desk_post(page, "contracts/change/preview", OPERATOR, site, &change).await;
    assert_eq!(status, 200);
    assert!(pending.contains("Confirm change") && pending.contains("PAYER-FLEET"));
    assert!(!pending.contains("STOCK-1006") && !pending.contains("04AB0000000006"));
    for html in [&review, &issued] {
        for private in ["STOCK-1004", "04AB0000000004", OPERATOR, READER] {
            assert!(!html.contains(private));
        }
        assert!(!html.contains("<script") && html.len() < 150_000);
    }
    confirm
}

/// A writer records a private request and confirms the exact review. A reader, a
/// finance-only account and a cross-origin submission cannot enter the case.
async fn review_private_withdrawal(page: &str) {
    let site = "same-origin";
    let path = "contracts/withdrawal";
    let (status, landing) = get(&format!("{page}{path}?lang=en"), Some(&basic(OPERATOR))).await;
    assert_eq!(status, 200);
    assert!(landing.contains("Find a case"));
    for account in [READER, FINANCE] {
        assert_eq!(
            get(&format!("{page}{path}?lang=en"), Some(&basic(account)))
                .await
                .0,
            403
        );
    }
    for action in [
        "lookup",
        "intake",
        "review",
        "confirm",
        "evidence",
        "credit-review",
        "credit-confirm",
    ] {
        for (caller, origin) in [(OPERATOR, "cross-site"), (READER, site), (FINANCE, site)] {
            let (code, denied) = desk_post(
                page,
                &format!("{path}/{action}"),
                caller,
                origin,
                "private=PRIVATE-MARKER",
            )
            .await;
            assert_eq!(code, 403);
            assert!(!denied.contains("PRIVATE-MARKER"));
        }
    }
    let (code, enrolment) = desk_post(
        page,
        "contracts/preview",
        OPERATOR,
        site,
        "selection=DE*ABC*E00001%7Cstandard&stock=STOCK-1005&account=PAYER-PRIVATE",
    )
    .await;
    assert_eq!(code, 200, "{enrolment}");
    let agreement = hidden(&enrolment, "operation");
    assert_eq!(
        desk_post(
            page,
            "contracts/confirm",
            OPERATOR,
            site,
            &format!("operation={agreement}")
        )
        .await
        .0,
        200
    );
    let lookup = format!("agreement={agreement}");
    let (code, fresh) = desk_post(page, &format!("{path}/lookup"), OPERATOR, site, &lookup).await;
    assert_eq!(code, 200, "{fresh}");
    assert!(fresh.contains("Record request"));
    let received = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let request = format!("{lookup}&request_ref=MAIL-DESK-1&channel=email&received_at={received}");
    let (code, saved) = desk_post(page, &format!("{path}/intake"), OPERATOR, site, &request).await;
    assert_eq!(code, 200, "{saved}");
    assert!(saved.contains("MAIL-DESK-1") && saved.contains("Review decision"));
    let draft = format!("{lookup}&outcome=approve&evidence_ref=FILE-904&billing=review_required");
    let (code, review) = desk_post(page, &format!("{path}/review"), OPERATOR, site, &draft).await;
    assert_eq!(code, 200, "{review}");
    assert!(
        review.contains("refuse the next online charging authorization")
            && review.contains("separate billing confirmation")
    );
    let confirm = format!("{lookup}&operation={}", hidden(&review, "operation"));
    let (code, approved) =
        desk_post(page, &format!("{path}/confirm"), OPERATOR, site, &confirm).await;
    assert_eq!(code, 200, "{approved}");
    assert!(approved.contains("Withdrawal approved") && approved.contains("FILE-904"));
    assert!(!approved.contains("action=\"/contracts/withdrawal/review"));
    assert_eq!(
        desk_post(page, &format!("{path}/confirm"), OPERATOR, site, &confirm)
            .await
            .1,
        approved
    );
    assert_eq!(
        desk_post(page, &format!("{path}/lookup"), OPERATOR, site, &lookup)
            .await
            .1,
        approved
    );
    let (code, agreement) = desk_post(page, "contracts/lookup", OPERATOR, site, &lookup).await;
    assert_eq!(code, 200);
    assert!(agreement.contains("Withdrawal approved") && !agreement.contains("Review replacement"));
    assert_eq!(
        desk_post(
            page,
            &format!("{path}/credit-review"),
            OPERATOR,
            site,
            &format!("{lookup}&number=INV-1")
        )
        .await
        .0,
        403
    );
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
            journal: estate.dir.join("journals/empd.ndjson"),
            custody: estate.dir.join("custody"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("empd", "test"),
        })
        .await
        .expect("empd starts");
        let billd = billd::daemon::run(billd::daemon::Options {
            http: estate.billd,
            journal: estate.dir.join("journals/billd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("billd", "test"),
        })
        .await
        .expect("billd starts");
        let accounting = accountingd::run(accountingd::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            journal: estate.dir.join("journals/accountingd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("accountingd", "test"),
        }).await.expect("accountingd starts");
        let roamd = roamd::daemon::run(roamd::daemon::Options {
            http: estate.roamd,
            journal: estate.dir.join("journals/roamd.ndjson"),
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
            journal: estate.dir.join("journals/csmsd.ndjson"),
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
        desk_estate.services.accountingd = Some(format!("http://{}", accounting.http));
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
        line_credit_forms_require_a_finance_writer_and_same_origin(&page).await;
        finance_file_import(&page).await;
        erasure_is_reviewed_by_a_named_privacy_operator(&page).await;

        // SR-098: deskd witnesses every journalled daemon it reads.
        let rounds = desk.watch.round().await;
        assert_eq!(rounds.len(), 5, "{rounds:?}");
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
        review_private_withdrawal(&page).await;
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
        blind.services.accountingd = Some(format!("http://{}", accounting.http));
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
        accounting.kill().await;
        billd.kill().await;
        csmsd.kill().await;
        roamd.kill().await;
    })
    .await
    .expect("the story ran in under thirty seconds");
}

// The desk renders the owner's decision and recovers a stale form without
// submitting another movement. The accounting suite verifies the movement itself.
#[tokio::test]
async fn a_finance_writer_can_review_recover_and_reopen_a_credit_offset_from_the_desk() {
    use axum::extract::{Json, Request};
    use axum::http::StatusCode;
    use axum::response::IntoResponse as _;
    use serde_json::{Value, json};

    let receipt = json!({"account":"FLEET-A","currency":"EUR","source_number":"INV-A","target_number":"INV-B",
        "amount_minor":2000,"actor":"operator:Line reviewer","review":"a".repeat(64),
        "source_before_minor":-3000,"source_after_minor":-1000,"target_before_minor":8000,"target_after_minor":6000});
    let records = receipt.clone();
    let owner = axum::Router::new()
        .route("/v1/accountingd/balance", axum::routing::post(async move || Json(json!({
            "issuer":"DE*MSP","account":"FLEET-A","currency":"EUR","can_allocate":false,"can_offset":true,
            "net_due_minor":5000,"credit_minor":1000,"unapplied_minor":0,
            "documents":[{"number":"INV-A","kind":"invoice","amount_minor":10000,"predecessor":null,
                       "open_minor":0,"due_minor":-1000,"incoming_minor":0,"outgoing_minor":2000}],
            "payments":[],"held":[],"allocations":[],"offsets":[records],"next_page":1
        }))))
        .route("/v1/accountingd/withdrawal/evidence", axum::routing::post(async |Json(input): Json<Value>| Json(json!({
            "number":input["number"],"currency":"EUR","account":"FLEET-A",
            "due_minor":if input["number"]=="INV-A" {-1000} else {6000}
        }))))
        .route("/v1/accountingd/offsets/{action}", axum::routing::post(async move |req: Request| {
            assert_eq!(req.headers()["authorization"],format!("Bearer {FINANCE_WRITER}"));
            let confirm = req.uri().path().ends_with("/confirm");
            let payload: Value = serde_json::from_slice(&axum::body::to_bytes(req.into_body(),2048).await.unwrap()).unwrap();
            for key in ["source_number","target_number","currency","account","amount_minor"] {
                assert_eq!(payload[key], receipt[key], "{key}");
            }
            if confirm && payload["review"]!=receipt["review"] {
                return StatusCode::CONFLICT.into_response();
            }
            Json(receipt.clone()).into_response()
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let estate = Estate::new();
    let mut config: deskd::Estate = estate.sections();
    config.services.accountingd = Some(format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, owner).await.unwrap() });
    let desk = deskd::run(deskd::Options {
        http: "127.0.0.1:0".parse().unwrap(),
        estate: config,
        env: env(),
        shutdown: Shutdown::new(),
        identity: Identity::new("deskd", "test"),
        clock: deskd::system_clock(),
        witness: estate.dir.join("witness.ndjson"),
    })
    .await
    .unwrap();
    let page = format!("http://{}/", desk.http);

    let scope = "account=FLEET-A&currency=EUR";
    for (user, can_write) in [(FINANCE, false), (FINANCE_WRITER, true)] {
        let (status, html) =
            desk_post(&page, "receivables/balance", user, "same-origin", scope).await;
        assert_eq!(status, 200);
        assert!(html.contains("Posted credit offsets") && html.contains("operator:Line reviewer"));
        assert_eq!(
            html.contains("action=\"/receivables/offsets/review"),
            can_write
        );
        if can_write {
            assert!(html.contains(
                "Invoice with available credit<br><input type=\"text\" name=\"source_number\""
            ));
            assert!(html.contains("Invoice to pay<br><input type=\"text\" name=\"target_number\""));
            assert!(html.contains(r#"<option value="INV-A">10.00 EUR</option>"#));
            assert!(html.contains(r#"<datalist id="offset-targets"></datalist>"#));
        }
    }
    for path in ["receivables/offsets/review", "receivables/offsets/confirm"] {
        for (user, origin) in [
            (FINANCE, "same-origin"),
            (OPERATOR, "same-origin"),
            (FINANCE_WRITER, "cross-site"),
        ] {
            assert_eq!(desk_post(&page, path, user, origin, scope).await.0, 403);
        }
    }
    let selected = format!("{scope}&source_number=INV-A&target_number=INV-B");
    let (status, review) = desk_post(
        &page,
        "receivables/offsets/review",
        FINANCE_WRITER,
        "same-origin",
        &format!("{selected}&amount=20%2C00"),
    )
    .await;
    assert_eq!(status, 200);
    for text in [
        "Credit on invoice INV-A",
        "Amount due on invoice INV-B",
        "Before</dt><dd>30.00 EUR",
        "After</dt><dd>10.00 EUR",
        "Before</dt><dd>80.00 EUR",
        "After</dt><dd>60.00 EUR",
        "Confirm credit offset",
    ] {
        assert!(review.contains(text), "{text}");
    }
    let confirmation = format!(
        "{selected}&amount_minor=2000&review={}",
        hidden(&review, "review")
    );
    let (status, posted) = desk_post(
        &page,
        "receivables/offsets/confirm",
        FINANCE_WRITER,
        "same-origin",
        &confirmation,
    )
    .await;
    assert_eq!(status, 200);
    for text in [
        "Credit applied",
        "Receipt reference",
        "Confirmed by",
        "operator:Line reviewer",
    ] {
        assert!(posted.contains(text), "{text}");
    }
    assert!(!posted.contains("action=\"/receivables/offsets/confirm"));
    assert_eq!(
        desk_post(
            &page,
            "receivables/offsets/confirm",
            FINANCE_WRITER,
            "same-origin",
            &confirmation
        )
        .await
        .1,
        posted
    );
    let stale = format!("{selected}&amount_minor=2000&review={}", "e".repeat(64));
    let (status, changed) = desk_post(
        &page,
        "receivables/offsets/confirm",
        FINANCE_WRITER,
        "same-origin",
        &stale,
    )
    .await;
    assert_eq!(status, 409);
    for text in [
        "Available credit · INV-A · <strong>10.00 EUR",
        "Amount due · INV-B · <strong>60.00 EUR",
        "value=\"INV-A\"",
        "value=\"INV-B\"",
        "value=\"20.00\"",
    ] {
        assert!(changed.contains(text), "{text}");
    }
    assert!(!changed.contains("action=\"/receivables/offsets/confirm"));
    let de = reqwest::Client::new()
        .post(format!("{page}receivables/offsets/review?lang=de"))
        .header("authorization", basic(FINANCE_WRITER))
        .header("sec-fetch-site", "same-origin")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("{selected}&amount=20,00"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        de.contains("Guthaben auf Rechnung INV-A")
            && de.contains("30,00 EUR")
            && de.contains("Verrechnung bestätigen")
    );
    desk.kill().await;
    server.abort();
    std::fs::remove_dir_all(estate.dir).unwrap();
}
