//! 0.2: `csmsd` asks before a transaction opens, and `empd` answers.
//!
//! The contracts come from the estate, once; the answer is the book's; the
//! question is asked over a socket that takes the service token and nobody
//! else's, and the operator's view of the store never shows a uid.

use std::collections::BTreeMap;
use std::sync::Arc;

use emob_service::estate::{self, Env};
use emob_service::journal::Memory;
use emob_service::{Identity, Shutdown};
use empd::daemon::{Estate, Options, run};
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
    ]
    .into_iter()
    .collect();
    Arc::new(move |name| values.get(name).map(|value| (*value).to_owned()))
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

async fn started(name: &str) -> empd::daemon::Running {
    let dir = std::env::temp_dir().join(format!(
        "empd-asked-{name}-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    run(Options {
        http: "127.0.0.1:0".parse().unwrap(),
        journal: dir.join("empd.ndjson"),
        estate: demo(),
        env: env(),
        shutdown: Shutdown::new(),
        identity: Identity::new("empd", "test"),
    })
    .await
    .expect("empd starts on the demo estate")
}

/// A tiny HTTP/1.1 client over a raw socket, so this test carries no client
/// crate: one request, the status line and the body back.
async fn call(addr: std::net::SocketAddr, request: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response[9..12].parse().unwrap();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn post(addr: std::net::SocketAddr, token: Option<&str>, body: &str) -> String {
    let auth = token.map_or_else(String::new, |t| format!("authorization: Bearer {t}\r\n"));
    format!(
        "POST /v1/authorize HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\ncontent-type: application/json\r\n{auth}content-length: {}\r\n\r\n{body}",
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
    empd.kill();
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
    empd.kill();
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
