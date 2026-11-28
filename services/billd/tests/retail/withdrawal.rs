//! A case decision and an invoicing decision have separate owners and financial effects.
use super::{
    CustomerAccess, DUAL, FINANCE, FLEET, READER, Scratch, WRITER, call, cdr, deliver, env,
    fixture, json_call, june, provider_with_case, run,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

async fn post(
    owner: &billd::daemon::Running,
    route: &str,
    token: &str,
    input: &Value,
) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/billd/withdrawal/{route}", owner.http))
        .bearer_auth(token)
        .json(input)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(resp.headers()["cache-control"], "private, no-store");
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn issue(owner: &billd::daemon::Running) -> Value {
    let mut input = june();
    input["review"] = json_call(owner, "review", &input).await["review"].clone();
    json_call(owner, "close", &input).await
}

#[tokio::test]
async fn withdrawal_credit_requires_approved_case_and_replays_one_linked_document() {
    let (mut roster, offer) = fixture();
    roster["agreements"].as_array_mut().unwrap().truncate(1);
    let agreement = "a".repeat(64);
    let case = Arc::new(Mutex::new(
        json!({"agreement":agreement,"account":FLEET,"stage":"held","decision":null}),
    ));
    let url = provider_with_case(
        Arc::new(Mutex::new(roster)),
        CustomerAccess::default(),
        Arc::clone(&case),
    )
    .await;
    let dir = Scratch::new("withdrawal-credit");
    let keys = env();
    let owner = run(&dir, &url, Arc::clone(&keys)).await;
    assert_eq!(deliver(&owner, &offer, 1, true).await, StatusCode::OK);
    let invoice = issue(&owner).await;
    let mut selection = json!({"agreement":agreement,"number":invoice["number"]});
    let evidence = post(&owner, "evidence", WRITER, &selection).await;
    assert_eq!(evidence.0, StatusCode::OK);
    assert!(
        evidence.1["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["source"] == cdr(&offer, 1).key.to_string())
    );
    assert_eq!(
        post(&owner, "evidence", READER, &selection).await.0,
        StatusCode::FORBIDDEN
    );
    for token in [FINANCE, WRITER] {
        assert_eq!(
            post(&owner, "review", token, &selection).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        post(&owner, "review", DUAL, &selection).await.0,
        StatusCode::CONFLICT
    );
    *case.lock().unwrap() = json!({"agreement":agreement,"account":FLEET,"stage":"approved","decision":{"billing":"review_required","operation":"b".repeat(64)}});
    let (status, draft) = post(&owner, "review", DUAL, &selection).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(draft["state"], "ready");
    assert_eq!(draft["payable"], invoice["payable"]);
    selection["review"] = draft["review"].clone();
    case.lock().unwrap()["decision"]["operation"] = json!("c".repeat(64));
    assert_eq!(
        post(&owner, "confirm", DUAL, &selection).await.0,
        StatusCode::CONFLICT
    );
    case.lock().unwrap()["decision"]["operation"] = json!("b".repeat(64));
    let (one, two) = tokio::join!(
        post(&owner, "confirm", DUAL, &selection),
        post(&owner, "confirm", DUAL, &selection)
    );
    assert_eq!(one, two);
    assert_eq!(one.0, StatusCode::OK);
    assert_eq!(one.1["state"], "credited");
    let credit = &one.1["credit"];
    assert_ne!(credit, &invoice["number"]);
    for (number, kind) in [(&invoice["number"], "<Invoice "), (credit, "<CreditNote ")] {
        let mut request = june();
        request["number"] = number.clone();
        let xml = call(&owner, "document", FINANCE, &request)
            .await
            .text()
            .await
            .unwrap();
        assert!(xml.contains(kind), "{kind}");
        assert!(xml.contains(invoice["number"].as_str().unwrap()));
    }
    assert_eq!(json_call(&owner, "review", &june()).await["state"], "held");
    owner.kill().await;
    // The committed receipt is local, even when the provider is unreachable after restart.
    let restored = run(&dir, "http://127.0.0.1:1", keys).await;
    assert_eq!(post(&restored, "confirm", DUAL, &selection).await, one);
    let feed: Value = reqwest::Client::new()
        .get(format!("http://{}/v1/billd/receivables", restored.http))
        .bearer_auth(FINANCE)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(feed["documents"].as_array().unwrap().len(), 2);
    assert_eq!(feed["documents"][1]["kind"], "credit");
    assert_eq!(
        feed["documents"][1]["amount_minor"],
        feed["documents"][0]["amount_minor"]
    );
    assert_eq!(feed["documents"][1]["predecessor"], invoice["number"]);
    restored.kill().await;
}

#[tokio::test]
async fn withdrawal_evidence_excludes_other_drivers_and_holds_a_mixed_invoice() {
    let (roster, _) = fixture();
    let url = provider_with_case(
        Arc::new(Mutex::new(roster)),
        CustomerAccess::default(),
        Arc::new(Mutex::new(Value::Null)),
    )
    .await;
    let dir = Scratch::new("mixed-withdrawal");
    let owner = run(&dir, &url, env()).await;
    let document = issue(&owner).await;
    let input = json!({"number":document["number"],"agreement":"a".repeat(64)});
    let (_, view) = post(&owner, "review", DUAL, &input).await;
    assert_eq!(view["state"], "mixed");
    assert_eq!(view["lines"].as_array().unwrap().len(), 1);
    assert_eq!(view["lines"][0]["source"], input["agreement"]);
    assert_eq!(view["review"], Value::Null);
    let mut forged = input.clone();
    forged["review"] = json!("c".repeat(64));
    assert_eq!(
        post(&owner, "confirm", DUAL, &forged).await.0,
        StatusCode::CONFLICT
    );
    let mut wrong = input;
    wrong["agreement"] = json!("d".repeat(64));
    assert_eq!(
        post(&owner, "evidence", WRITER, &wrong).await.0,
        StatusCode::NOT_FOUND
    );
    owner.kill().await;
}
