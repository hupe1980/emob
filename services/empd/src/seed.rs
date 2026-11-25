//! The estate's `[[contracts]]`, journalled once.
//!
//! The contracts and initial card standing for a first deployment come from
//! the estate. A restart preserves the standing last recorded by the provider. Each is applied through
//! the book's journalled facts. A repeated seed leaves the journal unchanged.
//! Changing or removing a configured printed number updates the keyed search
//! index while preserving the card's standing.

use emob_roam::TokenType;
use emob_service::Actor;
use emob_service::estate::{ContractSeed, EstateError};
use emob_service::journal::Journal;

use crate::{Book, BookError, Contract, Whitelist};

/// Why a seed could not be applied.
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    /// The estate entry is not one this provider can issue.
    #[error(transparent)]
    Estate(#[from] EstateError),
    /// The book refused.
    #[error("the estate's {key} could not be applied: {source}")]
    Book {
        /// Which entry.
        key: String,
        /// Why.
        source: BookError,
    },
}

fn token_type(index: usize, token: usize, text: &str) -> Result<TokenType, EstateError> {
    Ok(match text {
        "RFID" => TokenType::Rfid,
        "APP_USER" => TokenType::AppUser,
        "AD_HOC_USER" => TokenType::AdHocUser,
        "EMAID" => TokenType::Emaid,
        "OTHER" => TokenType::Other,
        other => {
            return Err(EstateError::Invalid {
                key: format!("contracts[{index}].tokens[{token}].type"),
                reason: format!("{other} is not an OCPI token type"),
            });
        }
    })
}

fn whitelist(index: usize, token: usize, text: &str) -> Result<Whitelist, EstateError> {
    Ok(match text {
        "ALWAYS" => Whitelist::Always,
        "ALLOWED" => Whitelist::Allowed,
        "ALLOWED_OFFLINE" => Whitelist::AllowedOffline,
        "NEVER" => Whitelist::Never,
        other => {
            return Err(EstateError::Invalid {
                key: format!("contracts[{index}].tokens[{token}].whitelist"),
                reason: format!("{other} is not an OCPI whitelist type"),
            });
        }
    })
}

/// Apply every seed the journal does not already hold. Returns how many facts
/// were written.
///
/// Each fact is the estate's ([`Actor::Estate`]): the file read at start
/// decided it, not a caller.
///
/// # Errors
///
/// [`SeedError`], naming the estate key.
pub fn apply<J: Journal>(book: &mut Book<J>, seeds: &[ContractSeed]) -> Result<usize, SeedError> {
    for (index, seed) in seeds.iter().enumerate() {
        if book.state().agreements.holds_account(&seed.id) {
            return Err(EstateError::Invalid {
                key: format!("contracts[{index}].id"),
                reason: "the signed journal already assigned this native agreement".into(),
            }
            .into());
        }
        for (t, token) in seed.tokens.iter().enumerate() {
            if book
                .state()
                .agreements
                .for_card(&book.state().reference_for(&token.uid.to_ascii_uppercase()))
                .is_some()
            {
                return Err(EstateError::Invalid {
                    key: format!("contracts[{index}].tokens[{t}]"),
                    reason: "the signed journal already assigned this native card".into(),
                }
                .into());
            }
            if token
                .visual_number
                .as_deref()
                .is_some_and(|printed| emob_service::estate::printed_card_number(printed).is_none())
            {
                return Err(EstateError::Invalid {
                    key: format!("contracts[{index}].tokens[{t}].visual_number"),
                    reason: "expected 1 to 64 printable characters".to_owned(),
                }
                .into());
            }
        }
    }
    let mut written = 0;
    for (index, seed) in seeds.iter().enumerate() {
        let book_error = |key: String| move |source| SeedError::Book { key, source };
        if book.state().contract(&seed.id).is_none() {
            let from = time::Date::parse(
                &seed.from,
                time::macros::format_description!("[year]-[month]-[day]"),
            )
            .map_err(|error| EstateError::Invalid {
                key: format!("contracts[{index}].from"),
                reason: error.to_string(),
            })?;
            book.with(&Actor::Estate, Contract::new(seed.id.clone(), from))
                .map_err(book_error(format!("contracts[{index}]")))?;
            written += 1;
        }
        for (t, token) in seed.tokens.iter().enumerate() {
            let key = format!("contracts[{index}].tokens[{t}]");
            let reference = book.state().reference_for(&token.uid);
            if book.state().token(&reference).is_none() {
                book.issue_with_standing(
                    &Actor::Estate,
                    &seed.id,
                    token.uid.clone(),
                    token_type(index, t, &token.token_type)?,
                    whitelist(index, t, &token.whitelist)?,
                    token.blocked,
                )
                .map_err(book_error(key.clone()))?;
                written += 1;
            }
            let digest = token
                .visual_number
                .as_deref()
                .and_then(|printed| book.state().printed_digest(printed));
            if book
                .label(&Actor::Estate, &reference, digest)
                .map_err(book_error(key.clone()))?
            {
                written += 1;
            }
        }
    }
    Ok(written)
}
