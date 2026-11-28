//! The ordered retail documents finance can book, reconstructed from the signed billing journal.
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::{Json, Router, routing::get};
use emob_core::{Currency, PartyId};
use emob_service::{Operators, journal::Journal};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::Ledger;
use super::http::Shared;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    #[serde(default)]
    after: usize,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct Booking {
    position: usize,
    issuer: PartyId,
    account: String,
    currency: Currency,
    number: String,
    kind: &'static str,
    amount_minor: i64,
    predecessor: Option<String>,
}

#[derive(Serialize)]
struct Page {
    documents: Vec<Booking>,
    next_after: usize,
    has_more: bool,
}

/// Only amounts that finance can represent exactly may be issued.
pub(super) fn minor_units(currency: Currency, payable: Decimal) -> Option<i64> {
    let multiplier = Decimal::from(10_u64.pow(currency.minor_unit_digits()));
    let minor = payable.checked_mul(multiplier)?;
    if minor < Decimal::ZERO || !minor.fract().is_zero() {
        return None;
    }
    i64::try_from(minor).ok()
}

impl Ledger {
    fn booking(&self, position: usize, number: &str) -> Option<Booking> {
        let row = self.document_row(number)?;
        Some(Booking {
            position,
            issuer: row.issuer,
            account: row.account,
            currency: row.currency,
            amount_minor: minor_units(row.currency, row.payable.parse().ok()?)?,
            number: row.number,
            kind: row.kind,
            predecessor: row.supersedes,
        })
    }

    fn bookings(&self, cursor: &Cursor) -> Result<Page, StatusCode> {
        let limit = cursor.limit.unwrap_or(50);
        if !(1..=100).contains(&limit) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let numbers = self
            .document_numbers
            .get(cursor.after..)
            .ok_or(StatusCode::CONFLICT)?;
        let documents = numbers
            .iter()
            .take(limit)
            .enumerate()
            .map(|(i, number)| {
                self.booking(cursor.after + i + 1, number)
                    .ok_or(StatusCode::SERVICE_UNAVAILABLE)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            next_after: cursor.after + documents.len(),
            has_more: numbers.len() > documents.len(),
            documents,
        })
    }
}

async fn bookings<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Query(cursor): Query<Cursor>,
) -> Result<Json<Page>, StatusCode> {
    let books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    books.state().retail.bookings(&cursor).map(Json)
}

pub(super) fn routes<J: Journal>(owner: Arc<Shared<J>>, operators: Operators) -> Router {
    let routes = Router::new()
        .route("/v1/billd/receivables", get(bookings::<J>))
        .with_state(owner);
    emob_service::serve::finance_only(routes, operators)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn booking_amounts_preserve_every_minor_unit_or_are_refused() {
        let amount = |value: &str| minor_units(Currency::EUR, value.parse().unwrap());
        assert_eq!(amount("0.00"), Some(0));
        assert_eq!(amount("0.01"), Some(1));
        assert_eq!(amount("0.001"), None);
        assert_eq!(amount("-0.01"), None);
        assert_eq!(amount("92233720368547758.07"), Some(i64::MAX));
        assert_eq!(amount("92233720368547758.08"), None);
        assert_eq!(minor_units(Currency::EUR, Decimal::MAX), None);
    }
}
