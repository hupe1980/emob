//! The estate's `[[contracts]]`, journalled once.
//!
//! 0.2 adds no write endpoint, so the contracts and tokens a demonstration or
//! a first deployment starts with come from the estate. Each is applied through
//! the book's own journalled facts — `Contracted`, `Issued`, `Blocked` — and
//! only when the journal does not already hold it, so a second start changes
//! nothing and a token blocked since stays blocked.

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
                book.issue_token(
                    &Actor::Estate,
                    &seed.id,
                    token.uid.clone(),
                    token_type(index, t, &token.token_type)?,
                    whitelist(index, t, &token.whitelist)?,
                )
                .map_err(book_error(key.clone()))?;
                written += 1;
            }
            let blocked = book
                .state()
                .token(&reference)
                .is_some_and(|held| held.blocked);
            if token.blocked && !blocked {
                book.block(&Actor::Estate, &reference)
                    .map_err(book_error(key))?;
                written += 1;
            }
        }
    }
    Ok(written)
}
