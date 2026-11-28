use std::time::Duration;

use axum::body::Body;
use axum::http::{StatusCode, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{Estate, origin};

pub(crate) struct Owners {
    client: Client,
    empd: String,
    billd: String,
    pub(crate) accounting: Option<String>,
    pub(crate) origin: String,
    receipt_origin: Option<String>,
    pub(crate) secure: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Login {
    pub subject: String,
    pub secret: String,
}

#[derive(Deserialize)]
struct Session {
    session: String,
}

#[derive(Deserialize)]
pub(crate) struct Customer {
    pub subject: String,
    pub agreements: Vec<Agreement>,
    pub accounts: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct Agreement {
    #[serde(rename = "agreement")]
    pub id: String,
    pub emaid: String,
    pub point: String,
    pub plan: String,
    pub reference: String,
    pub ended_at: Option<String>,
    pub withdrawal_status: Option<WithdrawalStatus>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WithdrawalStatus {
    Received,
    Held,
    Approved,
    Declined,
}

#[derive(Deserialize)]
pub(crate) struct History {
    pub invoices: Vec<Invoice>,
    pub receipts: Vec<Receipt>,
    pub next_page: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct Invoice {
    pub account: String,
    pub period: String,
    pub number: String,
    pub payable: String,
    pub currency: String,
    pub kind: DocumentKind,
    pub supersedes: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DocumentKind {
    Invoice,
    Credit,
    Replacement,
    #[serde(rename = "line_credit")]
    LineCredit,
}

#[derive(Deserialize)]
pub(crate) struct Balance {
    pub number: String,
    pub currency: emob_core::Currency,
    pub amount_minor: u64,
    pub credited_minor: u64,
    pub paid_minor: u64,
    pub incoming_minor: u64,
    pub outgoing_minor: u64,
    pub due_minor: i64,
}

#[derive(Deserialize)]
pub(crate) struct Receipt {
    #[serde(with = "time::serde::rfc3339")]
    pub ended_at: time::OffsetDateTime,
    pub kwh: String,
    pub point: String,
    pub agreement: String,
    pub source: String,
    #[serde(rename = "receipt")]
    pub link: String,
}

pub(crate) fn valid_session(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn send(request: RequestBuilder) -> Result<reqwest::Response, StatusCode> {
    let response = request
        .send()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    match response.status() {
        StatusCode::OK | StatusCode::NO_CONTENT => Ok(response),
        status @ (StatusCode::UNAUTHORIZED
        | StatusCode::CONFLICT
        | StatusCode::NOT_FOUND
        | StatusCode::TOO_MANY_REQUESTS) => Err(status),
        _ => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

async fn json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, StatusCode> {
    const LIMIT: usize = 1024 * 1024;
    let mut response = send(request.timeout(Duration::from_secs(5))).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    {
        if chunk.len() > LIMIT - bytes.len() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

impl Owners {
    pub(crate) fn new(estate: &Estate) -> anyhow::Result<Self> {
        let origin = origin(&estate.portal.public_url)?
            .origin()
            .ascii_serialization();
        let secure = origin.starts_with("https://");
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .read_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
            empd: estate.services.empd.trim_end_matches('/').to_owned(),
            accounting: estate
                .services
                .accountingd
                .as_deref()
                .map(|s| s.trim_end_matches('/').to_owned()),
            billd: estate.services.billd.trim_end_matches('/').to_owned(),
            origin,
            receipt_origin: estate
                .receipts
                .as_ref()
                .map(|r| r.public_url.trim_end_matches('/').to_owned()),
            secure,
        })
    }

    pub(crate) async fn login(&self, input: &Login) -> Result<String, StatusCode> {
        let issued: Session = json(
            self.client
                .post(format!("{}/v1/empd/customer/login", self.empd))
                .json(input),
        )
        .await?;
        if !valid_session(&issued.session) {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(issued.session)
    }

    pub(crate) async fn logout(&self, token: &str) -> Result<(), StatusCode> {
        send(
            self.client
                .post(format!("{}/v1/empd/customer/logout", self.empd))
                .bearer_auth(token)
                .json(&serde_json::json!({})),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn customer(&self, token: &str) -> Result<Customer, StatusCode> {
        json(
            self.client
                .post(format!("{}/v1/empd/customer/me", self.empd))
                .bearer_auth(token)
                .json(&serde_json::json!({})),
        )
        .await
    }

    pub(crate) async fn history(&self, token: &str, page: u32) -> Result<History, StatusCode> {
        json(
            self.client
                .post(format!("{}/v1/billd/customer/history", self.billd))
                .bearer_auth(token)
                .json(&serde_json::json!({ "page": page })),
        )
        .await
    }

    pub(crate) async fn balance(&self, token: &str, number: &str) -> Result<Balance, StatusCode> {
        let base = self
            .accounting
            .as_ref()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let result: Balance = json(
            self.client
                .post(format!("{base}/v1/accountingd/customer/balance"))
                .bearer_auth(token)
                .json(&serde_json::json!({"number":number})),
        )
        .await?;
        if result.number != number {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(result)
    }

    pub(crate) async fn document(&self, token: &str, number: &str) -> Result<Body, StatusCode> {
        let response = send(
            self.client
                .post(format!("{}/v1/billd/customer/document", self.billd))
                .bearer_auth(token)
                .json(&serde_json::json!({ "number": number })),
        )
        .await?;
        let is_xml = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value.split(';').next() == Some("application/xml"));
        if !is_xml {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(Body::new(reqwest::Body::from(response)))
    }

    pub(crate) fn terms(&self, reference: &str) -> Option<String> {
        let base = self.receipt_origin.as_deref()?;
        valid_session(reference).then(|| format!("{base}/p/offers/{reference}"))
    }

    pub(crate) fn receipt_allowed(&self, link: &str) -> bool {
        self.receipt_origin
            .as_deref()
            .and_then(|base| link.strip_prefix(base))
            .and_then(|path| path.strip_prefix("/r/"))
            .is_some_and(|token| {
                token.len() == 22
                    && URL_SAFE_NO_PAD
                        .decode(token)
                        .is_ok_and(|bytes| bytes.len() == 16)
            })
    }
}
