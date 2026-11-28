//! 0.2: `csmsd` asks before a transaction opens, and `empd` answers.
//!
//! The contracts come from the estate, once; the answer is the book's; the
//! question is asked over a socket that takes the service token and nobody
//! else's, and the operator's view of the store never shows a uid.

use std::collections::BTreeMap;
use std::sync::Arc;

use emob_service::estate::{self, Env};
use emob_service::journal::{Journal, Memory};
use emob_service::{Identity, Shutdown};
use empd::daemon::{Estate, Options};
use empd::http::{Status, decide};
use empd::{Book, Empd};

const DEMO: &str = include_str!("../../../deploy/estate.example.toml");

fn demo() -> Estate {
    estate::from_str(DEMO).expect("the demo estate")
}

fn env() -> Env {
    let values: BTreeMap<&str, &str> = [
        (
            "EMPD_DATA_KEY",
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        ),
        (
            "EMPD_SIGNING_KEY",
            "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
        ),
        ("EMOB_SERVICE_TOKEN", "service-token"),
        ("EMOB_OPERATOR_TOKEN", "operator-token"),
        ("EMOB_DESK_READ_TOKEN", "reader-token"),
        ("PRIVACY_TEST", "privacy-token"),
        ("PRIVACY_SECOND", "second-privacy"),
    ]
    .into_iter()
    .collect();
    Arc::new(move |name| match name {
        "CUSTOMER_TEST_KEY" => Some("9a".repeat(32)),
        "CUSTOMER_TEST_OTHER" => Some("b3".repeat(32)),
        _ => values.get(name).map(|v| (*v).to_owned()),
    })
}

fn book() -> Book<Memory> {
    let mut book = Book::open(
        Empd::new(
            "DE*MSP".parse().unwrap(),
            emob_service::vault::DataKey::from_bytes(&[1; 32]),
        ),
        Memory::new(),
    )
    .unwrap();
    empd::seed::apply(&mut book, &demo().contracts).expect("the demo contracts seed");
    book
}

fn today() -> time::Date {
    time::macros::date!(2026 - 11 - 18)
}

#[test]
fn the_estates_contracts_are_seeded_once_and_a_second_start_changes_nothing() {
    let mut book = book();
    let written = empd::seed::apply(&mut book, &demo().contracts).unwrap();
    assert_eq!(
        written, 0,
        "a seed already journalled is not journalled again"
    );
    assert_eq!(book.state().contracts().count(), 1);
    assert_eq!(
        book.state().tokens().count(),
        3,
        "two drivers and a blocked card"
    );
    let mut seeds = demo().contracts;
    seeds[0].tokens[0].visual_number = Some("REPLACED".into());
    let original_number = seeds[0].tokens[2]
        .visual_number
        .replace("bad\nlabel".into());
    let initial = book.journal().head().unwrap();
    assert!(empd::seed::apply(&mut book, &seeds).is_err());
    assert_eq!(
        book.journal().head().unwrap(),
        initial,
        "all printed numbers are checked before the first change"
    );
    seeds[0].tokens[2].visual_number = original_number;
    assert_eq!(empd::seed::apply(&mut book, &seeds).unwrap(), 1);
    seeds[0].tokens[0].visual_number = None;
    assert_eq!(empd::seed::apply(&mut book, &seeds).unwrap(), 1);
    assert_eq!(empd::seed::apply(&mut book, &seeds).unwrap(), 0);
}

#[test]
fn a_token_the_provider_blocked_is_answered_blocked() {
    let book = book();
    let answer = decide(&book, "04DEADBEEF0001", today());
    assert_eq!(answer.status, Status::Blocked);
    assert!(answer.token.is_none(), "a refusal names nobody to bill");
}

#[test]
fn a_token_in_force_is_accepted_with_whom_it_bills() {
    let book = book();
    let answer = decide(&book, "04A1B2C3D4E5F6", today());
    assert_eq!(answer.status, Status::Accepted, "{answer:?}");
    let token = answer.token.expect("who pays");
    assert_eq!(token.contract_id.as_str(), "DE-MSP-C00000001-W");
    assert_eq!(token.issuer.to_string(), "DE*MSP");
    assert_eq!(answer.reference.map(|r| r.len()), Some(64));
}

#[test]
fn a_uid_nobody_issued_and_a_contract_not_yet_in_force_are_refusals_with_reasons() {
    let book = book();
    let unknown = decide(&book, "nobody", today());
    assert_eq!(unknown.status, Status::Invalid);
    assert!(unknown.reason.contains("no token"), "{unknown:?}");

    let early = decide(&book, "04A1B2C3D4E5F6", time::macros::date!(2025 - 06 - 01));
    assert_eq!(early.status, Status::Expired);
}

fn journal_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!(
            "empd-asked-{name}-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ))
        .join("journals/empd.ndjson")
}

fn fixture_root(journal: &std::path::Path) -> &std::path::Path {
    assert!(journal.ends_with("journals/empd.ndjson"));
    journal.ancestors().nth(2).unwrap()
}

async fn started(name: &str) -> empd::daemon::Running {
    start_at(&journal_path(name)).await
}

async fn start_at(path: &std::path::Path) -> empd::daemon::Running {
    start_with(path, demo()).await
}

async fn start_with(path: &std::path::Path, estate: Estate) -> empd::daemon::Running {
    start_with_clock(path, estate, emob_service::system_clock()).await
}

async fn start_with_clock(
    path: &std::path::Path,
    estate: Estate,
    clock: emob_service::Clock,
) -> empd::daemon::Running {
    empd::daemon::run_with_clock(
        Options {
            http: "127.0.0.1:0".parse().unwrap(),
            journal: path.to_owned(),
            custody: fixture_root(path).join("custody"),
            estate,
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("empd", "test"),
        },
        clock,
    )
    .await
    .expect("empd starts on the demo estate")
}

/// A tiny HTTP/1.1 client over a raw socket, so this test carries no client
/// crate: one request, the status line and the body back.
async fn call(addr: std::net::SocketAddr, request: &str) -> (u16, String) {
    let response = raw_call(addr, request).await;
    let status = response[9..12].parse().unwrap();
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    (status, body.to_owned())
}

async fn raw_call(addr: std::net::SocketAddr, request: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

fn post(addr: std::net::SocketAddr, token: Option<&str>, body: &str) -> String {
    post_to(addr, "/v1/authorize", token, body)
}

fn post_to(addr: std::net::SocketAddr, path: &str, token: Option<&str>, body: &str) -> String {
    let auth = token.map_or_else(String::new, |t| format!("authorization: Bearer {t}\r\n"));
    format!(
        "POST {path} HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\ncontent-type: application/json\r\n{auth}content-length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn get(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> String {
    let auth = token.map_or_else(String::new, |t| format!("authorization: Bearer {t}\r\n"));
    format!("GET {path} HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\n{auth}\r\n")
}

#[tokio::test]
async fn sr_033_only_the_service_token_may_ask() {
    let empd = started("service").await;
    let body = r#"{"uid":"04A1B2C3D4E5F6"}"#;
    assert_eq!(call(empd.http, &post(empd.http, None, body)).await.0, 401);
    assert_eq!(
        call(empd.http, &post(empd.http, Some("operator-token"), body))
            .await
            .0,
        401,
        "an operator token is not the service token"
    );
    let (status, answer) = call(empd.http, &post(empd.http, Some("service-token"), body)).await;
    assert_eq!(status, 200);
    assert!(answer.contains(r#""status":"Accepted""#), "{answer}");
    empd.kill().await;
}

#[tokio::test]
async fn sr_034_the_operators_view_of_the_store_shows_no_uid() {
    let empd = started("operator").await;
    assert_eq!(
        call(empd.http, &get(empd.http, "/v1/empd", None)).await.0,
        401
    );
    let (status, body) = call(
        empd.http,
        &get(empd.http, "/v1/empd", Some("operator-token")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("DE-MSP-C00000001-W"), "{body}");
    for uid in ["04A1B2C3D4E5F6", "04DEADBEEF0001"] {
        assert!(!body.contains(uid), "a uid is personal data: {body}");
    }
    assert_eq!(
        call(empd.http, &get(empd.http, "/v1/empd/head", None))
            .await
            .0,
        401,
        "the signed head is a read model like any other (SR-103)"
    );
    let (status, note) = call(
        empd.http,
        &get(empd.http, "/v1/empd/head", Some("operator-token")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(!note.is_empty(), "a started empd has signed its head");
    empd.kill().await;
}

#[test]
fn sr_096_a_journal_cut_by_one_record_does_not_start() {
    use emob_service::head::{Signing, SigningKey, beside};
    use emob_service::journal::{File, JournalError};

    let dir = std::env::temp_dir().join(format!(
        "empd-cut-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("empd.ndjson");
    let key = || SigningKey::from_seed(&[2; 32], "empd");
    let open = || Signing::open(File::open(&path).unwrap(), key(), beside(&path));
    let provider = || {
        Empd::new(
            "DE*MSP".parse().unwrap(),
            emob_service::vault::DataKey::from_bytes(&[1; 32]),
        )
    };

    let written = {
        let mut book = Book::open(provider(), open().expect("a new journal opens")).unwrap();
        empd::seed::apply(&mut book, &demo().contracts).expect("the demo contracts seed")
    };
    assert!(written >= 2, "the seed wrote facts to cut: {written}");

    // Cut the last record, as a restore of yesterday's copy would.
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().collect();
    let cut = kept[..kept.len() - 1].join("\n") + "\n";
    std::fs::write(&path, cut).unwrap();

    let refused = open().expect_err("a journal shorter than its signed head does not open");
    let held = kept.len() as u64 - 1;
    assert!(
        matches!(
            refused,
            JournalError::Truncated { journal, signed, missing: 1 }
                if journal == held && signed == held + 1
        ),
        "{refused:?}"
    );
    let said = refused.to_string();
    assert!(
        said.contains(&format!("holds {held} records"))
            && said.contains(&format!("at {}", held + 1)),
        "the refusal names both positions: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// around-a-session/SR-030 and around-a-session/SR-031: only the named operator can resolve sensitive search values.
// Both decoding failures and valid lookups stay out of caches.
#[tokio::test]
async fn card_search_requires_operator_authority_and_keeps_identifiers_private() {
    use serde_json::{Value, json};
    let provider = started("card-search").await;
    let addr = provider.http;
    let path = "/v1/empd/cards/lookup";
    let owner = Some("operator-token");
    let request = |bearer, query: Value| post_to(addr, path, bearer, &query.to_string());
    let search = json!({"kind":"visual", "value":"MOB-1001"});
    for bearer in [None, Some("service-token")] {
        let raw = raw_call(addr, &request(bearer, search.clone())).await;
        assert!(raw.starts_with("HTTP/1.1 401"));
        assert!(
            raw.to_ascii_lowercase()
                .contains("cache-control: private, no-store")
        );
    }
    let raw = raw_call(addr, &request(owner, search)).await;
    assert!(raw.starts_with("HTTP/1.1 200"));
    assert!(
        raw.to_ascii_lowercase()
            .contains("cache-control: private, no-store")
    );
    let (_, body) = raw.split_once("\r\n\r\n").unwrap();
    for private in [
        "MOB-1001",
        "04A1B2C3D4E5F6",
        "DE-MSP-C00000001-W",
        "operator-token",
    ] {
        assert!(!body.contains(private));
    }
    let page: Value = serde_json::from_str(body).unwrap();
    let card = &page["items"][0];
    assert_eq!(card["blocked"], false);
    assert_eq!(card["token_type"], "Rfid");
    assert_eq!(card["reference"].as_str().unwrap().len(), 64);
    for (kind, key) in [("reference", "reference"), ("visual", "visual")] {
        let repeated = request(owner, json!({"kind":kind,"value":card[key]}));
        assert_eq!(call(addr, &repeated).await, (200, body.to_owned()));
    }
    let zero = request(owner, json!({"kind":"visual","value":"04A1B2C3D4E5F6"}));
    assert_eq!(
        call(addr, &zero).await,
        (200, r#"{"items":[],"next":null}"#.into())
    );
    for invalid in [
        json!({"kind":"reference","value":"04A1B2C3D4E5F6"}),
        json!({"kind":"emaid","value":"this is private"}),
        json!({"kind":"visual","value":"A".repeat(65)}),
        json!({"kind":"visual","value":"bad\nlabel"}),
        json!({"kind":"MOB-1001","value":"unknown"}),
    ] {
        let raw = raw_call(addr, &request(owner, invalid)).await;
        assert!(raw.starts_with("HTTP/1.1 400"));
        assert!(raw.ends_with("\r\n\r\n"));
        assert!(
            raw.to_ascii_lowercase()
                .contains("cache-control: private, no-store")
        );
    }
    let oversized = request(owner, json!({"kind":"visual", "value":"X".repeat(2048)}));
    assert_eq!(call(addr, &oversized).await.0, 413);
    assert_eq!(call(addr, &get(addr, path, owner)).await.0, 405);
    provider.kill().await;
}

async fn card_action(
    addr: std::net::SocketAddr,
    route: &str,
    bearer: Option<&str>,
    input: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let path = format!("/v1/empd/cards/{route}");
    let raw = raw_call(addr, &post_to(addr, &path, bearer, &input.to_string())).await;
    assert!(
        raw.to_ascii_lowercase()
            .contains("cache-control: private, no-store")
    );
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = headers[9..12].parse().unwrap();
    for secret in [
        "reader-token",
        "operator-token",
        "04DEADBEEF0001",
        "DE-MSP-C00000001-W",
    ] {
        assert!(!body.contains(secret));
    }
    (
        status,
        if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(body).unwrap()
        },
    )
}

// heute-acts/SR-030, heute-acts/SR-033, heute-acts/SR-034, heute-acts/SR-036. The real writer gate, signed journal,
// replay, read-only history and next online station decision share the same state.
#[tokio::test]
async fn a_writer_reverses_a_seed_block_once_and_the_decision_survives_restart() {
    use serde_json::json;
    let path = journal_path("card-change");
    let reference = book().state().reference_for("04DEADBEEF0001");
    let writer = Some("operator-token");
    let reader = Some("reader-token");
    let service = Some("service-token");
    let request = json!({"reference":reference, "blocked":false, "reason":"recovered"});
    let lookup = json!({"kind":"reference", "value":reference});
    let running = start_at(&path).await;
    let addr = running.http;
    for (bearer, status) in [(None, 401), (service, 401), (reader, 403)] {
        for route in ["preview", "confirm"] {
            assert_eq!(card_action(addr, route, bearer, &request).await.0, status);
        }
    }
    for invalid in [
        json!({"reference":reference, "blocked":false, "reason":"stolen"}),
        json!({"reference":reference, "blocked":false, "reason":"free text"}),
        json!({"reference":reference, "blocked":false, "reason":"cleared", "actor":"operator:forged"}),
        json!({"reference":"04DEADBEEF0001", "blocked":false, "reason":"cleared"}),
    ] {
        assert_eq!(card_action(addr, "preview", writer, &invalid).await.0, 400);
    }
    assert_eq!(
        card_action(
            addr,
            "preview",
            writer,
            &json!({"reference":"a".repeat(64), "blocked":true, "reason":"lost"})
        )
        .await
        .0,
        404
    );
    let (status, preview) = card_action(addr, "preview", writer, &request).await;
    assert_eq!(status, 200);
    assert_eq!(preview["reference"], lookup["value"]);
    assert_eq!(preview["effect"], "evaluate_contract");
    assert_eq!(preview["offline_cache"], "unchanged");
    assert_eq!(preview["operation"].as_str().unwrap().len(), 64);
    assert!(preview["expires_at"].is_string());
    let op = json!({"operation": preview["operation"], "reference":reference});
    let changed_intent =
        json!({"operation": preview["operation"], "blocked":true, "reason":"lost"});
    assert_eq!(
        card_action(addr, "confirm", writer, &changed_intent)
            .await
            .0,
        400
    );
    let oversized = json!({"operation":"a".repeat(2048)});
    assert_eq!(
        card_action(addr, "confirm", writer, &oversized).await.0,
        413
    );
    let (_, card) = card_action(addr, "lookup", reader, &lookup).await;
    assert_eq!(card["items"][0]["blocked"], true);
    assert_eq!(card["items"][0]["history"], json!([]));
    let (first, second) = tokio::join!(
        card_action(addr, "confirm", writer, &op),
        card_action(addr, "confirm", writer, &op)
    );
    let (applied, duplicate) = if first.0 == 200 {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!((applied.0, duplicate.0), (200, 409));
    assert_eq!(applied.1["reason"], "recovered");
    assert_eq!(applied.1["actor"], "operator:demo operator");
    assert_eq!(applied.1["operation"], op["operation"]);
    assert_eq!(applied.1["blocked"], false);
    assert!(applied.1["reviewed_at"].is_string() && applied.1["at"].is_string());
    let block = json!({"reference":reference, "blocked":true, "reason":"lost"});
    let (_, discarded_preview) = card_action(addr, "preview", writer, &block).await;
    running.kill().await;

    let restarted = start_at(&path).await;
    let addr = restarted.http;
    assert_eq!(card_action(addr, "confirm", writer, &op).await.0, 409);
    assert_eq!(
        card_action(
            addr,
            "confirm",
            writer,
            &json!({"operation":discarded_preview["operation"]})
        )
        .await
        .0,
        409
    );
    let (status, lookup_result) = card_action(addr, "lookup", reader, &lookup).await;
    assert_eq!(status, 200);
    let card = &lookup_result["items"][0];
    assert_eq!(card["history"], json!([applied.1]));
    assert_eq!(card["blocked"], false);
    let (_, broad) = card_action(
        addr,
        "lookup",
        reader,
        &json!({"kind":"visual", "value":card["visual"]}),
    )
    .await;
    assert_eq!(broad["items"][0]["history"], json!([]));
    let station = post(addr, service, r#"{"uid":"04DEADBEEF0001"}"#);
    assert!(
        call(addr, &station)
            .await
            .1
            .contains(r#""status":"Accepted""#)
    );
    let (status, review) = card_action(addr, "preview", writer, &block).await;
    assert_eq!(status, 200);
    assert_eq!(review["effect"], "refuse_online");
    let (status, _) = card_action(
        addr,
        "confirm",
        writer,
        &json!({"operation":review["operation"]}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        call(addr, &station)
            .await
            .1
            .contains(r#""status":"Blocked""#)
    );
    restarted.kill().await;
    std::fs::remove_dir_all(fixture_root(&path)).unwrap();
}

async fn enrol_action(
    addr: std::net::SocketAddr,
    action: &str,
    bearer: Option<&str>,
    input: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let raw = raw_call(
        addr,
        &post_to(
            addr,
            &format!("/v1/empd/contracts/{action}"),
            bearer,
            &input.to_string(),
        ),
    )
    .await;
    assert!(
        raw.to_ascii_lowercase()
            .contains("cache-control: private, no-store")
    );
    assert!(!raw.contains("04AB0000000004") && !raw.contains("STOCK-1004"));
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
    (
        headers[9..12].parse().unwrap(),
        serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
    )
}

fn retail_fixture() -> emob_service::estate::RetailOffer {
    use emob_service::estate::{RetailOffer, RetailTerms};
    let fixture: estate::Estate = estate::from_str(DEMO).unwrap();
    let plan = &fixture.public_plans[0];
    let terms = RetailTerms {
        point: plan.points[0].clone(),
        provider: plan.provider.clone(),
        operator: fixture.parties.operator,
        code: plan.code.clone(),
        name: plan.name.clone(),
        base: fixture.tariffs[0].to_tariff(0).unwrap(),
        fees: plan.schedules[0].clone(),
        superseded: None,
    };
    RetailOffer {
        reference: terms.reference(),
        terms,
    }
}

// native-driver-enrolment/SR-301, native-driver-enrolment/SR-302,
// native-driver-enrolment/SR-303, native-driver-enrolment/SR-304,
// native-driver-enrolment/SR-305, native-driver-enrolment/SR-306.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn current_terms_and_one_physical_card_are_committed_once() {
    use axum::{Router, http::StatusCode, routing::get};
    use serde_json::json;

    // The estate can run a historical clock. The provider and the CSMS must
    // compare terms, record issuance and authorize using that same instant.
    let mut live = retail_fixture();
    let frozen = time::macros::datetime!(2020-02-01 12:00 UTC);
    let end = frozen + time::Duration::days(1);
    live.terms.fees.until = Some(end);
    live.reference = live.terms.reference();
    let now = Arc::new(std::sync::Mutex::new(frozen));
    let held = Arc::clone(&now);
    let clock: emob_service::Clock = Arc::new(move || *held.lock().unwrap());
    let published = Arc::new(std::sync::Mutex::new((
        StatusCode::OK,
        serde_json::to_string(&live).unwrap(),
    )));
    let source = published.clone();
    let routes = Router::new().route(
        "/v1/csmsd/retail-offers/{point}/{provider}/{plan}",
        get(move || {
            let source = source.clone();
            async move { source.lock().unwrap().clone() }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut estate = demo();
    estate.desk.as_mut().unwrap().csmsd = format!("http://{}", listener.local_addr().unwrap());
    let price_task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let path = journal_path("native-offer");
    let running = start_with_clock(&path, estate.clone(), Arc::clone(&clock)).await;
    let addr = running.http;
    let original = running
        .book
        .lock()
        .unwrap()
        .journal()
        .head()
        .unwrap()
        .position;
    let writer = Some("operator-token");
    let reader = Some("reader-token");
    let service = Some("service-token");
    let input = json!({"account":"PAYER-FLEET","stock":"STOCK-1004","point":live.terms.point,"plan":live.terms.code});
    let unknown = json!({"account":"PAYER-FLEET","stock":"GUESS","point":live.terms.point,"plan":live.terms.code});
    for (bearer, code) in [(reader, 403), (service, 401), (None, 401)] {
        assert_eq!(enrol_action(addr, "preview", bearer, &input).await.0, code);
        assert_eq!(
            enrol_action(addr, "confirm", bearer, &json!({"operation":"bad"}))
                .await
                .0,
            code
        );
    }
    assert_eq!(enrol_action(addr, "preview", writer, &unknown).await.0, 404);
    assert_eq!(enrol_action(addr, "preview", writer, &json!({"uid":"xxxx","stock":"STOCK-1004","point":live.terms.point,"plan":live.terms.code})).await.0, 400);
    assert_eq!(
        enrol_action(
            addr,
            "preview",
            writer,
            &json!({"stock":"z".repeat(3000),"point":live.terms.point,"plan":live.terms.code})
        )
        .await
        .0,
        413
    );
    let mut bad_payer = input.clone();
    bad_payer["account"] = json!("UNKNOWN-PAYER");
    assert_eq!(
        enrol_action(addr, "preview", writer, &bad_payer).await.0,
        404
    );
    bad_payer.as_object_mut().unwrap().remove("account");
    assert_eq!(
        enrol_action(addr, "preview", writer, &bad_payer).await.0,
        400
    );
    let (status, review) = enrol_action(addr, "preview", writer, &input).await;
    assert_eq!(status, 200);
    assert_eq!(review["offer"]["reference"], live.reference);
    let op = json!({"operation":review["operation"]});
    let query = json!({"agreement":review["operation"]});
    assert_eq!(enrol_action(addr, "lookup", reader, &query).await.0, 404);

    // Same selection, changed public fees, different valid digest.
    let mut changed = live.clone();
    changed.terms.fees.energy += rust_decimal::Decimal::ONE;
    changed.reference = changed.terms.reference();
    published.lock().unwrap().1 = serde_json::to_string(&changed).unwrap();
    assert_eq!(enrol_action(addr, "confirm", writer, &op).await.0, 409);
    published.lock().unwrap().0 = StatusCode::NOT_FOUND;
    assert_eq!(enrol_action(addr, "preview", writer, &input).await.0, 404);
    published.lock().unwrap().0 = StatusCode::OK;
    published.lock().unwrap().1 = "x".repeat(65_537);
    assert_eq!(enrol_action(addr, "confirm", writer, &op).await.0, 503);
    changed.reference.clone_from(&live.reference);
    published.lock().unwrap().1 = serde_json::to_string(&changed).unwrap();
    assert_eq!(enrol_action(addr, "confirm", writer, &op).await.0, 503);
    assert_eq!(
        running
            .book
            .lock()
            .unwrap()
            .journal()
            .head()
            .unwrap()
            .position,
        original
    );
    published.lock().unwrap().1 = serde_json::to_string(&live).unwrap();

    let (first, second) = tokio::join!(
        enrol_action(addr, "confirm", writer, &op),
        enrol_action(addr, "confirm", writer, &op)
    );
    assert_eq!(first.0, 200);
    assert_eq!(first, second);
    let receipt = first.1;
    assert_eq!(receipt["actor"], "operator:demo operator");
    assert_eq!(receipt["agreement"], review["operation"]);
    assert_eq!(receipt["offer"], review["offer"]);
    assert!(
        emob_core::Emaid::parse(receipt["emaid"].as_str().unwrap())
            .unwrap()
            .carried_check_digit()
    );
    assert_eq!(
        enrol_action(addr, "lookup", reader, &query).await.1,
        receipt
    );
    assert_eq!(enrol_action(addr, "preview", writer, &input).await.0, 409);
    assert_eq!(
        running
            .book
            .lock()
            .unwrap()
            .journal()
            .head()
            .unwrap()
            .position,
        original + 1
    );
    running.kill().await;
    price_task.abort();
    let restored = start_with_clock(&path, estate.clone(), Arc::clone(&clock)).await;
    let addr = restored.http;
    // Completed confirmations and lookup work even when the price service is offline.
    assert_eq!(enrol_action(addr, "confirm", writer, &op).await.1, receipt);
    assert_eq!(
        enrol_action(addr, "lookup", reader, &query).await.1,
        receipt
    );
    assert_eq!(
        restored
            .book
            .lock()
            .unwrap()
            .journal()
            .head()
            .unwrap()
            .position,
        original + 1
    );
    // retail billing roster: the price and payer survive replay, without card or operator details.
    let fleet = json!({"account":"PAYER-FLEET"});
    assert_eq!(
        enrol_action(addr, "billing-agreements", service, &fleet)
            .await
            .0,
        401
    );
    let (status, roster) = enrol_action(addr, "billing-agreements", reader, &fleet).await;
    assert_eq!(status, 200);
    assert_eq!(roster["account"]["id"], receipt["account"]);
    assert_eq!(roster["agreements"][0]["account"], receipt["account"]);
    assert_eq!(roster["agreements"][0]["offer"], receipt["offer"]);
    assert!(roster["agreements"][0].get("card").is_none());
    assert!(roster["agreements"][0].get("actor").is_none());
    let (_, unrelated) = enrol_action(
        addr,
        "billing-agreements",
        reader,
        &json!({"account":"PAYER-PRIVATE"}),
    )
    .await;
    assert_eq!(unrelated["agreements"], json!([]));
    for (point, expected) in [
        (None, "Invalid"),
        (Some("DE*ABC*E00002"), "Invalid"),
        (Some("DE*ABC*E00001"), "Accepted"),
    ] {
        let req = json!({"uid":"04AB0000000004","evse":point});
        let (s, raw) = call(addr, &post(addr, service, &req.to_string())).await;
        assert_eq!(s, 200);
        let answer: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(answer["status"], expected);
        if point == Some("DE*ABC*E00001") {
            assert_eq!(answer["token"]["contract_id"], receipt["emaid"]);
            assert_eq!(answer["retail"]["agreement"], receipt["agreement"]);
            assert_eq!(answer["retail"]["reference"], live.reference);
        } else {
            assert!(answer["token"].is_null() && answer["retail"].is_null());
        }
    }
    *now.lock().unwrap() = end;
    let req = json!({"uid":"04AB0000000004","evse":live.terms.point});
    let (_, expired) = call(addr, &post(addr, service, &req.to_string())).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&expired).unwrap()["status"],
        "Invalid"
    );
    *now.lock().unwrap() = frozen;
    let replaced = replace_issued_card(addr, &receipt, &now).await;
    *now.lock().unwrap() += time::Duration::hours(1);
    let stop = json!({"agreement":receipt["agreement"],"action":"end"});
    let (status, review) = enrol_action(addr, "change/preview", writer, &stop).await;
    assert_eq!(status, 200);
    let end_op = json!({"agreement":receipt["agreement"],"operation":review["operation"]});
    let (status, ended) = enrol_action(addr, "change/confirm", writer, &end_op).await;
    assert_eq!(status, 200);
    assert_eq!(ended["action"], "end");
    restored.kill().await;
    let restored = start_with_clock(&path, estate, clock).await;
    let addr = restored.http;
    let replace = json!({"agreement":receipt["agreement"],"action":"replace","stock":"STOCK-1006"});
    let replace_op = json!({"agreement":receipt["agreement"],"operation":replaced["operation"]});
    assert_eq!(
        enrol_action(addr, "change/preview", writer, &replace)
            .await
            .0,
        409
    );
    assert_eq!(
        enrol_action(addr, "change/confirm", writer, &end_op)
            .await
            .1,
        ended
    );
    assert_eq!(
        enrol_action(addr, "change/confirm", writer, &replace_op)
            .await
            .1,
        replaced
    );
    assert_eq!(
        enrol_action(addr, "lookup", reader, &query).await.1["lifecycle"],
        ended
    );
    assert_eq!(
        enrol_action(addr, "confirm", writer, &op).await.1["lifecycle"],
        ended
    );
    let after = enrol_action(addr, "billing-agreements", reader, &fleet)
        .await
        .1;
    assert_eq!(after["agreements"][0]["ended_at"], ended["at"]);
    assert_eq!(after["agreements"][0]["offer"], receipt["offer"]);
    for uid in ["04AB0000000004", "04AB0000000006"] {
        let answer = authorize_card(addr, uid).await;
        assert_eq!(answer["status"], "Blocked");
        assert!(answer["token"].is_null() && answer["retail"].is_null());
    }
    assert_eq!(
        restored
            .book
            .lock()
            .unwrap()
            .journal()
            .head()
            .unwrap()
            .position,
        original + 3
    );
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(!file.contains("04AB0000000004") && !file.contains("STOCK-1004"));
    assert!(!file.contains("04AB0000000006") && !file.contains("STOCK-1006"));
    assert!(!file.contains("Musterflotte") && !file.contains("Beispielweg"));
    restored.kill().await;
}

async fn replace_issued_card(
    addr: std::net::SocketAddr,
    receipt: &serde_json::Value,
    now: &std::sync::Mutex<time::OffsetDateTime>,
) -> serde_json::Value {
    use serde_json::json;

    let writer = Some("operator-token");
    let reader = Some("reader-token");
    let service = Some("service-token");
    let query = json!({"agreement":receipt["agreement"]});
    let replace = json!({"agreement":receipt["agreement"],"action":"replace","stock":"STOCK-1006"});
    for (bearer, status) in [(None, 401), (reader, 403), (service, 401)] {
        assert_eq!(
            enrol_action(addr, "change/preview", bearer, &replace)
                .await
                .0,
            status
        );
        assert_eq!(
            enrol_action(addr, "change/confirm", bearer, &query).await.0,
            status
        );
    }
    let (status, review) = enrol_action(addr, "change/preview", writer, &replace).await;
    assert_eq!(status, 200);
    assert_eq!(review["current_card"], receipt["card"]);
    let replace_op = json!({"agreement":receipt["agreement"],"operation":review["operation"]});
    let wrong = json!({"agreement":"0".repeat(64),"operation":review["operation"]});
    assert_eq!(
        enrol_action(addr, "change/confirm", writer, &wrong).await.0,
        409
    );
    *now.lock().unwrap() += time::Duration::minutes(5);
    assert_eq!(
        enrol_action(addr, "change/confirm", writer, &replace_op)
            .await
            .0,
        409
    );
    let (status, review) = enrol_action(addr, "change/preview", writer, &replace).await;
    assert_eq!(status, 200);
    let replace_op = json!({"agreement":receipt["agreement"],"operation":review["operation"]});
    let (first, second) = tokio::join!(
        enrol_action(addr, "change/confirm", writer, &replace_op),
        enrol_action(addr, "change/confirm", writer, &replace_op)
    );
    assert_eq!(first.0, 200);
    assert_eq!(first, second);
    assert_eq!(first.1["current_card"], review["replacement_card"]);
    assert_eq!(first.1["account"], receipt["account"]);
    let lookup = enrol_action(addr, "lookup", reader, &query).await.1;
    assert_eq!(lookup["lifecycle"], first.1);
    assert_eq!(lookup["offer"], receipt["offer"]);
    assert_eq!(lookup["emaid"], receipt["emaid"]);
    let unblock = json!({"reference":receipt["reference"],"blocked":false,"reason":"recovered"});
    assert_eq!(card_action(addr, "preview", writer, &unblock).await.0, 409);
    for (uid, status) in [
        ("04AB0000000004", "Blocked"),
        ("04AB0000000006", "Accepted"),
    ] {
        let answer = authorize_card(addr, uid).await;
        assert_eq!(answer["status"], status);
        if status == "Accepted" {
            assert_eq!(answer["retail"]["agreement"], receipt["agreement"]);
            assert_eq!(answer["token"]["contract_id"], receipt["emaid"]);
        }
    }
    first.1
}

async fn authorize_card(addr: std::net::SocketAddr, uid: &str) -> serde_json::Value {
    let req = serde_json::json!({"uid":uid,"evse":"DE*ABC*E00001"});
    let (status, raw) = call(addr, &post(addr, Some("service-token"), &req.to_string())).await;
    assert_eq!(status, 200);
    serde_json::from_str(&raw).unwrap()
}

#[path = "customer/access.rs"]
mod customer_access;

#[path = "withdrawal/case.rs"]
mod withdrawal_case;

#[path = "customer/erasure.rs"]
mod customer_erasure;
