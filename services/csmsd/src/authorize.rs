//! Asking `empd` whether a token may charge — and what silence means.
//!
//! `[OCPP 1.6 §4.2]` and `[OCPP 2.0.1 C01]` leave the answer to the CSMS. Until
//! this module the answer was `Accepted`, for every card, without asking
//! anybody: a blocked card charged, and a CDR left with nobody named to pay it.
//!
//! # Fail closed
//!
//! `empd` that does not answer in time, answers something other than `200`, or
//! cannot be reached is an `Invalid` — with the failure as the reason, in the
//! journal. A station's own local authorisation list stays the station's
//! decision; what the backend says when asked is never "probably fine"
//! (SR-032).

use core::future::Future;
use core::pin::Pin;
use std::time::Duration;

use emob_service::config::Secret;
use serde::{Deserialize, Serialize};

use crate::journal::Presented;

/// What `empd` answered, in the four words a station understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Charge.
    Accepted,
    /// The token is blocked.
    Blocked,
    /// The contract is not in force.
    Expired,
    /// Anything else, including no answer.
    Invalid,
}

/// The answer, its reason, and — when it is `Accepted` — whom the session
/// bills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The status.
    pub status: Status,
    /// Why, in words an operator can act on.
    pub reason: String,
    /// The token as the provider presents it, on `Accepted`.
    pub token: Option<Presented>,
    /// The provider's keyed digest of the uid — what a session may hold.
    pub reference: Option<String>,
}

impl Answer {
    /// A refusal for a reason this side found.
    #[must_use]
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            status: Status::Invalid,
            reason: reason.into(),
            token: None,
            reference: None,
        }
    }

    /// Whether the session may start.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        self.status == Status::Accepted && self.token.is_some()
    }

    /// The authorisation a session started under this answer carries.
    ///
    /// A backend `Authorize` — `OCPP_AUTH_TLS`, `CENTRAL` `[OCMF Tab. 14]` —
    /// graded as a roaming authorisation, whose ceiling is `CERTIFIED`.
    #[must_use]
    pub fn authorization(&self) -> emob_session::Authorization {
        let Some(token) = &self.token else {
            return emob_session::Authorization::ad_hoc();
        };
        emob_session::Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: token.contract_id.clone(),
                emaid: emob_core::Emaid::parse(token.contract_id.as_str()).ok(),
            },
            token_ref: self
                .reference
                .as_deref()
                .and_then(|reference| emob_session::TokenRef::new(reference).ok()),
            authorization_reference: None,
        }
    }
}

/// Who answers. A trait so a test can stand in for `empd` without a socket.
pub trait Authorize: Send + Sync {
    /// Ask about one uid.
    fn ask<'a>(&'a self, uid: &'a str) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>>;
}

/// `empd`, over HTTP.
pub struct Empd {
    client: reqwest::Client,
    url: String,
    token: Secret,
}

impl core::fmt::Debug for Empd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Empd")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Empd {
    /// `empd` at `base`, asked with `token`, waited for `timeout`.
    ///
    /// # Errors
    ///
    /// The client could not be built.
    pub fn new(base: &str, token: Secret, timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            url: format!("{}/v1/authorize", base.trim_end_matches('/')),
            token,
        })
    }

    async fn call(&self, uid: &str) -> Answer {
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({ "uid": uid }))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) if error.is_timeout() => {
                return Answer::invalid(format!(
                    "empd did not answer within the configured timeout, so the token is refused \
                     rather than waved through: {error}"
                ));
            }
            Err(error) => {
                return Answer::invalid(format!(
                    "empd could not be reached, so the token is refused rather than waved \
                     through: {error}"
                ));
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Answer::invalid(format!(
                "empd answered {status}, so the token is refused rather than waved through"
            ));
        }
        response.json::<Answer>().await.unwrap_or_else(|error| {
            Answer::invalid(format!("empd's answer could not be read: {error}"))
        })
    }
}

impl Authorize for Empd {
    fn ask<'a>(&'a self, uid: &'a str) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>> {
        Box::pin(self.call(uid))
    }
}

/// Map a status onto 1.6's vocabulary.
#[must_use]
pub const fn v1_6(status: Status) -> ocpp_kit::v1_6::AuthorizationStatus {
    use ocpp_kit::v1_6::AuthorizationStatus as S;
    match status {
        Status::Accepted => S::Accepted,
        Status::Blocked => S::Blocked,
        Status::Expired => S::Expired,
        Status::Invalid => S::Invalid,
    }
}

/// Map a status onto 2.0.1's vocabulary.
#[must_use]
pub const fn v2_0_1(status: Status) -> ocpp_kit::v2_0_1::AuthorizationStatus {
    use ocpp_kit::v2_0_1::AuthorizationStatus as S;
    match status {
        Status::Accepted => S::Accepted,
        Status::Blocked => S::Blocked,
        Status::Expired => S::Expired,
        Status::Invalid => S::Invalid,
    }
}

/// Map a status onto 2.1's vocabulary.
#[must_use]
pub const fn v2_1(status: Status) -> ocpp_kit::v2_1::AuthorizationStatus {
    use ocpp_kit::v2_1::AuthorizationStatus as S;
    match status {
        Status::Accepted => S::Accepted,
        Status::Blocked => S::Blocked,
        Status::Expired => S::Expired,
        Status::Invalid => S::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sr_032_an_empd_nobody_can_reach_is_an_invalid_with_the_reason() {
        // A port nothing listens on: bind, read the address, drop the socket.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let empd = Empd::new(
            &format!("http://{addr}"),
            Secret::new("t"),
            Duration::from_millis(500),
        )
        .unwrap();
        let answer = empd.ask("04A1B2C3D4E5F6").await;
        assert_eq!(answer.status, Status::Invalid);
        assert!(
            answer.reason.contains("refused rather than waved through"),
            "{answer:?}"
        );
        assert!(!answer.is_accepted());
    }

    #[test]
    fn an_accepted_answer_without_a_token_does_not_open_a_session() {
        // Nobody to bill is not a session that may start.
        let answer = Answer {
            status: Status::Accepted,
            reason: String::new(),
            token: None,
            reference: None,
        };
        assert!(!answer.is_accepted());
    }
}
