//! HTTP transport for structured `CloudEvents`. Each domain records the result in its journal.

use axum::http::StatusCode;
use std::time::Duration;

use super::{Delivery, ID_HEADER, SIGNATURE_HEADER, Secret, TIMESTAMP_HEADER, sign};
use crate::cloudevent::CONTENT_TYPE;

const MAX_RECEIPT_BYTES: usize = 8192;

/// Sending credentials and signed event bodies is restricted to the configured destination.
pub struct Client(reqwest::Client);

/// The configured target could not be reached, or the event could not be signed.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The transport failed before a status was received.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// The body could not be signed.
    #[error(transparent)]
    Signature(#[from] standardwebhooks::WebhookError),
}

impl Client {
    /// Build a reusable client with a time budget per attempt.
    ///
    /// # Errors
    ///
    /// Returns the HTTP client's initialization error.
    pub fn new(timeout: Duration) -> Result<Self, reqwest::Error> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map(Self)
    }

    /// Sign and send one event. A response body carries only diagnostic text or an optional
    /// receipt. The HTTP status decides whether the event reached its destination.
    ///
    /// # Errors
    ///
    /// Returns a signing or transport error when no status was received.
    pub async fn post(
        &self,
        url: &str,
        secret: &Secret,
        id: &str,
        body: String,
        at: time::OffsetDateTime,
    ) -> Result<Reply, SendError> {
        let delivery = Delivery::new(id, at, body.as_bytes());
        let signature = sign(&delivery, secret)?;
        let mut response = self
            .0
            .post(url)
            .header(ID_HEADER, id)
            .header(TIMESTAMP_HEADER, delivery.timestamp.to_string())
            .header(SIGNATURE_HEADER, signature)
            .header("content-type", CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            let remaining = MAX_RECEIPT_BYTES - bytes.len();
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                bytes.extend_from_slice("…".as_bytes());
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

/// A receiver's HTTP status and bounded answer.
pub struct Reply {
    /// The status sent by the configured receiver.
    pub status: StatusCode,
    /// Diagnostic text or the operator system's optional receipt.
    pub text: String,
}

/// How the outbox treats an HTTP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The receiver acknowledges custody.
    Accepted,
    /// The receiver has refused this delivery permanently.
    Refused,
    /// Keep the item pending. This includes credential rotation and rate limits.
    Retry,
}

impl Reply {
    /// Classify the HTTP status independently of the response body's contents.
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        match self.status.as_u16() {
            200..=299 => Disposition::Accepted,
            401 | 408 | 429 => Disposition::Retry,
            400..=499 => Disposition::Refused,
            _ => Disposition::Retry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::header::LOCATION, routing::post};

    #[test]
    fn each_status_has_one_delivery_outcome() {
        for (code, expected) in [
            (200, Disposition::Accepted),
            (204, Disposition::Accepted),
            (307, Disposition::Retry),
            (401, Disposition::Retry),
            (408, Disposition::Retry),
            (429, Disposition::Retry),
            (503, Disposition::Retry),
            (400, Disposition::Refused),
            (403, Disposition::Refused),
            (410, Disposition::Refused),
        ] {
            let reply = Reply {
                status: StatusCode::from_u16(code).unwrap(),
                text: String::new(),
            };
            assert_eq!(reply.disposition(), expected, "{code}");
        }
    }

    #[tokio::test]
    async fn signed_delivery_stays_at_its_configured_target_and_bounds_diagnostics() {
        let routes = Router::new()
            .route(
                "/redirect",
                post(|| async { (StatusCode::TEMPORARY_REDIRECT, [(LOCATION, "/receiver")]) }),
            )
            .route(
                "/receiver",
                post(|| async { (StatusCode::BAD_REQUEST, "r".repeat(64 * 1024)) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let client = Client::new(Duration::from_secs(5)).unwrap();
        let key = Secret::raw(b"local fixture").unwrap();
        let send = |path| {
            let target = format!("http://{addr}/{path}");
            let client = &client;
            let key = &key;
            async move {
                client
                    .post(
                        &target,
                        key,
                        "example",
                        "{}".to_owned(),
                        time::OffsetDateTime::UNIX_EPOCH,
                    )
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            send("redirect").await.status,
            StatusCode::TEMPORARY_REDIRECT
        );
        let reply = send("receiver").await;
        assert_eq!(reply.disposition(), Disposition::Refused);
        assert_eq!(reply.text, format!("{}…", "r".repeat(MAX_RECEIPT_BYTES)));
        server.abort();
    }
}
