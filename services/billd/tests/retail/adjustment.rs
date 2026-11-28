//! A single fleet line is credited once and is still attributable after replay.
use super::{
    CustomerAccess, DUAL, FINANCE, FLEET, READER, SECOND_FINANCE_WRITER, Scratch, WRITER, call,
    deliver, env, fixture, json_call, june, provider_with_customers, run,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

async fn post(
    owner: &billd::daemon::Running,
    route: &str,
    token: &str,
    data: &Value,
) -> reqwest::Response {
    let reply = reqwest::Client::new()
        .post(format!("http://{}/v1/billd/{route}", owner.http))
        .bearer_auth(token)
        .json(data)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.headers()["cache-control"], "private, no-store");
    reply
}

async fn ok(owner: &billd::daemon::Running, route: &str, token: &str, data: &Value) -> Value {
    let reply = post(owner, route, token, data).await;
    assert_eq!(reply.status(), StatusCode::OK, "{route}");
    reply.json().await.unwrap()
}

#[tokio::test]
async fn a_partial_fleet_credit_retries_once_and_preserves_the_other_line_and_cash_basis() {
    let (roster, offer) = fixture();
    let access = CustomerAccess::default();
    let driver = "a".repeat(64);
    let other = "b".repeat(64);
    {
        let mut users = access.lock().unwrap();
        users.insert(
            "payer".into(),
            json!({"subject":"payer","accounts":[FLEET],"agreements":[]}),
        );
        users.insert(
            "driver".into(),
            json!({"subject":"driver","accounts":[],"agreements":[{"agreement":driver,"emaid":"driver-a","reference":offer.reference,"point":offer.terms.point}]}),
        );
    }
    let url = provider_with_customers(Arc::new(Mutex::new(roster)), access.clone()).await;
    let scratch = Scratch::new("partial-fleet-line");
    let keys = env();
    let owner = run(&scratch, &url, keys.clone()).await;
    let mut monthly = june();
    monthly["review"] = json_call(&owner, "review", &monthly).await["review"].clone();
    let issued = json_call(&owner, "close", &monthly).await;
    let invoice = &issued["number"];
    let original = json!({"number":invoice});
    let xml = post(&owner, "customer/document", "payer", &original)
        .await
        .text()
        .await
        .unwrap();
    let scope = json!({"number":invoice,"agreement":driver});
    for wrong in [FINANCE, WRITER, READER] {
        assert_eq!(
            post(&owner, "adjustment/lookup", wrong, &scope)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        post(&owner, "customer/balance-basis", "driver", &original)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let scoped = ok(&owner, "adjustment/lookup", DUAL, &scope).await;
    assert_eq!(scoped["lines"].as_array().unwrap().len(), 1);
    let id = &scoped["lines"][0]["line_id"];
    let mut req = json!({"number":invoice,"agreement":driver,"line_id":id,"net_minor":250,"reason":"billing_error"});
    for wrong in [WRITER, FINANCE] {
        assert_eq!(
            post(&owner, "adjustment/review", wrong, &req)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let mut forged = req.clone();
    forged["net_minor"] = json!(1001);
    assert_eq!(
        post(&owner, "adjustment/review", DUAL, &forged)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    forged = req.clone();
    forged["agreement"] = json!(other);
    assert_eq!(
        post(&owner, "adjustment/review", DUAL, &forged)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let preview = ok(&owner, "adjustment/review", DUAL, &req).await;
    assert_eq!(preview["gross_minor"], 298);
    assert_eq!(preview["tax_minor"], 48);
    assert_eq!(preview["remaining_minor"], 750);
    let mut competing = req.clone();
    competing["net_minor"] = json!(200);
    competing["review"] = ok(&owner, "adjustment/review", DUAL, &competing).await["review"].clone();
    req["review"] = preview["review"].clone();
    assert_eq!(
        post(&owner, "adjustment/confirm", SECOND_FINANCE_WRITER, &req)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let (a, b) = tokio::join!(
        ok(&owner, "adjustment/confirm", DUAL, &req),
        ok(&owner, "adjustment/confirm", DUAL, &req)
    );
    assert_eq!(a, b);
    assert_eq!(
        post(&owner, "adjustment/confirm", SECOND_FINANCE_WRITER, &req)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(a["state"], "issued");
    assert_eq!(
        post(&owner, "adjustment/confirm", DUAL, &competing)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    forged = req.clone();
    forged["reason"] = json!("goodwill");
    assert_eq!(
        post(&owner, "adjustment/confirm", DUAL, &forged)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let credit = json!({"number":a["credit"]});
    let credit_xml = post(&owner, "customer/document", "payer", &credit)
        .await
        .text()
        .await
        .unwrap();
    assert!(credit_xml.contains("<CreditNote ") && credit_xml.contains(invoice.as_str().unwrap()));
    assert!(credit_xml.contains("2.98") && !credit_xml.contains("driver-b"));
    let basis = ok(&owner, "customer/balance-basis", "payer", &original).await;
    assert_eq!(basis["credited_minor"], 298);
    assert_eq!(basis["amount_minor"], 2380);
    assert_eq!(
        post(&owner, "customer/balance-basis", "payer", &credit)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&owner, "customer/balance-basis", "driver", &credit)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let second = ok(
        &owner,
        "adjustment/lookup",
        DUAL,
        &json!({"number":invoice,"agreement":other}),
    )
    .await;
    assert_eq!(second["lines"][0]["remaining_minor"], 1000);
    assert_eq!(second["lines"][0]["last_credit"], Value::Null);
    assert_eq!(
        post(&owner, "withdrawal/evidence", DUAL, &scope)
            .await
            .json::<Value>()
            .await
            .unwrap()["state"],
        "line_adjusted"
    );
    assert_eq!(deliver(&owner, &offer, 1, true).await, StatusCode::OK);
    let changed = json_call(&owner, "review", &june()).await;
    assert_eq!(changed["state"], "held");
    assert!(
        changed["withheld"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["reason"] == "line_credit_issued")
    );
    owner.kill().await;
    let restored = run(&scratch, &url, keys).await;
    assert_eq!(ok(&restored, "adjustment/confirm", DUAL, &req).await, a);
    assert_eq!(
        ok(&restored, "customer/balance-basis", "payer", &original).await,
        basis
    );
    assert_eq!(
        ok(&restored, "adjustment/lookup", DUAL, &scope).await["lines"][0]["last_credit"],
        a["credit"]
    );
    assert_eq!(
        post(&restored, "customer/document", "payer", &original)
            .await
            .text()
            .await
            .unwrap(),
        xml
    );
    let archived = call(
        &restored,
        "document",
        FINANCE,
        &json!({"account":FLEET,"period":"2026-06","number":a["credit"]}),
    )
    .await;
    assert_eq!(archived.text().await.unwrap(), credit_xml);
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
    let row = &feed["documents"][1];
    assert_eq!(row["kind"], "line_credit");
    assert_eq!(row["amount_minor"], 298);
    assert_eq!(row["predecessor"], *invoice);
    access.lock().unwrap().get_mut("payer").unwrap()["accounts"] = json!([]);
    assert_eq!(
        post(&restored, "customer/balance-basis", "payer", &original)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&restored, "customer/document", "payer", &credit)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    restored.kill().await;
}
