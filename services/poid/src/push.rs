//! The table, delivered (spec 016).
//!
//! The Mobilithek takes a publication as one `POST` of the DATEX II JSON to the
//! push address it issued, over mutual TLS — the operator authenticates by
//! certificate (Technische Schnittstellenbeschreibung v1.3.2). The document is
//! written by `datex2`'s Mobilithek writer, which refuses what the profile's
//! schemas reject, so a document that would be refused there is refused here,
//! naming the member, and never sent.
//!
//! Its answers are few and documented, and each gets a name an operator can
//! act on, because "HTTP 403" tells nobody that a certificate expired: a
//! `400` is a document the access point refused, a `401`/`403` a certificate it
//! does not accept, a `404` a publication it does not know, and anything else
//! is the access point not answering — the one cause a retry can fix.
//!
//! Only an answer of `2xx` is recorded, by [`crate::Ledger::accepted`], after
//! it came back: a push that failed leaves the feed unconfirmed, and
//! [`crate::Poid::stale`] says so.

use emob_poi::datex::TablePublication;
use emob_service::estate::{EstateError, Mobilithek};

/// How long one push may take.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Why the access point did not take a publication.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The document did not pass the profile's schemas here; nothing was sent.
    #[error("the table does not pass the DATEX II profile and was not sent: {0}")]
    Invalid(String),
    /// `400`: the access point refused the document.
    #[error("the Mobilithek refused the document (400): {0}")]
    Rejected(String),
    /// `401` or `403`: the client certificate is not accepted.
    #[error(
        "the Mobilithek does not accept this operator's certificate ({0}) — check \
         [mobilithek] client_cert and its expiry"
    )]
    NotAuthorised(u16),
    /// `404`: no such publication.
    #[error("the Mobilithek knows no publication at [mobilithek] url (404)")]
    UnknownPublication,
    /// No answer, or one a retry may change.
    #[error(
        "the Mobilithek did not take the publication ({0}); it is sent again on the next round"
    )]
    Unavailable(String),
}

impl Refused {
    /// Whether sending the same document again can succeed — only when the
    /// access point did not answer; every other cause needs a person.
    #[must_use]
    pub const fn retries(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// The HTTP client for `mobilithek`: its certificate presented, its address
/// `https://` unless it is a loopback mock.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming the `[mobilithek]` key that is wrong.
pub fn client(mobilithek: &Mobilithek) -> Result<reqwest::Client, EstateError> {
    let invalid = |key: &str, reason: String| EstateError::Invalid {
        key: format!("mobilithek.{key}"),
        reason,
    };
    if mobilithek.status_max_age_secs == 0 {
        return Err(invalid(
            "status_max_age_secs",
            "a positive source freshness window is required".to_owned(),
        ));
    }
    let destination = |key: &str, raw: &str| {
        let url = reqwest::Url::parse(raw)
            .map_err(|_| invalid(key, "expected an absolute publication URL".to_owned()))?;
        let loopback = url.scheme() == "http"
            && url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
        if !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || (url.scheme() != "https" && !loopback)
        {
            return Err(invalid(
                key,
                "use HTTPS or loopback HTTP, without embedded credentials or fragments".to_owned(),
            ));
        }
        Ok((url, loopback))
    };
    let (table, local) = destination("url", &mobilithek.url)?;
    let (dynamic, local_dynamic) = destination("dynamic_url", &mobilithek.dynamic_url)?;
    if table == dynamic {
        return Err(invalid(
            "dynamic_url",
            "static and dynamic offers need distinct addresses".to_owned(),
        ));
    }
    let loopback = local && local_dynamic;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT);
    match (&mobilithek.client_cert, &mobilithek.client_key) {
        (Some(cert), Some(key)) => {
            let read = |key_name: &str, path: &str| {
                std::fs::read(path)
                    .map_err(|e| invalid(key_name, format!("{path} could not be read ({e})")))
            };
            let mut pem = read("client_cert", cert)?;
            pem.extend(read("client_key", key)?);
            let identity = reqwest::Identity::from_pem(&pem)
                .map_err(|e| invalid("client_cert", format!("not a certificate and key ({e})")))?;
            builder = builder.identity(identity);
        }
        (None, None) if loopback => {}
        _ => {
            return Err(invalid(
                "client_cert",
                "the Mobilithek authenticates the operator by certificate: set client_cert and \
                 client_key"
                    .to_owned(),
            ));
        }
    }
    builder
        .build()
        .map_err(|e| invalid("url", format!("no HTTP client ({e})")))
}

/// Post one table. `Ok` only on `2xx`.
///
/// # Errors
///
/// [`Refused`], by its documented cause.
pub async fn push(
    client: &reqwest::Client,
    url: &str,
    publication: &TablePublication,
) -> Result<(), Refused> {
    let body = publication
        .to_json()
        .map_err(|e| Refused::Invalid(e.to_string()))?;
    post(client, url, body).await
}

/// Send a validated publication and require a direct 2xx acknowledgement.
///
/// # Errors
/// Returns the receiver or transport refusal.
pub async fn post(client: &reqwest::Client, url: &str, body: String) -> Result<(), Refused> {
    let answer = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| Refused::Unavailable(e.without_url().to_string()))?;
    let status = answer.status().as_u16();
    match status {
        200..=299 => Ok(()),
        400 => Err(Refused::Rejected(
            "check the publication against the registered profile".to_owned(),
        )),
        401 | 403 => Err(Refused::NotAuthorised(status)),
        404 => Err(Refused::UnknownPublication),
        other => Err(Refused::Unavailable(format!("HTTP {other}"))),
    }
}
