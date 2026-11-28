//! Customer sign-in and explicit, journalled access to agreements or billing accounts.
pub(crate) mod erasure;
mod identity;
pub(crate) use identity::Identity;

use std::collections::BTreeMap;
use std::time::Instant;

use axum::extract::{DefaultBodyLimit, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::{Extension, Json, Router, routing::post};
use emob_service::journal::Journal;
use emob_service::vault::{DataKey, Digest, Sealed};
use emob_service::{Actor, Operators};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::http::{Shared, lock, payload};
use crate::{Book, Event};

const PURPOSE: &str = "empd/customer-grant/v2";
const MAX_GRANTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Agreement,
    Account,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    subject: String,
    kind: Kind,
    reference: String,
    granted: bool,
    revision: u64,
}

#[derive(Serialize, Deserialize)]
struct Change {
    subject: String,
    kind: Kind,
    reference: String,
    granted: bool,
    profile: Option<Digest>,
}

#[derive(Clone, Default)]
struct Assignment {
    revision: u64,
    targets: BTreeMap<(Kind, String), Option<Digest>>,
}

#[derive(Default, Clone)]
pub(crate) struct Grants {
    subjects: BTreeMap<String, Assignment>,
    witnessed: std::collections::BTreeSet<String>,
    unreadable: bool,
}

impl core::fmt::Debug for Grants {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Grants").finish_non_exhaustive()
    }
}

impl Grants {
    pub(crate) const fn readable(&self) -> bool {
        !self.unreadable
    }

    pub(crate) fn reject_custody(&mut self) {
        self.unreadable = true;
    }

    pub(crate) fn forget(&mut self, subject: &str) {
        self.subjects.remove(subject);
    }

    fn apply(&mut self, master: &DataKey, tag: &str, key: &DataKey, sealed: &Sealed) {
        let change = key
            .open(PURPOSE, sealed)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Change>(&bytes).ok());
        let Some(change) = change else {
            self.unreadable = true;
            return;
        };
        if subject_tag(master, &change.subject) != tag {
            self.unreadable = true;
            return;
        }
        let assignment = self.subjects.entry(change.subject).or_default();
        let target = (change.kind, change.reference);
        if change.granted {
            assignment.targets.insert(target, change.profile);
        } else {
            assignment.targets.remove(&target);
        }
        assignment.revision += 1;
    }

    fn view(&self, subject: String) -> Access {
        let mut access = Access {
            subject,
            accounts: Vec::new(),
            agreements: Vec::new(),
            revision: 0,
        };
        if let Some(assignment) = self.subjects.get(&access.subject) {
            access.revision = assignment.revision;
            for (kind, id) in assignment.targets.keys() {
                match kind {
                    Kind::Account => &mut access.accounts,
                    Kind::Agreement => &mut access.agreements,
                }
                .push(id.clone());
            }
        }
        access
    }
}

#[derive(Serialize)]
struct Access {
    subject: String,
    accounts: Vec<String>,
    agreements: Vec<String>,
    revision: u64,
}

#[derive(Serialize)]
struct CustomerView {
    subject: String,
    accounts: Vec<String>,
    agreements: Vec<AgreementView>,
}

#[derive(Serialize)]
struct AgreementView {
    agreement: String,
    emaid: emob_core::ContractId,
    point: emob_core::EvseId,
    plan: String,
    reference: String,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    withdrawal_status: Option<crate::withdrawal::Stage>,
}

fn subject_tag(master: &DataKey, subject: &str) -> String {
    master
        .digest("empd/customer-subject/v1", subject.as_bytes())
        .hex()
        .to_owned()
}

impl crate::Empd {
    pub(crate) fn apply_customer_grant(&mut self, tag: &str, sealed: &Sealed) {
        match self.custody.key(tag) {
            Ok(Some(key)) => self.grants.apply(&self.key, tag, &key, sealed),
            Ok(None) => {} // Current custody carries a durable erasure receipt.
            Err(_) => self.grants.unreadable = true,
        }
    }

    pub(crate) fn apply_erasure_witness(&mut self, tag: String, operation: &str) {
        match self.custody.receipt(&tag) {
            Ok(Some(receipt)) if receipt.operation == operation => {
                self.grants.witnessed.insert(tag);
            }
            _ => self.grants.reject_custody(),
        }
    }

    pub(crate) fn is_customer_erased(&self, subject: &str) -> anyhow::Result<bool> {
        Ok(self
            .custody
            .receipt(&subject_tag(&self.key, subject))?
            .is_some())
    }
}

impl<J: Journal> Book<J> {
    fn customer_ready(&self) -> Result<(), StatusCode> {
        if self.is_halted() || !self.state().grants.readable() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(())
    }

    fn customer_active(&self, subject: &str, erased: StatusCode) -> Result<(), StatusCode> {
        self.customer_ready()?;
        if self
            .state()
            .is_customer_erased(subject)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        {
            return Err(erased);
        }
        Ok(())
    }

    fn assign(
        &mut self,
        catalog: &crate::enrol::Catalog,
        actor: &Actor,
        grant: Grant,
    ) -> Result<Access, StatusCode> {
        self.customer_active(&grant.subject, StatusCode::NOT_FOUND)?;
        let grants = &self.state().grants;
        let current = grants.subjects.get(&grant.subject);
        let target = (grant.kind, grant.reference);
        let existing = current.and_then(|a| a.targets.get(&target));
        let profile = match (grant.granted, target.0) {
            (true, Kind::Account) => Some(self.profile_digest(catalog.account(&target.1)?)?),
            (true, Kind::Agreement) => {
                self.receipt(&target.1)?.ok_or(StatusCode::NOT_FOUND)?;
                None
            }
            (false, _) => None,
        };
        let rev = current.map_or(0, |a| a.revision);
        if grant.revision > rev {
            return Err(StatusCode::CONFLICT);
        }
        if (grant.granted && existing == Some(&profile)) || (!grant.granted && existing.is_none()) {
            return Ok(grants.view(grant.subject));
        }
        if grant.revision != rev
            || (grant.granted
                && existing.is_none()
                && current.is_some_and(|a| a.targets.len() >= MAX_GRANTS))
        {
            return Err(StatusCode::CONFLICT);
        }
        let change = Change {
            subject: grant.subject.clone(),
            kind: target.0,
            reference: target.1,
            granted: grant.granted,
            profile,
        };
        let bytes = serde_json::to_vec(&change).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let tag = subject_tag(&self.state().key, &grant.subject);
        let key = self
            .state()
            .custody
            .ensure_key(&tag)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let sealed = key.seal(PURPOSE, &bytes);
        self.record_fact(
            actor,
            Event::CustomerGrant {
                subject_tag: tag,
                change: sealed,
            },
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        self.customer_ready()?;
        Ok(self.state().grants.view(grant.subject))
    }

    fn customer_view(
        &self,
        catalog: &crate::enrol::Catalog,
        subject: String,
    ) -> Result<CustomerView, StatusCode> {
        self.customer_active(&subject, StatusCode::UNAUTHORIZED)?;
        let mut view = CustomerView {
            subject,
            accounts: Vec::new(),
            agreements: Vec::new(),
        };
        let Some(assignment) = self.state().grants.subjects.get(&view.subject) else {
            return Ok(view);
        };
        for ((kind, id), pin) in &assignment.targets {
            match kind {
                Kind::Account => {
                    if let Ok(account) = catalog.account(id)
                        && self
                            .profile_digest(account)
                            .is_ok_and(|digest| Some(&digest) == pin.as_ref())
                    {
                        view.accounts.push(id.clone());
                    }
                }
                Kind::Agreement => {
                    let agreement = self.receipt(id)?.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
                    view.agreements.push(AgreementView {
                        agreement: id.clone(),
                        emaid: agreement.emaid,
                        point: agreement.offer.terms.point,
                        plan: agreement.offer.terms.code,
                        reference: agreement.offer.reference,
                        ended_at: self.state().ended_at(id),
                        withdrawal_status: self.state().withdrawals.stage(id),
                    });
                }
            }
        }
        Ok(view)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    subject: String,
    secret: String,
}
#[derive(Serialize)]
struct Authenticated {
    session: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Subject {
    subject: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn bearer(headers: &HeaderMap) -> Result<&str, StatusCode> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| crate::enrol::handle_valid(s))
        .ok_or(StatusCode::UNAUTHORIZED)
}

async fn login<J: Journal>(
    State(shared): State<Shared<J>>,
    input: Result<Json<Login>, JsonRejection>,
) -> Result<Json<Authenticated>, StatusCode> {
    let input = payload(input)?;
    let book = lock(&shared.book);
    book.customer_ready()?;
    let mut customers = lock(&shared.customers);
    if book
        .state()
        .is_customer_erased(&input.subject)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    {
        customers.forget(&input.subject);
    }
    let session = customers.login(&input.subject, &input.secret, Instant::now())?;
    Ok(Json(Authenticated { session }))
}

async fn me<J: Journal>(
    State(shared): State<Shared<J>>,
    headers: HeaderMap,
    input: Result<Json<Empty>, JsonRejection>,
) -> Result<Json<CustomerView>, StatusCode> {
    payload(input)?;
    let subject = lock(&shared.customers).subject(bearer(&headers)?, Instant::now())?;
    lock(&shared.book)
        .customer_view(&shared.catalog, subject)
        .map(Json)
}

async fn logout<J: Journal>(
    State(shared): State<Shared<J>>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    lock(&shared.customers).logout(bearer(&headers)?, Instant::now())?;
    Ok(StatusCode::NO_CONTENT)
}

async fn access<J: Journal>(
    State(shared): State<Shared<J>>,
    input: Result<Json<Subject>, JsonRejection>,
) -> Result<Json<Access>, StatusCode> {
    let input = payload(input)?;
    let configured = lock(&shared.customers).contains(&input.subject);
    let book = lock(&shared.book);
    book.customer_active(&input.subject, StatusCode::NOT_FOUND)?;
    if !configured && !book.state().grants.subjects.contains_key(&input.subject) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(book.state().grants.view(input.subject)))
}

async fn grant<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<Actor>,
    input: Result<Json<Grant>, JsonRejection>,
) -> Result<Json<Access>, StatusCode> {
    let input = payload(input)?;
    let configured = lock(&shared.customers).contains(&input.subject);
    let mut book = lock(&shared.book);
    if !configured && (input.granted || !book.state().grants.subjects.contains_key(&input.subject))
    {
        return Err(StatusCode::NOT_FOUND);
    }
    book.assign(&shared.catalog, &actor, input).map(Json)
}

pub(crate) fn routes<J: Journal>(shared: Shared<J>, operators: Operators) -> Router {
    let staff = Router::new()
        .route("/v1/empd/customer/grants", post(grant::<J>))
        .route("/v1/empd/customer/access", post(access::<J>))
        .with_state(shared.clone());
    Router::new()
        .route("/v1/empd/customer/login", post(login::<J>))
        .route("/v1/empd/customer/me", post(me::<J>))
        .route("/v1/empd/customer/logout", post(logout::<J>))
        .with_state(shared.clone())
        .merge(erasure::routes(shared, operators.clone()))
        .merge(emob_service::serve::operator_write(staff, operators))
        .layer(DefaultBodyLimit::max(2048))
}
