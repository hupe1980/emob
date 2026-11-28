//! Real provider startup, scoped encryption and a restore from a cut taken before erasure.
use super::{
    customer_access::{api, fixture, signin},
    journal_path, start_with,
};
use emob_service::{
    estate::{EnvRef, Operator},
    head,
    journal::Record,
    vault::DataKey,
};
use serde_json::{Value, json};
use std::net::SocketAddr;

const DRIVER: &str = "driver-test-034";
const OTHER: &str = "other-test-034";
const PRIVACY: &str = "privacy-token";
const WRITER: &str = "operator-token";
const ACCOUNT: &str = "PAYER-PRIVATE";

fn estate() -> empd::daemon::Estate {
    let mut estate = fixture();
    for (name, variable) in [
        ("privacy reviewer", "PRIVACY_TEST"),
        ("second reviewer", "PRIVACY_SECOND"),
    ] {
        estate.operators.push(Operator {
            name: name.into(),
            token_env: EnvRef::new(variable),
            finance: false,
            write: false,
            privacy: true,
        });
    }
    estate
}

async fn change(addr: SocketAddr, subject: &str, revision: u64, granted: bool) -> u16 {
    api(addr,"grants",Some(WRITER),json!({"subject":subject,"kind":"account","reference":ACCOUNT,"revision":revision,"granted":granted})).await.0
}

async fn review(addr: SocketAddr, subject: &str) -> Value {
    let (status, result) = api(
        addr,
        "erasure/review",
        Some(PRIVACY),
        json!({"subject":subject}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(result["state"], "ready");
    assert_eq!(result["policy"], "portal_access_v1");
    result
}

fn request(review: &Value) -> Value {
    json!({"subject":review["subject"],"operation":review["operation"]})
}

fn assert_envelopes_under_subject_keys(cut: &str) {
    let master = DataKey::from_bytes(&[1; 32]);
    let mut seen = 0;
    for line in cut.lines() {
        let record: Record = serde_json::from_str(line).unwrap();
        if record.kind == "customer_grant" {
            let event: empd::Event = serde_json::from_str(&record.body).unwrap();
            let empd::Event::CustomerGrant { change, .. } = event else {
                panic!("expected customer grant")
            };
            assert!(master.open("empd/customer-grant/v2", &change).is_err());
            seen += 1;
        }
    }
    assert_eq!(seen, 2);
    for private in [DRIVER, OTHER, ACCOUNT] {
        assert!(!cut.contains(private));
    }
}

async fn reject_rolled_back_custody(path: &std::path::Path, obsolete: &[u8]) {
    let registry = super::fixture_root(path)
        .join("custody")
        .join("subjects.json");
    let current = std::fs::read(&registry).unwrap();
    std::fs::write(&registry, obsolete).unwrap();
    let result = empd::daemon::run(empd::daemon::Options {
        http: "127.0.0.1:0".parse().unwrap(),
        journal: path.to_owned(),
        custody: super::fixture_root(path).join("custody"),
        estate: estate(),
        env: super::env(),
        shutdown: emob_service::Shutdown::new(),
        identity: emob_service::Identity::new("empd", "test"),
    })
    .await;
    let Err(error) = result else {
        panic!("obsolete custody reopened a witnessed erasure")
    };
    assert!(format!("{error:#}").contains("opening customer grants"));
    std::fs::write(registry, current).unwrap();
}

#[tokio::test]
async fn erasure_survives_a_pre_erasure_journal_and_preserves_the_other_customer() {
    let path = journal_path("customer-erasure");
    let signed_head = head::beside(&path);
    let first = start_with(&path, estate()).await;
    let addr = first.http;
    assert_eq!(change(addr, DRIVER, 0, true).await, 200);
    assert_eq!(change(addr, OTHER, 0, true).await, 200);
    let cut = std::fs::read_to_string(&path).unwrap();
    let stale_custody = std::fs::read(
        super::fixture_root(&path)
            .join("custody")
            .join("subjects.json"),
    )
    .unwrap();
    let head = std::fs::read(&signed_head).unwrap();
    assert_envelopes_under_subject_keys(&cut);
    let session = signin(addr, DRIVER).await;
    let unaffected = signin(addr, OTHER).await;
    let query = json!({"subject":DRIVER});
    let (_, active) = api(addr, "erasure/status", Some(PRIVACY), query.clone()).await;
    assert_eq!(active["state"], "active");
    assert_eq!(active["accounts"], 1);
    let selection = request(&review(addr, DRIVER).await);
    let (left, right) = tokio::join!(
        api(addr, "erasure/confirm", Some(PRIVACY), selection.clone()),
        api(addr, "erasure/confirm", Some(PRIVACY), selection.clone())
    );
    assert_eq!(left, right);
    assert_eq!(left.0, 200);
    let receipt = left.1;
    assert_eq!(receipt["state"], "erased");
    assert_eq!(receipt["policy"], "portal_access_v1");
    assert_eq!(receipt["actor"], "operator:privacy reviewer");
    assert_eq!(receipt["accounts"], 1);
    assert_eq!(api(addr, "me", Some(&session), json!({})).await.0, 401);
    assert_eq!(
        api(addr, "me", Some(&unaffected), json!({})).await.1["accounts"],
        json!([ACCOUNT])
    );
    assert_eq!(change(addr, DRIVER, 0, true).await, 404);
    // Advance the unaffected grant journal after the cut to exercise a real rollback.
    assert_eq!(change(addr, OTHER, 1, false).await, 200);
    assert_ne!(std::fs::read_to_string(&path).unwrap(), cut);
    first.kill().await;
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|row| row.contains(r#""kind":"customer_erased""#))
            .count(),
        1
    );
    reject_rolled_back_custody(&path, &stale_custody).await;
    std::fs::write(&path, cut).unwrap();
    std::fs::write(&signed_head, head).unwrap();
    let restored = start_with(&path, estate()).await;
    let addr = restored.http;
    assert_eq!(
        api(addr, "erasure/status", Some(PRIVACY), query.clone()).await,
        (200, receipt.clone())
    );
    assert_eq!(
        api(addr, "erasure/confirm", Some(PRIVACY), selection).await,
        (200, receipt)
    );
    assert_eq!(api(addr, "access", Some(WRITER), query).await.0, 404);
    assert_eq!(
        api(
            addr,
            "login",
            None,
            json!({"subject":DRIVER,"secret":"9a".repeat(32)})
        )
        .await
        .0,
        401
    );
    assert_eq!(change(addr, DRIVER, 1, true).await, 404);
    let other = signin(addr, OTHER).await;
    assert_eq!(
        api(addr, "me", Some(&other), json!({})).await.1["accounts"],
        json!([ACCOUNT])
    );
    restored.kill().await;
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}

#[tokio::test]
async fn a_review_is_private_and_bound_to_one_actor_subject_and_grant_revision() {
    let path = journal_path("erasure-review");
    let provider = start_with(&path, estate()).await;
    let addr = provider.http;
    let query = json!({"subject":DRIVER});
    for route in ["erasure/status", "erasure/review", "erasure/confirm"] {
        assert_eq!(api(addr, route, Some(WRITER), query.clone()).await.0, 403);
        assert_eq!(api(addr, route, None, query.clone()).await.0, 401);
    }
    assert_eq!(
        api(
            addr,
            "erasure/review",
            Some(PRIVACY),
            json!({"subject":"unknown"})
        )
        .await
        .0,
        404
    );
    let first = request(&review(addr, DRIVER).await);
    let cross = json!({"subject":OTHER,"operation":first["operation"]});
    assert_eq!(
        api(addr, "erasure/confirm", Some(PRIVACY), cross).await.0,
        409
    );
    assert_eq!(
        api(
            addr,
            "erasure/confirm",
            Some("second-privacy"),
            first.clone()
        )
        .await
        .0,
        409
    );
    assert_eq!(change(addr, DRIVER, 0, true).await, 200);
    assert_eq!(
        api(addr, "erasure/confirm", Some(PRIVACY), first).await.0,
        409
    );
    let second = request(&review(addr, OTHER).await);
    let (code, receipt) = api(addr, "erasure/confirm", Some(PRIVACY), second.clone()).await;
    assert_eq!(code, 200);
    assert_eq!(receipt["accounts"], 0);
    assert_eq!(
        api(addr, "erasure/confirm", Some("second-privacy"), second)
            .await
            .0,
        409
    );
    let token = signin(addr, DRIVER).await;
    assert_eq!(
        api(addr, "me", Some(&token), json!({})).await.1["accounts"],
        json!([ACCOUNT])
    );
    provider.kill().await;
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}
