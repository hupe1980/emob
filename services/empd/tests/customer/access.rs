//! Customer sessions and sealed, revocable grants through the actual HTTP owner.
use super::{call, demo, get, journal_path, post_to, raw_call, start_with};
use emob_service::estate::{EnvRef, PortalUser};
use serde_json::{Value, json};
use std::net::SocketAddr;

const SUBJECT: &str = "driver-test-034";
const OTHER: &str = "other-test-034";
const WRITER: &str = "operator-token";
const READER: &str = "reader-token";
const ACCOUNT: &str = "PAYER-PRIVATE";

pub(super) async fn api(
    addr: SocketAddr,
    action: &str,
    token: Option<&str>,
    body: Value,
) -> (u16, Value) {
    let raw = raw_call(
        addr,
        &post_to(
            addr,
            &format!("/v1/empd/customer/{action}"),
            token,
            &body.to_string(),
        ),
    )
    .await;
    assert!(
        raw.to_ascii_lowercase()
            .contains("cache-control: private, no-store")
    );
    let (header, text) = raw.split_once("\r\n\r\n").unwrap();
    let status = header[9..12].parse().unwrap();
    if status != 200 {
        assert!(text.is_empty());
    }
    (status, serde_json::from_str(text).unwrap_or(Value::Null))
}

pub(super) async fn signin(addr: SocketAddr, subject: &str) -> String {
    let key = if subject == SUBJECT { "9a" } else { "b3" }.repeat(32);
    let (status, login) = api(addr, "login", None, json!({"subject":subject,"secret":key})).await;
    assert_eq!(status, 200);
    let token = login["session"].as_str().unwrap();
    assert_ne!(token, key);
    assert_eq!(token.len(), 64);
    token.into()
}

pub(super) fn fixture() -> empd::daemon::Estate {
    let mut estate = demo();
    estate.portal_users = vec![
        PortalUser {
            subject: SUBJECT.into(),
            secret_env: EnvRef::new("CUSTOMER_TEST_KEY"),
        },
        PortalUser {
            subject: OTHER.into(),
            secret_env: EnvRef::new("CUSTOMER_TEST_OTHER"),
        },
    ];
    estate
}

fn change(granted: bool, revision: u64) -> Value {
    json!({"subject":SUBJECT,"kind":"account","reference":ACCOUNT,"granted":granted,"revision":revision})
}

async fn scopes(addr: SocketAddr, session: &str, expected: Value) {
    let (status, me) = api(addr, "me", Some(session), json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(me["accounts"], expected);
    assert_eq!(me["agreements"], json!([]));
}

// customer-invoice-access/SR-342
#[tokio::test]
async fn a_revoked_grant_is_effective_in_the_same_session_and_after_owner_restart() {
    let path = journal_path("customer-access");
    let first = start_with(&path, fixture()).await;
    let addr = first.http;
    let token = signin(addr, SUBJECT).await;
    let other = signin(addr, OTHER).await;
    scopes(addr, &token, json!([])).await;
    let subject = json!({"subject":SUBJECT});
    for denied in [None, Some(READER), Some(token.as_str())] {
        assert!(matches!(
            api(addr, "access", denied, subject.clone()).await.0,
            401 | 403
        ));
        assert!(matches!(
            api(addr, "grants", denied, change(true, 0)).await.0,
            401 | 403
        ));
    }
    assert_eq!(
        api(addr, "access", Some(WRITER), subject.clone()).await.1["revision"],
        0
    );
    let (status, assigned) = api(addr, "grants", Some(WRITER), change(true, 0)).await;
    assert_eq!(status, 200);
    assert_eq!(assigned["revision"], 1);
    assert_eq!(
        api(addr, "grants", Some(WRITER), change(true, 0)).await.1,
        assigned
    );
    scopes(addr, &token, json!([ACCOUNT])).await;
    scopes(addr, &other, json!([])).await;
    assert_eq!(
        api(addr, "grants", Some(WRITER), change(false, 1)).await.1["revision"],
        2
    );
    assert_eq!(
        api(addr, "grants", Some(WRITER), change(true, 0)).await.0,
        409
    );
    scopes(addr, &token, json!([])).await;
    assert_eq!(api(addr, "logout", Some(&other), json!({})).await.0, 204);
    assert_eq!(api(addr, "me", Some(&other), json!({})).await.0, 401);
    assert_eq!(api(addr, "me", Some(WRITER), json!({})).await.0, 401);
    first.kill().await;
    let raw = std::fs::read_to_string(&path).unwrap();
    for secret in [&token, SUBJECT, ACCOUNT, &"9a".repeat(32)] {
        assert!(!raw.contains(secret));
    }
    assert!(raw.contains("customer_grant"));
    let restarted = start_with(&path, fixture()).await;
    assert_eq!(
        api(restarted.http, "me", Some(&token), json!({})).await.0,
        401
    );
    let (status, readback) = api(restarted.http, "access", Some(WRITER), subject).await;
    assert_eq!(status, 200);
    assert_eq!(readback["revision"], 2);
    let token = signin(restarted.http, SUBJECT).await;
    scopes(restarted.http, &token, json!([])).await;
    restarted.kill().await;
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}

// customer-invoice-access/SR-341
#[tokio::test]
async fn invalid_secrets_and_subject_overrides_return_no_customer_data() {
    let path = journal_path("customer-identity");
    let owner = start_with(&path, fixture()).await;
    for input in [
        json!({"subject":SUBJECT,"secret":WRITER}),
        json!({"subject":"absent","secret":"9a".repeat(32)}),
    ] {
        assert_eq!(api(owner.http, "login", None, input).await.0, 401);
    }
    assert_eq!(
        api(
            owner.http,
            "login",
            None,
            json!({"subject":SUBJECT,"secret":"9a".repeat(32),"accounts":[ACCOUNT]})
        )
        .await
        .0,
        400
    );
    let token = signin(owner.http, SUBJECT).await;
    assert_eq!(
        api(owner.http, "me", Some(&token), json!({"subject":OTHER}))
            .await
            .0,
        400
    );
    let (status, _) = call(
        owner.http,
        &get(owner.http, "/v1/empd/contracts/options", Some(&token)),
    )
    .await;
    assert_eq!(status, 401);
    owner.kill().await;
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}

// customer-invoice-access/SR-342
#[tokio::test]
async fn a_replayed_account_grant_requires_the_same_recipient_profile() {
    let path = journal_path("customer-profile");
    let initial = start_with(&path, fixture()).await;
    assert_eq!(
        api(initial.http, "grants", Some(WRITER), change(true, 0))
            .await
            .0,
        200
    );
    initial.kill().await;
    let restored = start_with(&path, fixture()).await;
    scopes(
        restored.http,
        &signin(restored.http, SUBJECT).await,
        json!([ACCOUNT]),
    )
    .await;
    restored.kill().await;
    let mut changed = fixture();
    changed
        .retail_accounts
        .iter_mut()
        .find(|a| a.id == ACCOUNT)
        .unwrap()
        .name
        .push_str(" renamed");
    let restored = start_with(&path, changed).await;
    scopes(
        restored.http,
        &signin(restored.http, SUBJECT).await,
        json!([]),
    )
    .await;
    restored.kill().await;
    let mut disabled = fixture();
    disabled.portal_users.retain(|u| u.subject != SUBJECT);
    let disabled = start_with(&path, disabled).await;
    let (_, access) = api(
        disabled.http,
        "access",
        Some(WRITER),
        json!({"subject":SUBJECT}),
    )
    .await;
    assert_eq!(access["revision"], 1);
    assert_eq!(
        api(disabled.http, "grants", Some(WRITER), change(true, 1))
            .await
            .0,
        404
    );
    assert_eq!(
        api(disabled.http, "grants", Some(WRITER), change(false, 1))
            .await
            .1["revision"],
        2
    );
    disabled.kill().await;
    let restored = start_with(&path, fixture()).await;
    scopes(
        restored.http,
        &signin(restored.http, SUBJECT).await,
        json!([]),
    )
    .await;
    restored.kill().await;
    std::fs::remove_dir_all(super::fixture_root(&path)).unwrap();
}
