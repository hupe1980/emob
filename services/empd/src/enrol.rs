//! A writer reviews the CSMS's currently offered terms and issues one physical
//! card. Only the confirmed agreement is a fact. Stock never comes from the browser.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::http::StatusCode;
use emob_core::{ContractId, Emaid, EvseId, PartyId};
use emob_service::Actor;
use emob_service::estate::RetailOffer;
use emob_service::journal::Journal;
use emob_session::TokenRef;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::cards::label;
use crate::{Book, Contract, Empd, Event, Token, TokenType, Whitelist};

const MAX_OFFER_BYTES: usize = 65_536;
const MAX_SELECTIONS: usize = 256;

#[derive(Clone, Serialize)]
pub(crate) struct Selection {
    pub(crate) point: EvseId,
    provider: PartyId,
    pub(crate) plan: String,
    name: String,
}

pub(crate) struct Catalog {
    base: Option<reqwest::Url>,
    client: reqwest::Client,
    stock: BTreeMap<String, String>,
    selections: Vec<Selection>,
}

impl Catalog {
    pub(crate) fn configuration(
        estate: &crate::daemon::Estate,
    ) -> anyhow::Result<(Option<reqwest::Url>, Vec<Selection>)> {
        if estate.card_stock.is_empty() {
            return Ok((None, Vec::new()));
        }
        emob_service::estate::validate_card_stock(&estate.card_stock, &estate.contracts)?;
        let mut selections = Vec::new();
        for plan in estate
            .public_plans
            .iter()
            .filter(|p| p.provider == estate.parties.provider && p.uniform_public_terms)
        {
            for point in &plan.points {
                anyhow::ensure!(
                    selections.len() < MAX_SELECTIONS,
                    "too many native retail selections (maximum 256)"
                );
                selections.push(Selection {
                    point: point.clone(),
                    provider: plan.provider.clone(),
                    plan: plan.code.clone(),
                    name: plan.name.clone(),
                });
            }
        }
        let base = estate
            .desk
            .as_ref()
            .map(|desk| reqwest::Url::parse(&desk.csmsd))
            .transpose()?;
        anyhow::ensure!(
            base.as_ref().is_some_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.has_host()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            }),
            "native card stock requires the CSMS service address in desk.csmsd"
        );
        Ok((base, selections))
    }

    pub(crate) fn from_estate(estate: &crate::daemon::Estate) -> anyhow::Result<Self> {
        let (base, selections) = Self::configuration(estate)?;
        let stock = estate
            .card_stock
            .iter()
            .map(|c| (c.code.clone(), c.uid.clone()))
            .collect();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2))
            .no_proxy()
            .build()?;
        Ok(Self {
            base,
            client,
            stock,
            selections,
        })
    }

    pub(crate) fn options(&self) -> &[Selection] {
        &self.selections
    }

    pub(crate) fn stock_uid(&self, code: &str) -> Result<&str, StatusCode> {
        self.stock
            .get(code)
            .map(String::as_str)
            .ok_or(StatusCode::NOT_FOUND)
    }

    pub(crate) async fn current(
        &self,
        point: &EvseId,
        plan: &str,
        clock: &emob_service::Clock,
    ) -> Result<RetailOffer, StatusCode> {
        let selection = self
            .selections
            .iter()
            .find(|s| &s.point == point && s.plan == plan)
            .ok_or(StatusCode::NOT_FOUND)?;
        let mut url = self.base.clone().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        // Only configured, typed selectors reach the trusted owner's route.
        url.path_segments_mut()
            .map_err(|()| StatusCode::SERVICE_UNAVAILABLE)?
            .pop_if_empty()
            .extend([
                "v1",
                "csmsd",
                "retail-offers",
                &selection.point.to_string(),
                &selection.provider.to_string(),
                &selection.plan,
            ]);
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Err(StatusCode::NOT_FOUND),
            _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_OFFER_BYTES {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            body.extend_from_slice(&chunk);
        }
        let offer: RetailOffer =
            serde_json::from_slice(&body).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !offer.matches(point, &selection.provider, plan, clock()) {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(offer)
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Review {
    pub(crate) operation: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    card: String,
    pub(crate) offer: RetailOffer,
    #[serde(skip)]
    reviewed_at: OffsetDateTime,
    #[serde(skip)]
    pub(crate) stock: String,
    #[serde(skip)]
    started: std::time::Instant,
    #[serde(skip)]
    reference: TokenRef,
}

pub(crate) type Reviews = BTreeMap<Actor, Review>;

/// The operator's durable receipt for the card issued against these exact terms.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agreement {
    /// Provider-issued operation identity, searchable after a lost confirmation.
    pub agreement: String,
    /// Provider's contract identifier used at charge time.
    pub emaid: ContractId,
    /// Masked label staff can match against the card view.
    pub card: String,
    /// Keyed card reference. The journal carries the UID separately, sealed.
    pub reference: TokenRef,
    /// The exact archived offer accepted at confirmation.
    pub offer: RetailOffer,
    /// Authenticated operator who confirmed.
    pub actor: Actor,
    /// Instant the agreement was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

impl Agreement {
    pub(crate) fn covers(&self, evse: Option<&EvseId>, at: OffsetDateTime) -> bool {
        let terms = &self.offer.terms;
        evse == Some(&terms.point)
            && at >= self.at
            && terms.from() <= at
            && terms.until().is_none_or(|end| at < end)
    }

    pub(crate) fn binding(&self) -> RetailBinding {
        RetailBinding {
            point: self.offer.terms.point.clone(),
            provider: self.offer.terms.provider.clone(),
            plan: self.offer.terms.code.clone(),
            reference: self.offer.reference.clone(),
        }
    }
}

/// The station service checks this exact identity against its currently published offer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetailBinding {
    /// Authorized EVSE.
    pub point: EvseId,
    /// Owner of the contract.
    pub provider: PartyId,
    /// Public plan code.
    pub plan: String,
    /// Digest of the accepted terms.
    pub reference: String,
}

#[derive(Clone, Default, Debug)]
pub(crate) struct Agreements {
    receipts: BTreeMap<String, Arc<Agreement>>,
    by_card: BTreeMap<TokenRef, String>,
    consumed_stock: BTreeSet<String>,
    native_accounts: BTreeSet<Emaid>,
}

impl Agreements {
    pub(crate) fn holds_account(&self, contract: &ContractId) -> bool {
        Emaid::parse(contract.as_str()).is_ok_and(|id| self.native_accounts.contains(&id))
    }

    pub(crate) fn for_card(&self, reference: &TokenRef) -> Option<&Agreement> {
        self.by_card
            .get(reference)
            .and_then(|id| self.receipts.get(id))
            .map(Arc::as_ref)
    }
}

impl Empd {
    pub(crate) fn apply_agreement(&mut self, agreement: Agreement, held: Token) {
        if let Ok(id) = Emaid::parse(agreement.emaid.as_str()) {
            self.agreements.native_accounts.insert(id);
        }
        self.hold(Contract::new(agreement.emaid.clone(), agreement.at.date()));
        if let Some(stock) = &held.visual_digest {
            self.agreements.consumed_stock.insert(stock.clone());
        }
        self.store_card(&agreement.reference, held);
        self.agreements
            .by_card
            .insert(agreement.reference.clone(), agreement.agreement.clone());
        self.agreements
            .receipts
            .insert(agreement.agreement.clone(), Arc::new(agreement));
    }

    fn available_stock(&self, stock: &str, uid: &str) -> Result<TokenRef, StatusCode> {
        let reference = self.reference_for(uid);
        let digest = self.printed_digest(stock).ok_or(StatusCode::BAD_REQUEST)?;
        if self.foreign_uid_key {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        if self.token(&reference).is_some()
            || self.issued_rfids.contains(&reference)
            || self.has_printed_digest(&digest)
            || self.agreements.consumed_stock.contains(&digest)
        {
            return Err(StatusCode::CONFLICT);
        }
        Ok(reference)
    }
}

pub(crate) fn handle_valid(handle: &str) -> bool {
    handle.len() == 64
        && handle
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl<J: Journal> Book<J> {
    pub(crate) fn review_agreement(
        &mut self,
        actor: &Actor,
        stock: String,
        uid: &str,
        offer: RetailOffer,
        now: OffsetDateTime,
    ) -> Result<Review, StatusCode> {
        self.guard().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let card = self.state().available_stock(&stock, uid)?;
        let mut nonce = [0_u8; 32];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let review = Review {
            operation: hex::encode(nonce),
            expires_at: now + Duration::minutes(5),
            card: label(&card),
            reference: card,
            offer,
            reviewed_at: now,
            started: std::time::Instant::now(),
            stock,
        };
        self.enrol_reviews.insert(actor.clone(), review.clone());
        Ok(review)
    }

    pub(crate) fn receipt(&self, id: &str) -> Result<Option<Agreement>, StatusCode> {
        self.guard().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !handle_valid(id) {
            return Err(StatusCode::BAD_REQUEST);
        }
        Ok(self
            .state()
            .agreements
            .receipts
            .get(id)
            .map(|r| (**r).clone()))
    }

    pub(crate) fn own_receipt(
        &self,
        actor: &Actor,
        id: &str,
    ) -> Result<Option<Agreement>, StatusCode> {
        match self.receipt(id)? {
            Some(r) if &r.actor != actor => Err(StatusCode::CONFLICT),
            other => Ok(other),
        }
    }

    pub(crate) fn pending_agreement(
        &self,
        actor: &Actor,
        id: &str,
        now: OffsetDateTime,
    ) -> Result<&Review, StatusCode> {
        self.guard().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        self.enrol_reviews
            .get(actor)
            .filter(|p| {
                p.operation == id
                    && p.reviewed_at <= now
                    && now < p.expires_at
                    && p.started.elapsed() < std::time::Duration::from_mins(5)
            })
            .ok_or(StatusCode::CONFLICT)
    }

    pub(crate) fn enrol(
        &mut self,
        actor: &Actor,
        id: &str,
        uid: &str,
        current: &RetailOffer,
        now: OffsetDateTime,
    ) -> Result<Agreement, StatusCode> {
        if let Some(prior) = self.own_receipt(actor, id)? {
            return Ok(prior);
        }
        let review = self.pending_agreement(actor, id, now)?;
        let terms = &review.offer.terms;
        if !current.matches(&terms.point, self.state().party(), &terms.code, now)
            || review.offer.reference != current.reference
        {
            return Err(StatusCode::CONFLICT);
        }
        let reference = self.state().available_stock(&review.stock, uid)?;
        if reference != review.reference {
            return Err(StatusCode::CONFLICT);
        }
        let me = self.state().party();
        let body = format!("{}{}N{}", me.country_code(), me.party_id(), &id[..8]);
        let emaid = Emaid::parse(&body).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        // The eMAID grammar has a shorter instance than the operation handle.
        // Never replace an existing contract, even in a different supported spelling.
        if self.state().account_ids.contains(&emaid) {
            return Err(StatusCode::CONFLICT);
        }
        let emaid =
            ContractId::new(emaid.canonical()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let mut held = self
            .state()
            .prepare_card(&emaid, uid, TokenType::Rfid, Whitelist::Never)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        held.visual_digest = self.state().printed_digest(&review.stock);
        let agreement = Agreement {
            agreement: id.to_owned(),
            emaid,
            card: label(&reference),
            reference,
            offer: review.offer.clone(),
            actor: actor.clone(),
            at: now,
        };
        self.record_card_fact(
            actor,
            Event::Enrolled {
                agreement: Box::new(agreement.clone()),
                held: Box::new(held),
            },
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        self.enrol_reviews.remove(actor);
        Ok(agreement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_service::estate::{Estate, RetailTerms};
    use emob_service::journal::Memory;
    use emob_service::vault::DataKey;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-11-25 12:00 UTC);
    const UID: &str = "04AB0000000004";

    fn offer() -> RetailOffer {
        let estate: Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .unwrap();
        let plan = &estate.public_plans[0];
        let terms = RetailTerms {
            point: plan.points[0].clone(),
            provider: plan.provider.clone(),
            operator: estate.parties.operator,
            code: plan.code.clone(),
            name: plan.name.clone(),
            base: estate.tariffs[0].to_tariff(0).unwrap(),
            fees: plan.schedules[0].clone(),
            superseded: None,
        };
        RetailOffer {
            reference: terms.reference(),
            terms,
        }
    }

    fn book() -> Book<Memory> {
        Book::open(
            Empd::new("DE*MSP".parse().unwrap(), DataKey::from_bytes(&[7; 32])),
            Memory::new(),
        )
        .unwrap()
    }

    // native-driver-enrolment/SR-301, native-driver-enrolment/SR-303, native-driver-enrolment/SR-304.
    #[test]
    fn review_expires_belongs_to_one_writer_and_allocates_one_exact_card() {
        let mut book = book();
        let owner = Actor::operator("one").unwrap();
        let other = Actor::operator("two").unwrap();
        let offer = offer();
        let review_one = book
            .review_agreement(&owner, "STOCK-1".into(), UID, offer.clone(), NOW)
            .unwrap();
        let review_two = book
            .review_agreement(&other, "STOCK-1".into(), UID, offer.clone(), NOW)
            .unwrap();
        let denied = |result: Result<Agreement, StatusCode>| {
            assert_eq!(result.unwrap_err(), StatusCode::CONFLICT);
        };
        denied(book.enrol(&other, &review_one.operation, UID, &offer, NOW));
        denied(book.enrol(
            &owner,
            &review_one.operation,
            UID,
            &offer,
            NOW - Duration::seconds(1),
        ));
        denied(book.enrol(
            &owner,
            &review_one.operation,
            UID,
            &offer,
            NOW + Duration::minutes(5),
        ));
        denied(book.enrol(&owner, &review_one.operation, "04AB0000000005", &offer, NOW));
        let replacement = book
            .review_agreement(&owner, "STOCK-1".into(), UID, offer.clone(), NOW)
            .unwrap();
        denied(book.enrol(&owner, &review_one.operation, UID, &offer, NOW));
        let receipt = book
            .enrol(&other, &review_two.operation, UID, &offer, NOW)
            .unwrap();
        assert_eq!(
            book.state().presented_reference(&UID.to_ascii_lowercase()),
            receipt.reference
        );
        denied(book.enrol(&owner, &replacement.operation, UID, &offer, NOW));
        denied(book.enrol(&owner, &review_two.operation, UID, &offer, NOW));
        assert!(
            book.review_agreement(
                &owner,
                "STOCK-1".into(),
                "04AB0000000005",
                offer.clone(),
                NOW
            )
            .is_err()
        );
        assert!(
            book.state()
                .started_from_list(&receipt.reference, NOW.date())
                .is_err()
        );
        assert_eq!(
            book.state()
                .authorize(&receipt.reference, NOW.date())
                .unwrap(),
            crate::Allowed::NotAllowed
        );
        assert_eq!(
            book.state()
                .authorize_at(&receipt.reference, NOW, Some(&offer.terms.point))
                .unwrap(),
            crate::Allowed::Allowed
        );
        book.block(&owner, &receipt.reference).unwrap();
        assert_eq!(
            book.state()
                .authorize_at(&receipt.reference, NOW, Some(&offer.terms.point))
                .unwrap(),
            crate::Allowed::Blocked
        );
    }

    // native-driver-enrolment/SR-301. Historic mixed-case UID and printed label.
    #[test]
    fn native_stock_cannot_reuse_an_issued_seed_after_its_configuration_is_removed() {
        let mut book = book();
        let account = ContractId::new("DE-MSP-C00000001-W").unwrap();
        book.with(&Actor::Estate, Contract::new(account.clone(), NOW.date()))
            .unwrap();
        let prior = book
            .issue_token(
                &Actor::Estate,
                &account,
                UID.to_ascii_lowercase(),
                TokenType::Rfid,
                Whitelist::Allowed,
            )
            .unwrap();
        let label = book.state().printed_digest("STOCK-1");
        book.label(&Actor::Estate, &prior, label).unwrap();
        assert_eq!(
            book.state()
                .available_stock("STOCK-OTHER", UID)
                .unwrap_err(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            book.state()
                .available_stock("STOCK-1", "04AB0000000005")
                .unwrap_err(),
            StatusCode::CONFLICT
        );
    }
    // native-driver-enrolment/SR-304. An unavailable ledger cannot expose either half of issuance.
    #[test]
    fn a_failed_agreement_append_leaves_no_contract_card_or_receipt() {
        use emob_service::journal::{JournalError, Record};
        struct Unwritable;
        impl Journal for Unwritable {
            fn append(&self, _: &str, _: &Actor, _: &str) -> Result<Record, JournalError> {
                Err(JournalError::Io {
                    path: "test".into(),
                    doing: "appending",
                    source: std::io::Error::other("storage unavailable"),
                })
            }
            fn records(&self) -> Result<Vec<Record>, JournalError> {
                Ok(Vec::new())
            }
        }
        let mut book = Book::open(
            Empd::new("DE*MSP".parse().unwrap(), DataKey::from_bytes(&[7; 32])),
            Unwritable,
        )
        .unwrap();
        let actor = Actor::operator("issuer").unwrap();
        let offer = offer();
        let r = book
            .review_agreement(&actor, "STOCK-1".into(), UID, offer.clone(), NOW)
            .unwrap();
        assert_eq!(
            book.enrol(&actor, &r.operation, UID, &offer, NOW)
                .unwrap_err(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(book.is_halted());
        assert_eq!(book.state().contracts().count(), 0);
        assert_eq!(book.state().tokens().count(), 0);
        assert!(book.state().agreements.receipts.is_empty());
    }
}
