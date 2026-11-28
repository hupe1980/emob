//! Retail closes through the signed receiver and the operator HTTP routes.
use super::{Scratch, dec, session};
use axum::Json;
use axum::http::StatusCode;
use emob_cdr::{Cdr, CdrBuilder, EvidenceRef};
use emob_core::Direction;
use emob_service::estate::{Env, Estate, RetailOffer, RetailTerms};
use emob_service::webhook;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::macros::datetime;

const ESTATE: &str = include_str!("../../../../deploy/estate.example.toml");
const FLEET: &str = "PAYER-FLEET";
const FINANCE: &str = "receivables-finance-test";
const SECOND_FINANCE_WRITER: &str = "second-test-finance-writer";
const DUAL: &str = "withdrawal-finance-writer";
const WRITER: &str = "retail-clerk";
const READER: &str = "retail-reader";
const SECRET: &str = "whsec_c2VjcmV0LXNlY3JldC1zZWNyZXQ=";

fn fixture() -> (Value, RetailOffer) {
    let estate: Estate = emob_service::estate::from_str(ESTATE).unwrap();
    let acct = estate
        .retail_accounts
        .iter()
        .find(|a| a.id == FLEET)
        .unwrap();
    let plan = &estate.public_plans[0];
    let mut fees = plan.schedules[0].clone();
    fees.monthly = dec("11.90");
    let mut base = estate.tariffs[0].to_tariff(0).unwrap();
    base.elements[0].components.clear();
    base.elements[0].components.push(
        emob_tariff::PriceComponent::new(emob_tariff::Dimension::Energy, dec("0.49"))
            .with_vat(dec("19")),
    );
    let terms = RetailTerms {
        point: "DE*AB7*E840*6487".parse().unwrap(),
        provider: plan.provider.clone(),
        operator: estate.parties.operator,
        code: plan.code.clone(),
        name: plan.name.clone(),
        fees,
        base,
        superseded: None,
    };
    let offer = RetailOffer {
        reference: terms.reference(),
        terms,
    };
    let agreements=['a','b'].into_iter().map(|id|json!({
        "agreement": id.to_string().repeat(64), "account":FLEET,"emaid":format!("driver-{id}"),"at":"2026-05-17T00:00:00Z","offer":offer
    })).collect::<Vec<_>>();
    (json!({"account":acct,"agreements":agreements}), offer)
}

fn env() -> Env {
    let data = emob_service::vault::DataKey::generate();
    let signing = emob_service::head::SigningKey::generate();
    Arc::new(move |key| match key {
        "EMOB_TEST_SECOND_FINANCE_WRITER_TOKEN" => Some(SECOND_FINANCE_WRITER.into()),
        "EMOB_TEST_DUAL_TOKEN" => Some(DUAL.into()),
        "EMOB_TEST_FINANCE_TOKEN" => Some(FINANCE.into()),
        "BILLD_DATA_KEY" => Some(data.clone()),
        "BILLD_SIGNING_KEY" => Some(signing.clone()),
        "EMOB_OPERATOR_TOKEN" => Some(WRITER.into()),
        "EMOB_DESK_READ_TOKEN" => Some(READER.into()),
        "EMOB_WEBHOOK_SECRET" => Some(SECRET.into()),
        _ => None,
    })
}

type CustomerAccess = Arc<Mutex<std::collections::BTreeMap<String, Value>>>;

async fn provider(profile: Arc<Mutex<Value>>) -> String {
    provider_with_customers(profile, CustomerAccess::default()).await
}

async fn provider_with_customers(profile: Arc<Mutex<Value>>, customers: CustomerAccess) -> String {
    provider_with_case(profile, customers, Arc::new(Mutex::new(Value::Null))).await
}

async fn provider_with_case(
    profile: Arc<Mutex<Value>>,
    customers: CustomerAccess,
    case: Arc<Mutex<Value>>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = axum::Router::new().route(
        "/v1/empd/contracts/billing-agreements",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, Json(req): Json<Value>| {
                let profile = Arc::clone(&profile);
                async move {
                    let token = headers[axum::http::header::AUTHORIZATION].to_str().unwrap();
                    assert!(token == "Bearer retail-clerk" || token == "Bearer retail-reader");
                    if req["account"] != FLEET {
                        return Err(StatusCode::NOT_FOUND);
                    }
                    Ok(Json(profile.lock().unwrap().clone()))
                }
            },
        ),
    );
    let routes = routes.route(
        "/v1/empd/customer/me",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let access = Arc::clone(&customers);
                async move {
                    assert_eq!(body, json!({}));
                    let session = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .ok_or(StatusCode::UNAUTHORIZED)?;
                    let grants = access
                        .lock()
                        .unwrap()
                        .get(session)
                        .cloned()
                        .ok_or(StatusCode::UNAUTHORIZED)?;
                    Ok::<_, StatusCode>(Json(grants))
                }
            },
        ),
    );
    let routes = routes.route(
        "/v1/empd/contracts/withdrawal/lookup",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, Json(req): Json<Value>| {
                let case = Arc::clone(&case);
                async move {
                    assert_eq!(headers["authorization"], format!("Bearer {DUAL}"));
                    let row = case.lock().unwrap();
                    if row["agreement"] != req["agreement"] {
                        return Err(StatusCode::NOT_FOUND);
                    }
                    Ok(Json(row.clone()))
                }
            },
        ),
    );
    tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    format!("http://{addr}")
}

async fn run(scratch: &Scratch, provider: &str, env: Env) -> billd::daemon::Running {
    let mut estate: billd::daemon::Estate = emob_service::estate::from_str(ESTATE).unwrap();
    estate.operators.push(
        serde_json::from_value(json!({
            "name": "receivables-test", "token_env": "EMOB_TEST_FINANCE_TOKEN", "finance": true
        }))
        .unwrap(),
    );
    estate.operators.push(serde_json::from_value(json!({"name":"withdrawal-finance","token_env":"EMOB_TEST_DUAL_TOKEN","write":true,"finance":true})).unwrap());
    estate.operators.push(serde_json::from_value(json!({"name":"second finance writer","token_env":"EMOB_TEST_SECOND_FINANCE_WRITER_TOKEN","write":true,"finance":true})).unwrap());
    estate.services.empd = provider.to_owned();
    estate.partners.clear();
    estate.billing.vat_rate = dec("7"); // Wholesale configuration is independent.
    billd::daemon::run_with_clock(
        billd::daemon::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            journal: scratch.0.clone(),
            estate,
            env,
            shutdown: emob_service::Shutdown::new(),
            identity: emob_service::Identity::new("billd", "test"),
        },
        Arc::new(|| datetime!(2026-07-01 00:00 UTC)),
    )
    .await
    .unwrap()
}

fn cdr(offer: &RetailOffer, n: i64) -> Cdr {
    let (s, e) = session(n, &format!("retail-{n}"), "150.0", "160.0");
    CdrBuilder::from_session(&s, Direction::Import)
        .unwrap()
        .key(
            offer.terms.operator.clone(),
            format!("retail-{n}").parse().unwrap(),
        )
        .evidence(EvidenceRef::from_evidence(&e, "OCMF"))
        .rated_with(&offer.terms.base)
        .build()
        .unwrap()
}

async fn deliver(
    running: &billd::daemon::Running,
    offer: &RetailOffer,
    n: i64,
    bound: bool,
) -> StatusCode {
    let record = cdr(offer, n);
    let mut data =
        json!({"cdr":record, "token":{"issuer":offer.terms.provider,"contract_id":"driver-a"}});
    if bound {
        data["retail"] = json!({"agreement":"a".repeat(64),"reference":offer.reference});
    }
    signed_delivery(running, &record.key.to_string(), data).await
}

async fn signed_delivery(running: &billd::daemon::Running, key: &str, data: Value) -> StatusCode {
    let payload = serde_json::to_vec(&emob_service::cloudevent::CloudEvent::new(
        key,
        "csmsd",
        emob_service::events::cdr::ISSUED,
        data,
    ))
    .unwrap();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let sig = webhook::sign(
        &webhook::Delivery {
            id: key,
            timestamp: now,
            payload: &payload,
        },
        &webhook::Secret::standard(SECRET).unwrap(),
    )
    .unwrap();
    reqwest::Client::new()
        .post(format!("http://{}/v1/deliveries", running.http))
        .header(webhook::ID_HEADER, key)
        .header(webhook::TIMESTAMP_HEADER, now.to_string())
        .header(webhook::SIGNATURE_HEADER, sig)
        .body(payload)
        .send()
        .await
        .unwrap()
        .status()
}

fn june() -> Value {
    json!({"account":FLEET,"period":"2026-06"})
}
async fn call(
    running: &billd::daemon::Running,
    action: &str,
    token: &str,
    request: &Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/billd/retail/{action}", running.http))
        .bearer_auth(token)
        .json(request)
        .send()
        .await
        .unwrap()
}
async fn json_call(running: &billd::daemon::Running, action: &str, request: &Value) -> Value {
    let res = call(running, action, WRITER, request).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["cache-control"], "private, no-store");
    res.json().await.unwrap()
}

// driver-monthly-invoice/SR-304
#[tokio::test]
async fn account_close_replays_without_usage_and_credits_a_late_signed_charge() {
    let (roster, offer) = fixture();
    let roster = Arc::new(Mutex::new(roster));
    let url = provider(Arc::clone(&roster)).await;
    let dir = Scratch::new("retail-replay");
    let env = env();
    let first = run(&dir, &url, Arc::clone(&env)).await;
    let ready = json_call(&first, "review", &june()).await;
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["payable"], "23.80");
    assert_eq!(ready["net"], "20.00");
    let mut close = june();
    close["review"] = ready["review"].clone();
    assert_eq!(
        call(&first, "close", READER, &close).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&first, "lookup", "unrelated-customer", &june())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let (issued, retry) = tokio::join!(
        json_call(&first, "close", &close),
        json_call(&first, "close", &close)
    );
    assert_eq!(issued, retry);
    let mut original = june();
    original["number"] = issued["number"].clone();
    for token in [WRITER, READER] {
        assert_eq!(
            call(&first, "document", token, &original).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    let file = call(&first, "document", FINANCE, &original)
        .await
        .text()
        .await
        .unwrap();
    assert!(!file.contains("CDR ") && !file.contains("ZZ*SUB"));
    assert!(file.contains("MSP Mobility GmbH") && file.contains("Musterflotte GmbH"));
    assert_eq!(json_call(&first, "close", &close).await, issued);
    first.kill().await;
    let second = run(&dir, &url, env).await;
    assert_eq!(json_call(&second, "lookup", &june()).await, issued);
    assert_eq!(deliver(&second, &offer, 1, true).await, StatusCode::OK);
    assert_eq!(deliver(&second, &offer, 1, true).await, StatusCode::OK);
    assert_eq!(
        deliver(&second, &offer, 1, false).await,
        StatusCode::CONFLICT
    );
    let revised = json_call(&second, "review", &june()).await;
    assert_eq!(revised["payable"], "30.10", "{revised}");
    assert_eq!(revised["supersedes"], issued["number"]);
    let mut correction = june();
    correction["review"] = revised["review"].clone();
    let corrected = json_call(&second, "close", &correction).await;
    assert!(corrected["credit"].is_string());
    assert_ne!(issued["number"], corrected["number"]);
    assert_eq!(
        call(&second, "document", FINANCE, &original)
            .await
            .text()
            .await
            .unwrap(),
        file
    );
    let mut other = original.clone();
    other["account"] = Value::from("PAYER-PRIVATE");
    assert_eq!(
        call(&second, "document", FINANCE, &other).await.status(),
        StatusCode::NOT_FOUND
    );
    let mut credit = june();
    credit["number"] = corrected["credit"].clone();
    assert!(
        call(&second, "document", FINANCE, &credit)
            .await
            .text()
            .await
            .unwrap()
            .contains("CreditNote")
    );
    assert_eq!(json_call(&second, "close", &close).await, issued);
    assert_eq!(deliver(&second, &offer, 2, false).await, StatusCode::OK);
    let held = json_call(&second, "review", &june()).await;
    assert_eq!(held["state"], "held");
    assert_eq!(held["withheld"][0]["reason"], "authorization_mismatch");
    assert_eq!(json_call(&second, "lookup", &june()).await, corrected);
    let mut open = june();
    open["period"] = Value::from("2026-07");
    assert_eq!(
        call(&second, "review", READER, &open).await.status(),
        StatusCode::CONFLICT
    );
    second.kill().await;
}

// driver-monthly-invoice/SR-303
#[tokio::test]
async fn changed_profile_or_partial_fee_refuses_confirmation() {
    let (profile, _) = fixture();
    let roster = Arc::new(Mutex::new(profile));
    let url = provider(Arc::clone(&roster)).await;
    let dir = Scratch::new("retail-refusal");
    let running = run(&dir, &url, env()).await;
    let initial = json_call(&running, "review", &june()).await;
    let mut confirmation = june();
    confirmation["review"] = initial["review"].clone();
    roster.lock().unwrap()["agreements"][0]["at"] = Value::from("2026-06-02T08:00:00Z");
    let held = json_call(&running, "review", &june()).await;
    assert_eq!(held["state"], "held");
    assert_eq!(held["withheld"][0]["reason"], "partial_month");
    assert_eq!(
        call(&running, "close", WRITER, &confirmation)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&running, "lookup", WRITER, &june()).await.status(),
        StatusCode::NOT_FOUND
    );
    {
        let mut roster = roster.lock().unwrap();
        roster["agreements"][0]["at"] = Value::from("2026-05-17T00:00:00Z");
        roster["agreements"][0]["ended_at"] = Value::from("2026-07-01T00:00:00Z");
        roster["agreements"].as_array_mut().unwrap().reverse();
    }
    assert_eq!(json_call(&running, "review", &june()).await, initial);
    roster.lock().unwrap()["agreements"][1]["ended_at"] = Value::from("2026-06-15T00:00:00Z");
    assert_eq!(
        json_call(&running, "review", &june()).await["withheld"][0]["reason"],
        "partial_month"
    );
    roster.lock().unwrap()["agreements"][1]["ended_at"] = Value::from("2026-06-01T00:00:00Z");
    assert_eq!(
        json_call(&running, "review", &june()).await["payable"],
        "11.90"
    );
    running.kill().await;
}

#[tokio::test]
async fn unrelated_card_is_excluded_and_unsigned_meter_data_holds_the_native_period() {
    let (profile, offer) = fixture();
    let url = provider(Arc::new(Mutex::new(profile))).await;
    let dir = Scratch::new("retail-evidence");
    let running = run(&dir, &url, env()).await;
    let initial = json_call(&running, "review", &june()).await;

    let ordinary = cdr(&offer, 1);
    let data = json!({"cdr": ordinary, "token": {"issuer": offer.terms.provider, "contract_id":"ordinary-seed"}});
    assert_eq!(
        signed_delivery(&running, &ordinary.key.to_string(), data).await,
        StatusCode::OK
    );
    assert_eq!(json_call(&running, "review", &june()).await, initial);

    let mut unsigned = cdr(&offer, 2);
    unsigned.evidence = None;
    let data = json!({"cdr": unsigned, "token": {"issuer": offer.terms.provider, "contract_id":"driver-a"}, "retail": {"agreement": "a".repeat(64), "reference":offer.reference}});
    assert_eq!(
        signed_delivery(&running, &unsigned.key.to_string(), data).await,
        StatusCode::OK
    );
    let held = json_call(&running, "review", &june()).await;
    assert_eq!(held["state"], "held");
    assert_eq!(held["withheld"][0]["reason"], "unverified_evidence");
    let mut close = june();
    close["review"] = initial["review"].clone();
    assert_eq!(
        call(&running, "close", WRITER, &close).await.status(),
        StatusCode::CONFLICT
    );
    running.kill().await;
}

#[tokio::test]
async fn confirmed_result_and_archived_xml_survive_provider_unavailability() {
    let (profile, _) = fixture();
    let roster = Arc::new(Mutex::new(profile));
    let url = provider(Arc::clone(&roster)).await;
    let dir = Scratch::new("retail-provider-offline");
    let running = run(&dir, &url, env()).await;
    let ready = json_call(&running, "review", &june()).await;
    let mut close = june();
    close["review"] = ready["review"].clone();
    let issued = json_call(&running, "close", &close).await;
    let before = call(&running, "document", FINANCE, &june())
        .await
        .bytes()
        .await
        .unwrap();
    *roster.lock().unwrap() = Value::Null;
    assert_eq!(
        call(&running, "review", WRITER, &june()).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(json_call(&running, "close", &close).await, issued);
    assert_eq!(json_call(&running, "lookup", &june()).await, issued);
    assert_eq!(
        call(&running, "document", FINANCE, &june())
            .await
            .bytes()
            .await
            .unwrap(),
        before
    );
    running.kill().await;
}

#[path = "customer.rs"]
mod customer;

#[path = "feed.rs"]
mod feed;

#[path = "withdrawal.rs"]
mod withdrawal;

#[path = "adjustment.rs"]
mod adjustment;
