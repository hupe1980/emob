//! Advance only after a bounded, contiguous billing page has been checked.
use std::time::Duration;

use axum::http::HeaderValue;
use emob_service::Actor;
use serde::Deserialize;

use crate::api::Owner;
use crate::ledger::{OFFLINE, Result};
use crate::model::{Document, Fact};

#[derive(Deserialize)]
struct Page {
    documents: Vec<Document>,
    next_after: u64,
    has_more: bool,
}

impl Owner {
    async fn fetch(&self, bearer: &HeaderValue, after: u64) -> Result<Page> {
        let mut response = self
            .client
            .get(self.feed.clone())
            .query(&[("after", after), ("limit", 100)])
            .header(reqwest::header::AUTHORIZATION, bearer)
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .map_err(|_| OFFLINE)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(OFFLINE);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| OFFLINE)? {
            if bytes.len() + chunk.len() > 128 * 1024 {
                return Err(OFFLINE);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| OFFLINE)
    }

    pub async fn sync(&self, bearer: &HeaderValue) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(6), self.catch_up(bearer))
            .await
            .map_err(|_| OFFLINE)?
    }

    async fn catch_up(&self, bearer: &HeaderValue) -> Result<()> {
        let _sync = self.syncing.lock().await;
        let last = {
            let book = self.book()?;
            book.live()?;
            book.state.last.clone()
        };
        let mut after = last.as_ref().map_or(0, |d| d.position.saturating_sub(1));
        let mut anchor = last;
        // Keep each request bounded. A large initial backlog advances durably in pages.
        for _ in 0..16 {
            let page = self.fetch(bearer, after).await?;
            let length = u64::try_from(page.documents.len()).map_err(|_| OFFLINE)?;
            if length > 100
                || Some(page.next_after) != after.checked_add(length)
                || (page.has_more && length == 0)
                || page
                    .documents
                    .iter()
                    .enumerate()
                    .any(|(i, d)| d.position != after + 1 + i as u64)
            {
                return Err(OFFLINE);
            }
            let mut docs = page.documents.into_iter();
            if let Some(previous) = anchor.take()
                && docs.next().as_ref() != Some(&previous)
            {
                return Err(OFFLINE);
            }
            let mut book = self.book()?;
            book.live()?;
            for document in docs {
                book.commit(&Actor::service("billd"), &Fact::Document { document })
                    .map_err(|_| OFFLINE)?;
            }
            if !page.has_more {
                return Ok(());
            }
            after = page.next_after;
        }
        Err(OFFLINE)
    }
}
