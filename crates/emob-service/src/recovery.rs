//! Recover one event addressed to a subscriber.
//!
//! The signed request names the
//! purpose, subscriber, event type and ID. An outbound event cannot authorize
//! a recovery. Each daemon looks up the historical recipient in its own journal.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::post;

use crate::{cloudevent, estate::Subscription, webhook};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    purpose: Purpose,
    subscriber: String,
    #[serde(rename = "type")]
    kind: String,
    id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Purpose {
    Recover,
}

/// Mount the subscriber read on its issuing daemon. The lookup must return
/// an event only when the original journal entry names that exact recipient.
pub fn router(
    subscriptions: Vec<Subscription>,
    kind: &'static str,
    lookup: impl Fn(&str, &str) -> Result<Option<String>, StatusCode> + Send + Sync + 'static,
) -> Router {
    let subscriptions = Arc::new(subscriptions);
    let lookup = Arc::new(lookup);
    Router::new().route(
        "/v1/subscriber-events/recover",
        post(move |headers: HeaderMap, body: Bytes| {
            let subscriptions = Arc::clone(&subscriptions);
            let lookup = Arc::clone(&lookup);
            async move {
                let Lookup {
                    purpose: Purpose::Recover,
                    subscriber,
                    kind: requested,
                    id,
                } = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
                if id.is_empty()
                    || subscriber.is_empty()
                    || !crate::events::ALL.contains(&requested.as_str())
                {
                    return Err(StatusCode::BAD_REQUEST);
                }
                let subscription = subscriptions
                    .iter()
                    .find(|s| s.name == subscriber)
                    .ok_or(StatusCode::UNAUTHORIZED)?;
                let now = time::OffsetDateTime::now_utc();
                webhook::receive(
                    &headers,
                    &body,
                    std::slice::from_ref(&subscription.secret),
                    now,
                    time::Duration::minutes(5),
                )
                .map_err(|_| StatusCode::UNAUTHORIZED)?;
                if requested != kind || !subscription.takes(kind) {
                    return Err(StatusCode::NOT_FOUND);
                }
                let event = lookup(&subscriber, &id)?.ok_or(StatusCode::NOT_FOUND)?;
                let at = time::OffsetDateTime::now_utc();
                let signature = webhook::sign(
                    &webhook::Delivery::new(&id, at, event.as_bytes()),
                    &subscription.secret,
                )
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                Ok::<Response, StatusCode>(
                    (
                        [
                            (header::CONTENT_TYPE.as_str(), cloudevent::CONTENT_TYPE),
                            (header::CACHE_CONTROL.as_str(), "private, no-store"),
                            (webhook::ID_HEADER, &id),
                            (webhook::TIMESTAMP_HEADER, &at.unix_timestamp().to_string()),
                            (webhook::SIGNATURE_HEADER, &signature),
                        ],
                        event,
                    )
                        .into_response(),
                )
            }
        })
        .layer(DefaultBodyLimit::max(4096)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower_service::Service as _;

    const KIND: &str = crate::events::cdr::ISSUED;

    fn key(name: &str) -> webhook::Secret {
        webhook::Secret::raw(name.as_bytes()).unwrap()
    }

    fn subscriber(name: &str) -> Subscription {
        Subscription {
            name: name.into(),
            url: "https://subscriber.example/events".into(),
            types: vec![KIND.into()],
            secret: key(name),
        }
    }

    fn request(body: String, name: &str, at: time::OffsetDateTime) -> Request<Body> {
        let nonce = "recovery-request";
        let signature = webhook::sign(
            &webhook::Delivery::new(nonce, at, body.as_bytes()),
            &key(name),
        )
        .unwrap();
        Request::post("/v1/subscriber-events/recover")
            .header(webhook::ID_HEADER, nonce)
            .header(webhook::TIMESTAMP_HEADER, at.unix_timestamp())
            .header(webhook::SIGNATURE_HEADER, signature)
            .body(Body::from(body))
            .unwrap()
    }

    // platform-interfaces/SR-030 and SR-031
    #[tokio::test]
    async fn recovery_requires_fresh_proof_current_type_and_original_recipient() {
        let mut revoked = subscriber("revoked");
        revoked.types.clear();
        let mut app = router(
            vec![subscriber("warehouse"), subscriber("later"), revoked],
            KIND,
            |subscriber, id| {
                Ok((subscriber == "warehouse" && id == "event-7").then(|| "original".to_owned()))
            },
        );
        let body = |name: &str| {
            serde_json::json!({"subscriber":name,"id":"event-7","type":KIND,"purpose":"recover"})
                .to_string()
        };
        let now = time::OffsetDateTime::now_utc();
        for (name, credential, at, status) in [
            ("warehouse", "wrong", now, StatusCode::UNAUTHORIZED),
            (
                "warehouse",
                "warehouse",
                now - time::Duration::minutes(10),
                StatusCode::UNAUTHORIZED,
            ),
            ("later", "later", now, StatusCode::NOT_FOUND),
            ("revoked", "revoked", now, StatusCode::NOT_FOUND),
            ("removed", "removed", now, StatusCode::UNAUTHORIZED),
        ] {
            assert_eq!(
                app.call(request(body(name), credential, at))
                    .await
                    .unwrap()
                    .status(),
                status
            );
        }
        let unknown = body("warehouse").replace("event-7", "event-8");
        assert_eq!(
            app.call(request(unknown, "warehouse", now))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.call(request("x".repeat(4097), "warehouse", now))
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            app.call(request(
                body("warehouse").replace("recover", "other"),
                "warehouse",
                now
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::BAD_REQUEST
        );
        for _ in 0..2 {
            let response = app
                .call(request(body("warehouse"), "warehouse", now))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "private, no-store"
            );
            let (parts, payload) = response.into_parts();
            let bytes = axum::body::to_bytes(payload, 1024).await.unwrap();
            let id = webhook::receive(
                &parts.headers,
                &bytes,
                &[key("warehouse")],
                now,
                time::Duration::minutes(5),
            )
            .unwrap();
            assert_eq!(id, "event-7");
            assert_eq!(bytes.as_ref(), b"original");
        }
    }
}
