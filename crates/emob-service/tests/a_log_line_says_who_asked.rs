//! SR-102: a request's log lines carry its id and its actor, as JSON fields,
//! and never the raw path, the token or a forged id.
//!
//! Its own test binary, because a process has one global subscriber.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use emob_service::authority::{Operators, Token};
use emob_service::telemetry::Captured;
use emob_service::{Actor, Identity, Readiness, Server, Shutdown};
use tower_service::Service as _;

#[tokio::test]
async fn sr_102_a_request_is_logged_with_its_id_and_actor_and_without_its_secrets() {
    let captured = Captured::start("debug").expect("the only subscriber in this binary");
    let operators = Operators::new(vec![(
        Actor::operator("leitstelle").unwrap(),
        Token::new("operator-token-secret"),
    )]);
    let gated = emob_service::serve::operator_only(
        Router::new().route(
            "/v1/testd/today",
            get(
                |axum::Extension(actor): axum::Extension<Actor>| async move {
                    tracing::info!("read by the operator");
                    actor.to_string()
                },
            ),
        ),
        operators,
    );
    let open = Router::new().route("/r/{token}", get(|| async { "receipt" }));
    let mut router = Server::new(
        Identity::new("testd", "0"),
        Readiness::new(),
        Shutdown::new(),
    )
    .with(gated.merge(open))
    .router();

    let response = router
        .call(
            Request::builder()
                .uri("/v1/testd/today")
                .header("authorization", "Bearer operator-token-secret")
                .header("x-request-id", "req-42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["x-request-id"], "req-42");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(&body[..], b"operator:leitstelle");

    let response = router
        .call(
            Request::builder()
                .uri("/r/receipt-capability-secret?x=1")
                .header("x-request-id", "forged\" id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let minted = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        minted.len(),
        32,
        "a refused inbound id is replaced: {minted}"
    );

    let logs = captured.text();
    let lines: Vec<serde_json::Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is JSON"))
        .collect();
    let read = lines
        .iter()
        .find(|line| line["message"] == "read by the operator")
        .expect("the handler's line");
    assert_eq!(read["span"]["id"], "req-42");
    assert_eq!(read["span"]["actor"], "operator:leitstelle");
    assert_eq!(read["span"]["route"], "/v1/testd/today");
    assert!(
        lines
            .iter()
            .any(|line| line["span"]["route"] == "/r/{token}" && line["span"]["id"] == minted),
        "{logs}"
    );
    for secret in [
        "operator-token-secret",
        "receipt-capability-secret",
        "forged",
    ] {
        assert!(!logs.contains(secret), "{secret} reached the logs:\n{logs}");
    }
}
