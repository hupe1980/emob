//! Bounded lists for the operator views (spec 007).
//!
//! A read model that returns its whole history is fast on day one and down on
//! day forty. Every list an operator view serves is therefore a [`Page`]: the
//! count, the newest `limit` items, and the cursor for the page before them.
//!
//! A cursor is a **position** in the daemon's append-only fold — an index that
//! only grows — so the page after "show older" is the same page however many
//! sessions arrived in between. It is an integer, not a secret: a forged one
//! selects another page of a list the token may already read (SR-090).
//!
//! The limit and every cursor are checked, not clamped: a `limit` of a million
//! is a `400`, so is a parameter the view does not know (SR-088, SR-089).

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The rows a page holds when the caller does not say.
pub const DEFAULT_LIMIT: usize = 50;

/// The most rows a caller may ask for.
pub const MAX_LIMIT: usize = 200;

/// One page of a list, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Page<T> {
    /// How many items the whole list holds.
    pub total: u64,
    /// At most `limit` of them, newest first.
    pub items: Vec<T>,
    /// The cursor for the next, older page; absent on the last.
    pub older: Option<u64>,
}

impl<T> Page<T> {
    /// The newest `limit` of `ascending` — `(position, item)` pairs in
    /// ascending position, already cut at the caller's cursor — with the
    /// cursor for what remains: the position of the oldest item shown.
    pub fn newest(
        ascending: impl DoubleEndedIterator<Item = (u64, T)>,
        total: u64,
        limit: usize,
    ) -> Self {
        let mut newest = ascending.rev();
        let mut items = Vec::new();
        let mut oldest = None;
        for (position, item) in newest.by_ref().take(limit) {
            oldest = Some(position);
            items.push(item);
        }
        let older = newest.next().and(oldest);
        Self {
            total,
            items,
            older,
        }
    }

    /// `limit` items of a list that is not history, from `offset` on: the
    /// cursor for the next page is the offset after this one.
    pub fn from_offset(
        all: impl Iterator<Item = T>,
        total: u64,
        offset: u64,
        limit: usize,
    ) -> Self {
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        let items: Vec<T> = all.skip(skip).take(limit).collect();
        let next = offset.saturating_add(items.len() as u64);
        Self {
            total,
            older: (next < total && !items.is_empty()).then_some(next),
            items,
        }
    }
}

/// A view refused its query: `400` with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "reason": self.0 })),
        )
            .into_response()
    }
}

impl From<QueryRejection> for Refused {
    fn from(rejection: QueryRejection) -> Self {
        Self(format!(
            "the query is not one this view reads: {}",
            rejection.body_text()
        ))
    }
}

/// The limit a caller asked for, or [`DEFAULT_LIMIT`].
///
/// # Errors
///
/// [`Refused`] for a limit outside `1..=MAX_LIMIT`.
pub fn limit(asked: Option<usize>) -> Result<usize, Refused> {
    match asked {
        None => Ok(DEFAULT_LIMIT),
        Some(limit @ 1..=MAX_LIMIT) => Ok(limit),
        Some(other) => Err(Refused(format!(
            "limit {other} is not between 1 and {MAX_LIMIT}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sr_088_the_limit_is_checked_not_clamped() {
        assert_eq!(limit(None), Ok(50));
        assert_eq!(limit(Some(1)), Ok(1));
        assert_eq!(limit(Some(200)), Ok(200));
        for wrong in [0, 201, usize::MAX] {
            let Refused(reason) = limit(Some(wrong)).unwrap_err();
            assert!(reason.contains("between 1 and 200"), "{reason}");
        }
    }

    #[test]
    fn the_newest_come_first_and_the_cursor_is_the_oldest_shown() {
        let list = (0..10_u64).map(|p| (p, p));
        let page = Page::newest(list.clone(), 10, 3);
        assert_eq!(page.items, [9, 8, 7]);
        assert_eq!(page.older, Some(7));
        // The next page: everything strictly older than the cursor.
        let next = Page::newest(list.clone().filter(|(p, _)| *p < 7), 10, 3);
        assert_eq!(next.items, [6, 5, 4]);
        let oldest = Page::newest(list.filter(|(p, _)| *p < 3), 10, 3);
        assert_eq!((oldest.items, oldest.older), (vec![2, 1, 0], None));
    }

    #[test]
    fn an_offset_page_says_where_the_next_starts_and_survives_any_offset() {
        let page = Page::from_offset(0..5, 5, 0, 2);
        assert_eq!((page.items, page.older), (vec![0, 1], Some(2)));
        let page = Page::from_offset(0..5, 5, 4, 2);
        assert_eq!((page.items, page.older), (vec![4], None));
        let page = Page::from_offset(0..5, 5, u64::MAX, 2);
        assert_eq!((page.items, page.older), (Vec::<i32>::new(), None));
    }
}
