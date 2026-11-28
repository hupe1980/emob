//! Operator intake, reviewed disposition and the real station decision across an owner restart.
use super::{authorize_card, demo, enrol_action, journal_path, retail_fixture, start_with_clock};
use axum::{Json, Router};
use emob_service::estate::{EnvRef, Operator};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;

const WRITER: Option<&str> = Some("operator-token");

async fn issued(addr: SocketAddr, payer: &str, stock: &str) -> Value {
    let offer = retail_fixture();
    let input =
        json!({"account":payer,"stock":stock,"point":offer.terms.point,"plan":offer.terms.code});
    let (status, review) = enrol_action(addr, "preview", WRITER, &input).await;
    assert_eq!(status, 200);
    let (status, sale) = enrol_action(
        addr,
        "confirm",
        WRITER,
        &json!({"operation":review["operation"]}),
    )
    .await;
    assert_eq!(status, 200);
    sale
}

async fn case_api(
    addr: SocketAddr,
    action: &str,
    bearer: Option<&str>,
    input: &Value,
    expected: u16,
) -> Value {
    let (status, value) = enrol_action(addr, &format!("withdrawal/{action}"), bearer, input).await;
    assert_eq!(status, expected, "{action}");
    value
}

async fn estate_with_prices() -> (empd::daemon::Estate, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut estate = demo();
    estate.desk.as_mut().unwrap().csmsd = format!("http://{}", listener.local_addr().unwrap());
    estate.operators.push(Operator {
        name: "second-writer".into(),
        token_env: EnvRef::new("CUSTOMER_TEST_OTHER"),
        write: true,
        finance: false,
        privacy: false,
    });
    let offer = retail_fixture();
    let routes = Router::new().route(
        "/v1/csmsd/retail-offers/{point}/{provider}/{plan}",
        axum::routing::get(move || async move { Json(offer) }),
    );
    let prices = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    (estate, prices)
}

// SR-361, SR-362, SR-363: one recorded entitlement decision and an attributable retry.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_reviewed_withdrawal_retires_only_the_active_card_and_replays_its_receipt() {
    let (estate, prices) = estate_with_prices().await;
    let second_token = "b3".repeat(32);
    let other = Some(second_token.as_str());
    let path = journal_path("withdrawal");
    let frozen = time::macros::datetime!(2020-02-01 12:00 UTC);
    let clock: emob_service::Clock = Arc::new(move || frozen);
    let first = start_with_clock(&path, estate.clone(), Arc::clone(&clock)).await;
    let addr = first.http;
    let sale = issued(addr, "PAYER-PRIVATE", "STOCK-1004").await;
    let fleet = issued(addr, "PAYER-FLEET", "STOCK-1005").await;
    let scope = json!({"agreement":sale["agreement"]});
    let intake = json!({"agreement":sale["agreement"],"request_ref":"LETTER-RECEIVED-17","received_at":sale["at"],"channel":"letter"});
    let proposal = json!({"agreement":sale["agreement"],"outcome":"approve","evidence_ref":"TERMS-AND-PERFORMANCE-04","billing":"review_required"});
    for route in ["lookup", "intake", "review", "confirm"] {
        for (token, expected) in [
            (Some("reader-token"), 403),
            (Some("service-token"), 401),
            (None, 401),
        ] {
            case_api(addr, route, token, &scope, expected).await;
        }
    }
    case_api(addr, "lookup", WRITER, &scope, 404).await;
    let mut fleet_intake = intake.clone();
    fleet_intake["agreement"] = fleet["agreement"].clone();
    case_api(addr, "intake", WRITER, &fleet_intake, 422).await;
    let mut future = intake.clone();
    future["received_at"] = json!("2020-02-02T00:00:00Z");
    case_api(addr, "intake", WRITER, &future, 400).await;
    let case = case_api(addr, "intake", WRITER, &intake, 200).await;
    assert_eq!(case["stage"], "received");
    assert_eq!(case["offer_reference"], sale["offer"]["reference"]);
    assert_eq!(case_api(addr, "intake", other, &intake, 200).await, case);
    let mut changed = intake.clone();
    changed["channel"] = json!("phone");
    case_api(addr, "intake", WRITER, &changed, 409).await;

    let mut hold = proposal.clone();
    hold["outcome"] = json!("hold");
    let review = case_api(addr, "review", WRITER, &hold, 200).await;
    assert_eq!(review["blocks_online"], false);
    let hold_op = json!({"agreement":sale["agreement"],"operation":review["operation"]});
    case_api(addr, "confirm", other, &hold_op, 409).await;
    let held = case_api(addr, "confirm", WRITER, &hold_op, 200).await;
    assert_eq!(held["stage"], "held");
    assert_eq!(
        authorize_card(addr, "04AB0000000004").await["status"],
        "Accepted"
    );

    let stale = case_api(addr, "review", WRITER, &proposal, 200).await;
    let replace = json!({"agreement":sale["agreement"],"action":"replace","stock":"STOCK-1006"});
    let (status, replace_review) = enrol_action(addr, "change/preview", WRITER, &replace).await;
    assert_eq!(status, 200);
    assert_eq!(
        enrol_action(
            addr,
            "change/confirm",
            WRITER,
            &json!({"agreement":sale["agreement"],"operation":replace_review["operation"]})
        )
        .await
        .0,
        200
    );
    case_api(
        addr,
        "confirm",
        WRITER,
        &json!({"agreement":sale["agreement"],"operation":stale["operation"]}),
        409,
    )
    .await;

    let approve = case_api(addr, "review", WRITER, &proposal, 200).await;
    assert_eq!(approve["blocks_online"], true);
    let competing = case_api(addr, "review", other, &proposal, 200).await;
    let confirmation = json!({"agreement":sale["agreement"],"operation":approve["operation"]});
    let (one, two) = tokio::join!(
        case_api(addr, "confirm", WRITER, &confirmation, 200),
        case_api(addr, "confirm", WRITER, &confirmation, 200)
    );
    assert_eq!(one, two);
    assert_eq!(one["stage"], "approved");
    assert_eq!(one["decision"]["evidence_ref"], proposal["evidence_ref"]);
    case_api(
        addr,
        "confirm",
        other,
        &json!({"agreement":sale["agreement"],"operation":competing["operation"]}),
        409,
    )
    .await;
    assert_eq!(
        authorize_card(addr, "04AB0000000006").await["status"],
        "Blocked"
    );
    assert_eq!(
        authorize_card(addr, "04AB0000000005").await["status"],
        "Accepted"
    );
    assert_eq!(
        enrol_action(addr, "lookup", WRITER, &scope).await.1["withdrawal_status"],
        "approved"
    );
    case_api(addr, "review", WRITER, &hold, 409).await;
    assert_eq!(
        enrol_action(addr, "change/preview", WRITER, &replace)
            .await
            .0,
        409
    );
    let roster = enrol_action(
        addr,
        "billing-agreements",
        WRITER,
        &json!({"account":"PAYER-PRIVATE"}),
    )
    .await
    .1;
    assert_eq!(roster["agreements"][0]["ended_at"], one["decision"]["at"]);
    first.kill().await;
    let journal = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        journal
            .lines()
            .filter(|line| line.contains("\"kind\":\"withdrawal\""))
            .count(),
        3
    );
    assert!(
        !journal.contains("LETTER-RECEIVED-17") && !journal.contains("TERMS-AND-PERFORMANCE-04")
    );
    let wrong_key = empd::Empd::new(
        "DE*MSP".parse().unwrap(),
        emob_service::vault::DataKey::from_bytes(&[42; 32]),
    );
    let cut = emob_service::journal::Memory::from_cold_cut(journal.as_bytes()).unwrap();
    assert!(empd::Book::open(wrong_key, cut).is_err());
    let restored = start_with_clock(&path, estate, clock).await;
    assert_eq!(
        case_api(restored.http, "lookup", WRITER, &scope, 200).await,
        one
    );
    assert_eq!(
        case_api(restored.http, "confirm", WRITER, &confirmation, 200).await,
        one
    );
    assert_eq!(
        case_api(restored.http, "confirm", WRITER, &hold_op, 200).await,
        held
    );
    case_api(restored.http, "confirm", other, &confirmation, 409).await;
    assert_eq!(
        authorize_card(restored.http, "04AB0000000006").await["status"],
        "Blocked"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), journal);
    restored.kill().await;
    prices.abort();
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}

// SR-362: a decline preserves the entitlement; an earlier ordinary termination keeps its effective time.
#[tokio::test]
async fn a_decline_preserves_standing_and_withdrawal_keeps_an_earlier_term_end() {
    let (mut estate, prices) = estate_with_prices().await;
    let path = journal_path("two-withdrawals");
    let now = Arc::new(std::sync::Mutex::new(
        time::macros::datetime!(2020-02-01 12:00 UTC),
    ));
    let service_time = Arc::clone(&now);
    let clock: emob_service::Clock = Arc::new(move || *service_time.lock().unwrap());
    let first = start_with_clock(&path, estate.clone(), Arc::clone(&clock)).await;
    let addr = first.http;
    let continuing = issued(addr, "PAYER-PRIVATE", "STOCK-1004").await;
    let ended = issued(addr, "PAYER-PRIVATE", "STOCK-1005").await;
    let received = |sale: &Value, reference: &str| json!({"agreement":sale["agreement"],"request_ref":reference,"received_at":sale["at"],"channel":"phone"});
    case_api(
        addr,
        "intake",
        WRITER,
        &received(&continuing, "SUPPORT-01"),
        200,
    )
    .await;
    case_api(addr, "intake", WRITER, &received(&ended, "SUPPORT-01"), 409).await;
    case_api(addr, "intake", WRITER, &received(&ended, "SUPPORT-02"), 200).await;
    let propose = |sale: &Value, outcome: &str| json!({"agreement":sale["agreement"],"outcome":outcome,"evidence_ref":"DOSSIER-02","billing":"unaffected"});
    let (status, end) = enrol_action(
        addr,
        "change/preview",
        WRITER,
        &json!({"agreement":ended["agreement"],"action":"end"}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, end) = enrol_action(
        addr,
        "change/confirm",
        WRITER,
        &json!({"agreement":ended["agreement"],"operation":end["operation"]}),
    )
    .await;
    assert_eq!(status, 200);

    let operation = |sale: &Value, review: &Value| json!({"agreement":sale["agreement"],"operation":review["operation"]});
    let expired = case_api(
        addr,
        "review",
        WRITER,
        &propose(&continuing, "decline"),
        200,
    )
    .await;
    *now.lock().unwrap() += time::Duration::minutes(5);
    case_api(
        addr,
        "confirm",
        WRITER,
        &operation(&continuing, &expired),
        409,
    )
    .await;
    let decline = case_api(
        addr,
        "review",
        WRITER,
        &propose(&continuing, "decline"),
        200,
    )
    .await;
    let result = case_api(
        addr,
        "confirm",
        WRITER,
        &operation(&continuing, &decline),
        200,
    )
    .await;
    assert_eq!(result["stage"], "declined");
    case_api(
        addr,
        "review",
        WRITER,
        &propose(&continuing, "approve"),
        409,
    )
    .await;
    assert_eq!(
        authorize_card(addr, "04AB0000000004").await["status"],
        "Accepted"
    );
    let approve = case_api(addr, "review", WRITER, &propose(&ended, "approve"), 200).await;
    let result = case_api(addr, "confirm", WRITER, &operation(&ended, &approve), 200).await;
    assert_eq!(result["stage"], "approved");
    assert_ne!(end["at"], result["decision"]["at"]);
    let (status, roster) = enrol_action(
        addr,
        "billing-agreements",
        WRITER,
        &json!({"account":"PAYER-PRIVATE"}),
    )
    .await;
    assert_eq!(status, 200);
    let row = roster["agreements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agreement"] == ended["agreement"])
        .unwrap();
    assert_eq!(row["ended_at"], end["at"]);
    let later = issued(addr, "PAYER-PRIVATE", "STOCK-1006").await;
    first.kill().await;
    estate
        .retail_accounts
        .retain(|payer| payer.id != "PAYER-PRIVATE");
    let reopened = start_with_clock(&path, estate, clock).await;
    let case = case_api(
        reopened.http,
        "intake",
        WRITER,
        &received(&later, "SUPPORT-03"),
        200,
    )
    .await;
    assert_eq!(case["account"], "PAYER-PRIVATE");
    reopened.kill().await;
    prices.abort();
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}

// SR-363: the current agreement grant controls visibility at the data owner.
#[tokio::test]
async fn only_the_agreement_grantee_sees_the_case_stage_and_it_contains_no_case_evidence() {
    use super::customer_access::{api, fixture, signin};

    let (source, prices) = estate_with_prices().await;
    let mut estate = fixture();
    estate.desk = source.desk;
    let path = journal_path("withdrawal-customer");
    let owner = start_with_clock(
        &path,
        estate,
        Arc::new(|| time::macros::datetime!(2020-02-01 12:00 UTC)),
    )
    .await;
    let addr = owner.http;
    let sale = issued(addr, "PAYER-PRIVATE", "STOCK-1004").await;
    let intake = json!({"agreement":sale["agreement"],"request_ref":"CUSTOMER-DECLARATION-01","received_at":sale["at"],"channel":"letter"});
    case_api(addr, "intake", WRITER, &intake, 200).await;
    let proposal = json!({"agreement":sale["agreement"],"outcome":"hold","evidence_ref":"CONFIDENTIAL-STAFF-ARCHIVE","billing":"review_required"});
    let review = case_api(addr, "review", WRITER, &proposal, 200).await;
    case_api(
        addr,
        "confirm",
        WRITER,
        &json!({"agreement":sale["agreement"],"operation":review["operation"]}),
        200,
    )
    .await;
    let driver = "driver-test-034";
    let payer = "other-test-034";
    let grant = |subject: &str, kind: &str, id: &Value, granted: bool, revision: u64| json!({"subject":subject,"kind":kind,"reference":id,"granted":granted,"revision":revision});
    assert_eq!(
        api(
            addr,
            "grants",
            WRITER,
            grant(driver, "agreement", &sale["agreement"], true, 0)
        )
        .await
        .0,
        200
    );
    assert_eq!(
        api(
            addr,
            "grants",
            WRITER,
            grant(payer, "account", &sale["account"], true, 0)
        )
        .await
        .0,
        200
    );
    let driver_session = signin(addr, driver).await;
    let payer_session = signin(addr, payer).await;
    let (status, view) = api(addr, "me", Some(&driver_session), json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(view["agreements"][0]["withdrawal_status"], "held");
    let visible = view.to_string();
    for staff in [
        "CONFIDENTIAL-STAFF-ARCHIVE",
        "CUSTOMER-DECLARATION-01",
        "review_required",
        "evidence_ref",
        "recorded_by",
        review["operation"].as_str().unwrap(),
    ] {
        assert!(!visible.contains(staff));
    }
    let (status, payer_view) = api(addr, "me", Some(&payer_session), json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(payer_view["agreements"], json!([]));
    case_api(
        addr,
        "lookup",
        Some(&driver_session),
        &json!({"agreement":sale["agreement"]}),
        401,
    )
    .await;
    assert_eq!(
        api(
            addr,
            "grants",
            WRITER,
            grant(driver, "agreement", &sale["agreement"], false, 1)
        )
        .await
        .0,
        200
    );
    assert_eq!(
        api(addr, "me", Some(&driver_session), json!({})).await.1["agreements"],
        json!([])
    );
    owner.kill().await;
    prices.abort();
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}
