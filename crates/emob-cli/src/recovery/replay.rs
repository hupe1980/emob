//! All decisions come from the daemon folds. The retry traverses billd's
//! signed HTTP handler in process, so no address from a backup is dialled.
use super::{Recovery, Report, Result, failed};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, atomic::AtomicU64};

use csmsd::journal::{Delivery, Target};
use emob_service::journal::Folded;
use emob_service::vault::{DataKey, Sealing};
use emob_service::{Actor, cloudevent, webhook};
use serde::{Deserialize, Serialize};
use tower::ServiceExt as _;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Expected {
    pub settled: String,
    pub energy_kwh: String,
    pub price: String,
    pub currency: String,
    pub refused_station: String,
    pub refused_tx: String,
    pub reasons: Vec<String>,
    pub invoice: String,
    pub invoice_total: String,
    pub partner: String,
    pub contract: String,
    pub allowed_uid: String,
    pub blocked_uid: String,
    pub publication: BTreeMap<String, String>,
    pub rate_ids: Vec<String>,
    pub owed: BTreeMap<String, usize>,
}

fn key(state: &Recovery, name: &str) -> Result<DataKey> {
    use base64::Engine as _;
    let value = state
        .keys
        .get(name)
        .ok_or_else(|| super::missing("key_missing"))?;
    let bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| failed("invalid_data_key"))?;
    Ok(DataKey::from_bytes(&bytes))
}

fn ok<T, E>(value: std::result::Result<T, E>) -> Result<T> {
    value.map_err(|_| failed("fold_refused"))
}

fn debts(
    state: &csmsd::journal::State,
    billing: &billd::Billd,
    has_receiver: bool,
    roaming: &roamd::Roamd,
) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::from([("billd", 0), ("roamd", 0), ("subscriber", 0)]);
    for delivery in state.pending() {
        let class = match delivery.to {
            Target::Billd => "billd",
            Target::Roamd => "roamd",
            Target::Subscriber(_) => "subscriber",
        };
        *counts.entry(class).or_default() += 1;
    }
    let mut invoice_receiver = 0;
    let mut invoice_subscribers = 0;
    for invoice in billing.documents() {
        invoice_receiver +=
            usize::from(has_receiver && invoice.submission == billd::Submission::Issued);
        invoice_subscribers += invoice.unannounced().count();
    }
    counts.insert("invoice_receiver", invoice_receiver);
    counts.insert("invoice_subscriber", invoice_subscribers);
    counts.insert("roaming_peer", roaming.pending().count());
    counts
}

fn fold<T>(
    report: &mut Report,
    name: &'static str,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let result = action();
    report.services.get_mut(name).expect("known service").replay =
        if result.is_ok() { "folded" } else { "refused" };
    result
}

pub(super) fn run(
    mut state: Recovery,
    config: &str,
    at: time::OffsetDateTime,
    e: &Expected,
    report: &mut Report,
) -> Result<()> {
    let estate: emob_service::estate::Estate = ok(emob_service::estate::from_str(config))?;
    let bill: billd::daemon::Estate = ok(emob_service::estate::from_str(config))?;
    let roam: roamd::daemon::Estate = ok(emob_service::estate::from_str(config))?;
    let zone = ok(emob_core::TimeZone::new(
        estate
            .desk
            .as_ref()
            .map_or("Europe/Berlin", |d| d.time_zone.as_str()),
    ))?;
    let mut csms = fold(report, "csmsd", || {
        let data = Arc::new(key(&state, "csmsd")?);
        ok(Folded::open(
            csmsd::journal::State::new(zone),
            Sealing::new(state.take("csmsd")?, data, "csmsd/journal"),
            csmsd::journal::apply,
        ))
    })?;
    let books = fold(report, "billd", || {
        let data = Arc::new(key(&state, "billd")?);
        ok(billd::Books::open(
            "emob",
            billd::Numbering::series(&bill.billing.series, at.year()),
            billd::ChartOfAccounts::new(),
            Sealing::new(state.take("billd")?, data, "billd/journal"),
        ))
    })?;
    let node = fold(report, "roamd", || {
        let data = Arc::new(key(&state, "roamd")?);
        ok(roamd::node::Node::open(
            roamd::Roamd::new(roamd::daemon::registry(&roam)),
            Sealing::new(state.take("roamd")?, Arc::clone(&data), "roamd/journal"),
            data,
        ))
    })?;
    let provider = fold(report, "empd", || {
        let data = key(&state, "empd")?;
        ok(empd::book::Book::open(
            empd::Empd::new(estate.parties.provider.clone(), data),
            state.take("empd")?,
        ))
    })?;
    let accepted = fold(report, "poid", || {
        ok(ok(poid::Ledger::open(state.take("poid")?))?.last_accepted())
    })?;
    let exact = csms
        .state()
        .cdrs
        .iter()
        .find(|c| c.key.to_string() == e.settled);
    report.compare(
        "settled",
        csms.state().cdrs.iter().count() == 1
            && exact.is_some_and(|cdr| {
                cdr.total_energy.kwh().to_string() == e.energy_kwh
                    && cdr.total_cost().is_some_and(|v| {
                        v.amount().to_string() == e.price && v.currency().as_str() == e.currency
                    })
                    && cdr
                        .evidence
                        .as_ref()
                        .is_some_and(|v| !v.payload_digests.is_empty())
            }),
    );
    let mut refusals = csms.state().outcomes.iter().filter_map(|o| match o {
        csmsd::Outcome::Refused {
            identity,
            transaction,
            reasons,
            ..
        } => Some((identity, transaction, reasons)),
        _ => None,
    });
    let refused = refusals
        .next()
        .is_some_and(|(station, transaction, reasons)| {
            station == &e.refused_station
                && transaction == &e.refused_tx
                && !reasons.is_empty()
                && reasons == &e.reasons
        })
        && refusals.next().is_none();
    let billed_refusal = books
        .state()
        .received()
        .any(|r| r.cdr.session_id.as_str() == e.refused_tx);
    report.compare("refused", refused && !billed_refusal);
    report.compare(
        "invoice",
        books.state().received_count() == 1
            && books.state().documents().count() == 1
            && books.state().issued(&e.invoice).is_some_and(|i| {
                i.invoice.records().len() == 1
                    && i.invoice.payable_total().amount().to_string() == e.invoice_total
                    && i.invoice.payable_total().currency().as_str() == e.currency
                    && i.invoice
                        .records()
                        .iter()
                        .any(|key| key.to_string() == e.settled)
            }),
    );
    report.compare(
        "cards",
        e.allowed_uid != e.blocked_uid
            && [
                (&e.allowed_uid, empd::Allowed::Allowed),
                (&e.blocked_uid, empd::Allowed::Blocked),
            ]
            .into_iter()
            .all(|(uid, decision)| {
                let reference = provider.state().reference_for(uid);
                provider
                    .authorize(&reference, at.date())
                    .is_ok_and(|x| x == decision)
                    && provider.state().present(&reference).is_ok_and(|t| {
                        t.uid == *uid
                            && t.contract_id.as_str() == e.contract
                            && t.issuer.to_string() == e.partner
                    })
            }),
    );
    report.compare(
        "roaming",
        node.state().consignments().any(|c| {
            c.key.to_string() == e.settled
                && c.delivery.is_accepted()
                && c.recipient().to_string() == e.partner
        }),
    );
    let publication = serde_json::json!({"facilities":e.publication,"rates":e.rate_ids});
    report.compare(
        "publication",
        !e.publication.is_empty()
            && accepted.is_some_and(|a| {
                serde_json::to_value(a.published).ok().as_ref() == Some(&publication)
            }),
    );
    let has_invoice_receiver = bill.billing.delivery.is_some();
    report.owed_before = debts(
        csms.state(),
        books.state(),
        has_invoice_receiver,
        node.state(),
    );
    let expected_counts: BTreeMap<&str, usize> =
        e.owed.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    if expected_counts != report.owed_before || report.owed_before["billd"] == 0 {
        return Err(failed("owed_mismatch"));
    }
    if report.exit() != 0 {
        return Ok(());
    }
    let delivery = csms
        .state()
        .pending()
        .find(|d| d.to == Target::Billd && d.id == e.settled)
        .cloned()
        .ok_or_else(|| failed("debt_missing"))?;
    let cached = books
        .state()
        .received()
        .find(|r| r.cdr.key.to_string() == e.settled)
        .ok_or_else(|| failed("receiver_precommit_missing"))?;
    let envelope = ok(cloudevent::open::<billd::receive::Body>(
        delivery.body.as_bytes(),
        emob_service::events::cdr::ISSUED,
    ))?;
    if envelope.id != e.settled
        || envelope.source != cloudevent::source("csmsd")
        || envelope.data.cdr != cached.cdr
        || envelope.data.token.issuer != cached.partner
    {
        return Err(failed("delivery_identity_mismatch"));
    }
    let count = books.state().received_count();
    let books = Arc::new(Mutex::new(books));
    let mut secret_bytes = [0u8; 32];
    getrandom::fill(&mut secret_bytes).map_err(|_| super::missing("random_unavailable"))?;
    let secret = webhook::Secret::raw(secret_bytes);
    let app = billd::receive::delivery_router(billd::receive::Shared {
        books: Arc::clone(&books),
        secret: secret.clone(),
        partners: bill.partners.iter().map(|p| p.party.clone()).collect(),
        refused: Arc::new(AtomicU64::new(0)),
    });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|_| super::missing("runtime_unavailable"))?;
    rt.block_on(retry(app.clone(), &secret, &delivery))?;
    ok(csms.record(
        &Actor::service("billd"),
        csmsd::journal::Event::Delivered {
            id: delivery.id.clone(),
            to: Target::Billd,
            at,
        },
    ))?;
    rt.block_on(retry(app, &secret, &delivery))?;
    let guard = books.lock().map_err(|_| failed("receiver_refused"))?;
    let mut expected_after = report.owed_before.clone();
    *expected_after.get_mut("billd").expect("known target") -= 1;
    report.owed_after = debts(
        csms.state(),
        guard.state(),
        has_invoice_receiver,
        node.state(),
    );
    if guard.state().received_count() != count
        || expected_after != report.owed_after
        || csms
            .state()
            .pending()
            .any(|d| d.id == delivery.id && d.to == Target::Billd)
    {
        return Err(failed("retry_changed_effect"));
    }
    report.retry = "one_effect";
    Ok(())
}

async fn retry(app: axum::Router, secret: &webhook::Secret, delivery: &Delivery) -> Result<()> {
    use axum::{body::Body, http::Request};
    let at = time::OffsetDateTime::now_utc();
    let attempt = webhook::Delivery::new(&delivery.id, at, delivery.body.as_bytes());
    let req = ok(Request::post("/v1/deliveries")
        .header(webhook::ID_HEADER, &delivery.id)
        .header(webhook::TIMESTAMP_HEADER, at.unix_timestamp().to_string())
        .header(webhook::SIGNATURE_HEADER, webhook::sign(&attempt, secret))
        .body(Body::from(delivery.body.clone())))?;
    let response = app.oneshot(req).await.expect("axum router is infallible");
    if !response.status().is_success() {
        return Err(failed("retry_refused"));
    }
    let bytes = ok(axum::body::to_bytes(response.into_body(), 1024).await)?;
    if ok(serde_json::from_slice::<serde_json::Value>(&bytes))?
        != serde_json::json!({"duplicate":true})
    {
        return Err(failed("unexpected_receiver_effect"));
    }
    Ok(())
}
