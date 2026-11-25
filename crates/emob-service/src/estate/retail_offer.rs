//! The exact retail terms shared by the public price page and the provider's intake.
use super::PublicSchedule;
use emob_core::{Currency, EvseId, PartyId};
use emob_tariff::Tariff;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;

/// One published combination of operator tariff, provider fees and point assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetailTerms {
    /// Eligible charge point.
    pub point: EvseId,
    /// Issuer of the plan.
    pub provider: PartyId,
    /// Operator of this point.
    pub operator: PartyId,
    /// Provider-scoped plan code.
    pub code: String,
    /// Plan name shown to the driver.
    pub name: String,
    /// Full operator tariff at this point, including its units, tax and validity.
    pub base: Tariff,
    /// Exact gross provider charges, including the monthly fee.
    pub fees: PublicSchedule,
    /// A subsequent fee schedule ends an originally open-ended offer.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub superseded: Option<OffsetDateTime>,
}

impl RetailTerms {
    /// Content identity shared with the immutable public price archive.
    ///
    /// # Panics
    ///
    /// The input must contain timestamps representable in RFC 3339, as supplied
    /// by the estate and the HTTP and journal deserializers.
    #[must_use]
    pub fn reference(&self) -> String {
        let wire = serde_json::to_vec(&("provider-offer-v1", self))
            .expect("published retail terms have RFC 3339 timestamps");
        hex::encode(Sha256::digest(wire))
    }

    /// First instant covered by both source schedules.
    #[must_use]
    pub fn from(&self) -> OffsetDateTime {
        self.base
            .valid_from
            .map_or(self.fees.from, |from| from.max(self.fees.from))
    }

    /// Earliest exclusive upper bound of the two schedules or their announced successor.
    #[must_use]
    pub fn until(&self) -> Option<OffsetDateTime> {
        [self.base.valid_until, self.fees.until, self.superseded]
            .into_iter()
            .flatten()
            .min()
    }

    /// Combine gross session rates while leaving the monthly subscription separate.
    ///
    /// # Errors
    ///
    /// Rejects unsupported tariffs, currencies, tax bases, and amounts.
    pub fn retail_tariff(&self) -> Result<Tariff, &'static str> {
        if self.fees.currency != Currency::EUR
            || self.fees.monthly < Decimal::ZERO
            || self.fees.monthly > Decimal::from(1_000_000)
        {
            return Err("invalid currency or monthly fee");
        }
        emob_tariff::retail::public_retail_tariff(&self.base, self.fees.fees())
    }
}

/// Public terms paired with their immutable archive reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetailOffer {
    /// Full lowercase SHA-256 reference used on the public price page.
    pub reference: String,
    /// Terms accepted by the customer, including provider and point assignment.
    pub terms: RetailTerms,
}

impl RetailOffer {
    /// Verify the point, provider, plan, term validity and full content reference
    /// of an offer received from the configured CSMS.
    #[must_use]
    pub fn matches(
        &self,
        point: &EvseId,
        provider: &PartyId,
        plan: &str,
        at: OffsetDateTime,
    ) -> bool {
        let terms = &self.terms;
        &terms.point == point
            && &terms.provider == provider
            && terms.code == plan
            && terms.from() <= at
            && terms.until().is_none_or(|end| at < end)
            && terms.retail_tariff().is_ok()
            && self.reference == terms.reference()
    }
}
