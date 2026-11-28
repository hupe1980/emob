//! Private bill-to identities for native retail agreements.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::EstateError;

/// How an operator identifies the payer during enrolment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum RetailAccountKind {
    /// One private bill-to party.
    Private,
    /// A business that pays for its drivers.
    Fleet,
}

/// Private invoice particulars configured by the provider.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RetailAccount {
    /// Stable, opaque bill-to identifier, shared by agreements with the same payer.
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[A-Z0-9][A-Z0-9-]{0,31}$"))
    )]
    pub id: String,
    /// Individual or fleet payer.
    pub kind: RetailAccountKind,
    /// Legal name on the invoice.
    pub name: String,
    /// Street and building number.
    pub street: String,
    /// Postal code.
    pub post_code: String,
    /// Postal locality.
    pub city: String,
    /// Two-letter country code.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[A-Z]{2}$")))]
    pub country: String,
    /// Business VAT identifier, when applicable.
    pub vat_id: Option<String>,
    /// Electronic invoice address, when available.
    pub electronic_address: Option<String>,
}

impl core::fmt::Debug for RetailAccount {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RetailAccount").finish_non_exhaustive()
    }
}

/// Validate private invoice profiles before an operator assigns a card.
///
/// # Errors
/// Identifies the section and field, excluding the bill-to particulars.
pub fn validate_retail_accounts(accounts: &[RetailAccount]) -> Result<(), EstateError> {
    let mut ids = BTreeSet::new();
    for (i, account) in accounts.iter().enumerate() {
        let invalid = |field: &str, reason: &str| EstateError::Invalid {
            key: format!("retail_accounts[{i}].{field}"),
            reason: reason.to_owned(),
        };
        if !valid_account_id(&account.id) || !ids.insert(&account.id) {
            return Err(invalid(
                "id",
                "expected a distinct uppercase account identifier (1–32 letters, digits or hyphens)",
            ));
        }
        if account.country.len() != 2 || !account.country.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(invalid(
                "country",
                "expected a two-letter uppercase country code",
            ));
        }
        for (key, value) in [
            ("name", Some(&account.name)),
            ("street", Some(&account.street)),
            ("post_code", Some(&account.post_code)),
            ("city", Some(&account.city)),
            ("vat_id", account.vat_id.as_ref()),
            ("electronic_address", account.electronic_address.as_ref()),
        ] {
            if value.is_some_and(|s| {
                s.is_empty() || s.trim() != s || s.len() > 254 || s.chars().any(char::is_control)
            }) {
                return Err(invalid(
                    key,
                    "expected a non-empty single-line value of up to 254 bytes",
                ));
            }
        }
        if account.kind == RetailAccountKind::Private && account.vat_id.is_some() {
            return Err(invalid(
                "vat_id",
                "only a business payer can declare a VAT identifier",
            ));
        }
    }
    Ok(())
}

/// Whether the supplied text is an opaque native billing-account identifier.
#[must_use]
pub fn valid_account_id(id: &str) -> bool {
    let part = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    id.len() <= 32
        && id.bytes().next().is_some_and(part)
        && id.bytes().all(|b| part(b) || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estate::{Estate, from_str};

    #[test]
    fn duplicate_ids_and_private_diagnostics_reveal_no_billing_particulars() {
        let config = include_str!("../../../../deploy/estate.example.toml");
        let mut accounts = from_str::<Estate>(config).unwrap().retail_accounts;
        validate_retail_accounts(&accounts).unwrap();
        accounts[1].id = accounts[0].id.clone();
        let error = validate_retail_accounts(&accounts).unwrap_err().to_string();
        assert!(error.contains("retail_accounts[1].id"));
        assert!(!error.contains(&accounts[0].id));
        let err = from_str::<Estate>(
            &config.replace("kind = \"private\"", "kind = \"sensitive-person\""),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid retail payer") && !err.contains("sensitive-person"));
    }
}
