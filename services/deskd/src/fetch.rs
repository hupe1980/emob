//! Reading the three bounded views, and remembering since when one has not
//! answered.
//!
//! Each read has a timeout and a body cap (SR-055): only the `/today` views
//! are read, whose size is bounded by the page and the estate, so the cap is
//! 1 MiB and a body past it is a fault, not a busy day (SR-092). A read that fails is
//! described by its **class** — no answer, refused, unreadable — never by the
//! request, its headers or the body it got back, so the operator token can
//! never reach the page or a log line through an error message (SR-053).

use core::fmt::Write as _;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use emob_service::config::Secret;
use serde::de::DeserializeOwned;

/// How long one read may take.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The most one view may weigh. Fifty refusals with their findings and the
/// silent stations of a large estate fit several times over; an endless body
/// does not.
pub const BODY_CAP: usize = 1024 * 1024;

/// The most a signed head may weigh.
pub const HEAD_CAP: usize = 4096;

/// Why a source did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Nothing came back in time, or nothing listened.
    NoAnswer,
    /// It answered `401`/`403`: the token is not one it admits.
    Refused,
    /// It answered another status.
    Status(u16),
    /// It answered something that is not its read model.
    Unreadable,
}

/// One daemon's read model, or why it is missing and since when.
#[derive(Debug, Clone)]
pub enum Source<T> {
    /// It answered.
    Up(T),
    /// It did not.
    Down {
        /// The first failure since it last answered.
        since: time::OffsetDateTime,
        /// What went wrong.
        why: Why,
    },
}

impl<T> Source<T> {
    /// The read model, when there is one.
    pub const fn up(&self) -> Option<&T> {
        match self {
            Self::Up(value) => Some(value),
            Self::Down { .. } => None,
        }
    }
}

/// The three daemons, and the one piece of memory `deskd` keeps: since when
/// each has been down. A restart forgets it, and says "since" the restart.
pub struct Reader {
    client: reqwest::Client,
    token: Secret,
    down: Mutex<BTreeMap<&'static str, time::OffsetDateTime>>,
}

impl core::fmt::Debug for Reader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reader").finish_non_exhaustive()
    }
}

impl Reader {
    /// A reader presenting `token`.
    ///
    /// # Errors
    ///
    /// The HTTP client could not be built.
    pub fn new(token: Secret) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            token,
            down: Mutex::new(BTreeMap::new()),
        })
    }

    /// Read `name`'s view at `base` + `/v1/<name>/today`, `limit` rows per
    /// list, each `(list, cursor)` passed on. The query is built from integers
    /// and fixed names only, so nothing a browser sent reaches the URL as text
    /// (SR-095).
    pub async fn read<T: DeserializeOwned>(
        &self,
        name: &'static str,
        base: &str,
        limit: usize,
        cursors: &[(&'static str, Option<u64>)],
        now: time::OffsetDateTime,
    ) -> Source<T> {
        let mut url = format!(
            "{}/v1/{name}/today?limit={limit}",
            base.trim_end_matches('/')
        );
        for (list, cursor) in cursors {
            if let Some(cursor) = cursor {
                let _ = write!(url, "&{list}={cursor}");
            }
        }
        match self.fetch(&url).await {
            Ok(value) => {
                self.down
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(name);
                Source::Up(value)
            }
            Err(why) => {
                tracing::warn!(daemon = name, why = ?why, "a read model did not answer");
                let since = *self
                    .down
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(name)
                    .or_insert(now);
                Source::Down { since, why }
            }
        }
    }

    /// `GET <base>/v1/<name>/head`: the signed note and the verifier key the
    /// daemon sends with it — for the witness (spec 008). A head is a few
    /// hundred bytes, so the cap is [`HEAD_CAP`]; a failure is reported by
    /// class only, like every read here (SR-099).
    ///
    /// # Errors
    ///
    /// Why it did not answer.
    pub async fn head(&self, base: &str, name: &str) -> Result<(String, Option<String>), Why> {
        let url = format!("{}/v1/{name}/head", base.trim_end_matches('/'));
        let (body, headers) = self.bytes(&url, true, HEAD_CAP).await?;
        let key = headers
            .get(emob_service::head::VERIFIER_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        String::from_utf8(body)
            .map(|note| (note, key))
            .map_err(|_| Why::Unreadable)
    }

    /// The daemon's name, from `GET <base>/about` (`"<name> <version>"`).
    ///
    /// # Errors
    ///
    /// Why it did not answer.
    pub async fn name(&self, base: &str) -> Result<String, Why> {
        let url = format!("{}/about", base.trim_end_matches('/'));
        let (body, _) = self.bytes(&url, false, HEAD_CAP).await?;
        let text = String::from_utf8(body).map_err(|_| Why::Unreadable)?;
        text.split_whitespace()
            .next()
            .filter(|name| name.bytes().all(|b| b.is_ascii_lowercase()))
            .map(str::to_owned)
            .ok_or(Why::Unreadable)
    }

    /// One station from `csmsd` (spec 012): `None` when the estate does not
    /// list it. The identity is percent-encoded byte by byte, so nothing a
    /// browser sent can change the path (SR-095).
    ///
    /// # Errors
    ///
    /// Why it did not answer.
    pub async fn station<T: DeserializeOwned>(
        &self,
        base: &str,
        identity: &str,
    ) -> Result<Option<T>, Why> {
        let url = format!(
            "{}/v1/csmsd/stations/{}",
            base.trim_end_matches('/'),
            crate::station::href(identity).trim_start_matches("/station/")
        );
        match self.fetch(&url).await {
            Ok(page) => Ok(Some(page)),
            Err(Why::Status(404)) => Ok(None),
            Err(why) => Err(why),
        }
    }

    /// A write to `csmsd` with the **operator's own** token — never the
    /// desk's read token (spec 021): `(status, body)`, the body JSON or null.
    ///
    /// # Errors
    ///
    /// When `csmsd` does not answer.
    pub async fn write(
        &self,
        method: reqwest::Method,
        url: &str,
        operator: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), Why> {
        let response = self
            .client
            .request(method, url)
            .bearer_auth(operator)
            .json(body)
            .send()
            .await
            .map_err(|_| Why::NoAnswer)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|_| Why::NoAnswer)?;
        Ok((
            status,
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
        ))
    }

    /// One page of `csmsd`'s station search, inside `area` when given.
    ///
    /// # Errors
    ///
    /// When `csmsd` does not answer or refuses the read.
    pub async fn stations<T: DeserializeOwned>(
        &self,
        base: &str,
        query: &str,
        area: Option<&str>,
        offset: u64,
        limit: usize,
    ) -> Result<T, Why> {
        let mut url = format!(
            "{}/v1/csmsd/stations?limit={limit}&offset={offset}",
            base.trim_end_matches('/')
        );
        search(&mut url, query, area);
        self.fetch(&url).await
    }

    /// The map of the same search (spec 022).
    ///
    /// # Errors
    ///
    /// When `csmsd` does not answer or refuses the read.
    pub async fn map<T: DeserializeOwned>(
        &self,
        base: &str,
        query: &str,
        area: Option<&str>,
    ) -> Result<T, Why> {
        let mut url = format!("{}/v1/csmsd/map?", base.trim_end_matches('/'));
        search(&mut url, query, area);
        self.fetch(&url).await
    }

    async fn bytes(
        &self,
        url: &str,
        token: bool,
        cap: usize,
    ) -> Result<(Vec<u8>, reqwest::header::HeaderMap), Why> {
        let mut request = self.client.get(url);
        if token {
            request = request.bearer_auth(self.token.expose());
        }
        let mut response = request.send().await.map_err(|_| Why::NoAnswer)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(Why::Refused),
            other => return Err(Why::Status(other)),
        }
        let headers = response.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Why::NoAnswer)? {
            body.extend_from_slice(&chunk);
            if body.len() > cap {
                return Err(Why::Unreadable);
            }
        }
        Ok((body, headers))
    }

    async fn fetch<T: DeserializeOwned>(&self, url: &str) -> Result<T, Why> {
        let mut response = self
            .client
            .get(url)
            .bearer_auth(self.token.expose())
            .send()
            .await
            .map_err(|_| Why::NoAnswer)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(Why::Refused),
            other => return Err(Why::Status(other)),
        }
        if response
            .content_length()
            .is_some_and(|length| length > BODY_CAP as u64)
        {
            return Err(Why::Unreadable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Why::NoAnswer)? {
            body.extend_from_slice(&chunk);
            if body.len() > BODY_CAP {
                return Err(Why::Unreadable);
            }
        }
        serde_json::from_slice(&body).map_err(|_| Why::Unreadable)
    }
}

/// `&q=…` and `&area=…`, percent-encoded byte by byte.
fn search(url: &mut String, query: &str, area: Option<&str>) {
    for (name, value) in [("q", Some(query)), ("area", area)] {
        let Some(value) = value else { continue };
        let _ = write!(url, "&{name}=");
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b".-_~".contains(&byte) {
                url.push(char::from(byte));
            } else {
                let _ = write!(url, "%{byte:02X}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A daemon on loopback that answers every path with `body`, and keeps
    /// the URIs it was asked.
    async fn upstream(body: Arc<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&asked);
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let body = Arc::clone(&body);
            let seen = Arc::clone(&seen);
            async move {
                seen.lock().unwrap().push(uri.to_string());
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body.as_str().to_owned(),
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), asked)
    }

    #[tokio::test]
    async fn sr_092_sr_095_only_the_bounded_view_is_read_and_a_body_past_a_mib_is_refused() {
        let reader = Reader::new(Secret::new("t".to_owned())).unwrap();
        let now = time::OffsetDateTime::UNIX_EPOCH;

        let (base, asked) = upstream(Arc::new("{}".to_owned())).await;
        let read: Source<serde_json::Value> = reader
            .read(
                "csmsd",
                &base,
                50,
                &[("refused", Some(7)), ("silent", None)],
                now,
            )
            .await;
        assert!(read.up().is_some());
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            ["/v1/csmsd/today?limit=50&refused=7"]
        );

        let huge = format!("\"{}\"", "x".repeat(BODY_CAP));
        let (base, _) = upstream(Arc::new(huge)).await;
        let read: Source<serde_json::Value> = reader.read("billd", &base, 50, &[], now).await;
        assert!(
            matches!(
                read,
                Source::Down {
                    why: Why::Unreadable,
                    ..
                }
            ),
            "{read:?}"
        );
    }
}
