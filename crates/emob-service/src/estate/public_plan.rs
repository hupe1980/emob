//! Explicitly public price data. These types never contain a contract or a card.
use super::EstateError;
use emob_core::{Currency, EvseId, PartyId};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use time::OffsetDateTime;

/// One retail product. A public code denotes identical fees at all of its eligible points.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PublicPlan {
    /// Public provider party id.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub provider: PartyId,
    /// Stable plan identifier.
    pub code: String,
    /// Public name printed on the contract.
    pub name: String,
    /// Provider approval that every contract with this code has these terms.
    pub uniform_public_terms: bool,
    /// Opted-in points whose publicly stated price the retail plan passes through.
    #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
    pub points: Vec<EvseId>,
    /// Explicit immutable fee versions.
    pub schedules: Vec<PublicSchedule>,
}

/// A half-open interval of public provider terms. All amounts include VAT.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PublicSchedule {
    /// Immutable issuer-assigned version.
    pub version: String,
    /// First instant included.
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub from: OffsetDateTime,
    /// First instant excluded, when known.
    #[serde(with = "time::serde::rfc3339::option", default)]
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub until: Option<OffsetDateTime>,
    /// Currency of all charges.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub currency: Currency,
    /// Percentage VAT included in all fees.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    #[serde(with = "rust_decimal::serde::str")]
    pub vat: Decimal,
    /// Gross fee per kWh.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    #[serde(with = "rust_decimal::serde::str")]
    pub energy: Decimal,
    /// Gross fee per session.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    #[serde(with = "rust_decimal::serde::str")]
    pub session: Decimal,
    /// Gross e-roaming charge per session.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    #[serde(with = "rust_decimal::serde::str")]
    pub roaming: Decimal,
    /// Gross recurring EUR per month, explicitly zero for no subscription.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    #[serde(with = "rust_decimal::serde::str")]
    pub monthly: Decimal,
}

impl PublicPlan {
    /// A later fee version supersedes an open-ended predecessor without changing its published contents.
    #[must_use]
    pub fn effective_until(&self, fees: &PublicSchedule) -> Option<OffsetDateTime> {
        fees.until.or_else(|| {
            self.schedules
                .iter()
                .filter(|later| later.from > fees.from)
                .map(|later| later.from)
                .min()
        })
    }
}

impl PublicSchedule {
    /// The provider fees used by the shared retail evaluator.
    #[must_use]
    pub const fn fees(&self) -> emob_tariff::retail::Fees {
        emob_tariff::retail::Fees {
            energy: self.energy,
            session: self.session,
            roaming: self.roaming,
            vat: self.vat,
        }
    }
}

/// Check opt-in, origin and uniqueness before enabling the public projection.
///
/// # Errors
///
/// An estate error names the first unpublishable entry.
pub fn validate_public_plans(plans: &[PublicPlan], origin: &str) -> Result<(), EstateError> {
    if plans.is_empty() {
        return Ok(());
    }
    let invalid = |key: String, reason: &str| EstateError::Invalid {
        key,
        reason: reason.into(),
    };
    validate_public_origin(origin)?;
    let mut codes = BTreeSet::new();
    for (i, plan) in plans.iter().enumerate() {
        let bad = |msg| invalid(format!("public_plans[{i}]"), msg);
        if !valid_code(&plan.code)
            || !plan.uniform_public_terms
            || plan.name.trim().is_empty()
            || plan.name.len() > 160
            || !codes.insert((plan.provider.to_string(), &plan.code))
        {
            return Err(bad(
                "expected a unique public plan code and explicit approval of uniform public terms",
            ));
        }
        let points: BTreeSet<_> = plan.points.iter().map(ToString::to_string).collect();
        if points.is_empty() || points.len() != plan.points.len() || plan.schedules.is_empty() {
            return Err(bad(
                "declare unique eligible points and at least one fee version",
            ));
        }
        let mut starts = BTreeMap::new();
        let mut versions = BTreeSet::new();
        for v in &plan.schedules {
            if !valid_code(&v.version)
                || !versions.insert(&v.version)
                || starts.insert(v.from, v.until).is_some()
                || v.until.is_some_and(|end| end <= v.from)
                || v.currency != Currency::EUR
                || v.vat < Decimal::ZERO
                || v.vat > Decimal::from(100)
                || [v.energy, v.session, v.roaming, v.monthly]
                    .iter()
                    .any(|n| *n < Decimal::ZERO || *n > Decimal::from(1_000_000))
            {
                return Err(bad(
                    "invalid or duplicate version, date range, currency, VAT or amount",
                ));
            }
        }
        let intervals: Vec<_> = starts.into_iter().collect();
        if intervals
            .windows(2)
            .any(|ab| ab[0].1.is_some_and(|end| end > ab[1].0))
        {
            return Err(bad("public price schedules overlap"));
        }
    }
    Ok(())
}

/// Check the public origin used by current and archived offer routes.
///
/// # Errors
///
/// Returns an estate diagnostic when the public origin is not HTTPS or loopback.
pub fn validate_public_origin(origin: &str) -> Result<(), EstateError> {
    let uri = origin.parse::<axum::http::Uri>().ok();
    let valid_origin = uri.as_ref().is_some_and(|u| {
        let host = u.host().unwrap_or("").trim_matches(['[', ']']);
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        !host.is_empty()
            && !u.authority().is_some_and(|a| a.as_str().contains('@'))
            && u.query().is_none()
            && matches!(u.path(), "" | "/")
            && (u.scheme_str() == Some("https") || u.scheme_str() == Some("http") && loopback)
    });
    if !valid_origin {
        return Err(EstateError::Invalid {
            key: "receipts.public_url".into(),
            reason: "public provider prices require an HTTPS origin (HTTP on loopback only)".into(),
        });
    }
    Ok(())
}

fn valid_code(value: &str) -> bool {
    (1..=40).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Check public plans against the configured estate for the CLI.
///
/// # Errors
///
/// Names the invalid offer and point. The daemon checks the live registry.
pub fn check_public_plans(whole: &super::Estate) -> Result<(), EstateError> {
    if whole.public_plans.is_empty() {
        return Ok(());
    }
    validate_public_plans(
        &whole.public_plans,
        whole
            .receipts
            .as_ref()
            .map_or("", |s| s.public_url.as_str()),
    )?;
    let histories = super::histories(&whole.tariffs)?;
    let mut issues = Vec::new();
    for plan in &whole.public_plans {
        let mut schedules: Vec<_> = plan.schedules.iter().collect();
        schedules.sort_by_key(|version| version.from);
        for pair in schedules.windows(2) {
            if pair[0].until.is_some_and(|end| end < pair[1].from) {
                issues.push(format!(
                    "{}/{}: no fees between versions {} and {}",
                    plan.provider, plan.code, pair[0].version, pair[1].version
                ));
            }
        }
        for point in &plan.points {
            let context = format!("{}/{}, {point}", plan.provider, plan.code);
            let Some(station) = whole.stations.iter().find(|s| s.evse == *point) else {
                issues.push(format!(
                    "{context}: point absent from the configured estate; check the live registry"
                ));
                continue;
            };
            if !station.publicly_accessible {
                issues.push(format!("{context}: point is private"));
                continue;
            }
            let Some(history) = histories.get(&station.tariff) else {
                issues.push(format!("{context}: unknown base tariff {}", station.tariff));
                continue;
            };
            for version in &plan.schedules {
                if history.in_force_at(version.from).is_none() {
                    issues.push(format!(
                        "{context}, {}: no operator tariff when provider fees begin",
                        version.version
                    ));
                }

                for base in history.versions() {
                    if base.valid_until.is_some_and(|end| end <= version.from)
                        || plan
                            .effective_until(version)
                            .zip(base.valid_from)
                            .is_some_and(|(end, start)| end <= start)
                    {
                        continue;
                    }
                    if let Err(reason) =
                        emob_tariff::retail::public_retail_tariff(base, version.fees())
                    {
                        issues.push(format!("{context}, {}: {reason}", version.version));
                    }
                }
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(EstateError::Invalid {
            key: "public_plans".into(),
            reason: issues.join("; "),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estate::{Estate, from_str};
    const ESTATE: &str = include_str!("../../../../deploy/estate.example.toml");

    #[test]
    fn public_plan_origin_is_tls_except_on_loopback_and_never_uses_credentials() {
        // SR-201. A public link cannot be downgraded through config or disguised userinfo.
        let estate: Estate = from_str(ESTATE).unwrap();
        for origin in [
            "https://prices.example",
            "http://127.0.0.1:9999",
            "http://[::1]:9999",
        ] {
            validate_public_plans(&estate.public_plans, origin).unwrap();
        }
        for origin in [
            "http://prices.example",
            "http://localhost.example",
            "https://user@prices.example",
            "https://prices.example/?secret=1",
        ] {
            assert!(
                validate_public_plans(&estate.public_plans, origin).is_err(),
                "{origin}"
            );
        }
    }

    #[test]
    fn legacy_estates_private_points_and_private_plans_do_not_publish_prices() {
        // SR-206. Only an expressly approved uniform plan on a public point can be enabled.
        let mut estate: Estate = from_str(ESTATE).unwrap();
        check_public_plans(&estate).unwrap();
        estate.tariffs[0].valid_from = Some("2026-01-01T00:00:00Z".into());
        assert!(check_public_plans(&estate).is_err());
        estate.tariffs[0].valid_from = None;
        estate.stations[0].publicly_accessible = false;
        assert!(check_public_plans(&estate).is_err());
        estate.stations[0].publicly_accessible = true;
        estate.public_plans[0].uniform_public_terms = false;
        assert!(check_public_plans(&estate).is_err());
        estate.public_plans.clear();
        check_public_plans(&estate).unwrap();
    }
}
