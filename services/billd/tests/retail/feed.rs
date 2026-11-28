//! receivables-allocation/SR-351. Only finance can retrieve booked amounts from the signed billing history.
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use billd::daemon::Running;
use serde_json::{Value, json};

use super::{
    FINANCE, FLEET, READER, Scratch, WRITER, deliver, env, fixture, json_call, june, provider, run,
};

async fn feed(owner: &Running, query: &str, token: &str) -> reqwest::Response {
    let response = reqwest::Client::new()
        .get(format!(
            "http://{}/v1/billd/receivables?{query}",
            owner.http
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    response
}

async fn page(owner: &Running, query: &str) -> Value {
    let response = feed(owner, query, FINANCE).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn issue(owner: &Running) -> Value {
    let mut body = june();
    body["review"] = json_call(owner, "review", &body).await["review"].clone();
    let document = json_call(owner, "close", &body).await;
    assert_eq!(json_call(owner, "close", &body).await, document);
    document
}

#[tokio::test]
async fn only_finance_can_page_issued_documents_and_replay_the_same_positions() {
    let (roster, offer) = fixture();
    let url = provider(Arc::new(Mutex::new(roster))).await;
    let directory = Scratch::new("receivables-feed");
    let keys = env();
    let owner = run(&directory, &url, Arc::clone(&keys)).await;
    for credential in [WRITER, READER] {
        assert_eq!(
            feed(&owner, "", credential).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        feed(&owner, "", "customer-session").await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        page(&owner, "").await,
        json!({"documents":[], "next_after":0, "has_more":false})
    );
    let first = issue(&owner).await;
    assert_eq!(deliver(&owner, &offer, 1, true).await, StatusCode::OK);
    let second = issue(&owner).await;
    let rows = page(&owner, "after=0&limit=2").await;
    let invoice = json!({
        "issuer":offer.terms.provider, "position":1, "account":FLEET, "number":first["number"],
        "currency":"EUR", "amount_minor":2380, "kind":"invoice", "predecessor":null
    });
    let credit = json!({
        "issuer":offer.terms.provider, "position":2, "account":FLEET, "number":second["credit"],
        "currency":"EUR", "amount_minor":2380, "kind":"credit", "predecessor":first["number"]
    });
    let replacement = json!({
        "issuer":offer.terms.provider, "position":3, "account":FLEET, "number":second["number"],
        "currency":"EUR", "amount_minor":3010, "kind":"replacement", "predecessor":first["number"]
    });
    assert_eq!(
        rows,
        json!({"documents":[invoice, credit], "next_after":2,"has_more":true})
    );
    let end = page(&owner, "after=2&limit=1").await;
    assert_eq!(
        end,
        json!({"documents":[replacement], "next_after":3,"has_more":false})
    );
    assert_eq!(
        page(&owner, "after=3").await,
        json!({"documents":[], "next_after":3,"has_more":false})
    );
    assert_eq!(
        feed(&owner, "after=4", FINANCE).await.status(),
        StatusCode::CONFLICT
    );
    for bad in ["limit=0", "limit=101", "after=-1", "account=PAYER-PRIVATE"] {
        assert_eq!(
            feed(&owner, bad, FINANCE).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    owner.kill().await;
    let replay = run(&directory, &url, keys).await;
    assert_eq!(page(&replay, "limit=2").await, rows);
    assert_eq!(page(&replay, "after=2").await, end);
    replay.kill().await;
}
