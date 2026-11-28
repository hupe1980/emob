//! Evidence for customer-invoice-access/SR-343 against the billing owner's HTTP boundary.
use super::{
    CustomerAccess, FINANCE, FLEET, Scratch, WRITER, call, cdr, env, fixture, json_call, june,
    provider_with_customers, run, signed_delivery,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const DELEGATE: &str = "fleet-billing-session";
const DRIVER: &str = "vehicle-driver-session";
const OTHER: &str = "unrelated-customer-session";
const RECEIPT: &str = "http://127.0.0.1:9001/r/AAAAAAAAAAAAAAAAAAAAAA";

async fn customer(
    owner: &billd::daemon::Running,
    route: &str,
    session: &str,
    input: &Value,
) -> reqwest::Response {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/billd/customer/{route}", owner.http))
        .bearer_auth(session)
        .json(input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    response
}

fn grants(subject: &str, accounts: &[&str], agreements: &[Value]) -> Value {
    json!({"subject":subject, "accounts":accounts, "agreements":agreements})
}

async fn issue(owner: &billd::daemon::Running) -> Value {
    let mut req = june();
    req["review"] = json_call(owner, "review", &req).await["review"].clone();
    json_call(owner, "close", &req).await
}

async fn statement(
    owner: &billd::daemon::Running,
    offer: &emob_service::estate::RetailOffer,
    n: i64,
    receipt: &str,
) -> String {
    let cdr = cdr(offer, n);
    let key = cdr.key.to_string();
    let data = json!({
        "cdr":cdr, "token":{"issuer":offer.terms.provider,"contract_id":"driver-a"},
        "retail":{"agreement":"a".repeat(64), "reference":offer.reference}, "receipt":receipt
    });
    assert_eq!(signed_delivery(owner, &key, data).await, StatusCode::OK);
    key
}

#[tokio::test]
async fn each_archived_document_requires_a_current_account_grant() {
    let (roster, offer) = fixture();
    let access = CustomerAccess::default();
    {
        let mut users = access.lock().unwrap();
        users.insert(DELEGATE.into(), grants("finance", &[FLEET], &[]));
        users.insert(OTHER.into(), grants("private", &["PAYER-PRIVATE"], &[]));
    }
    let url = provider_with_customers(Arc::new(Mutex::new(roster)), Arc::clone(&access)).await;
    let dir = Scratch::new("customer-archive");
    let env = env();
    let owner = run(&dir, &url, Arc::clone(&env)).await;
    let original = issue(&owner).await;
    statement(&owner, &offer, 1, RECEIPT).await;
    let replacement = issue(&owner).await;
    let pages = customer(&owner, "history", DELEGATE, &json!({}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(pages["receipts"], json!([]));
    let documents = pages["invoices"].as_array().unwrap();
    assert_eq!(documents.len(), 3);
    let expected = [
        (
            &replacement["number"],
            "replacement",
            &replacement["payable"],
        ),
        (&replacement["credit"], "credit", &original["payable"]),
        (&original["number"], "invoice", &original["payable"]),
    ];
    for (row, (number, kind, payable)) in documents.iter().zip(expected) {
        assert_eq!(
            (
                &row["number"],
                row["kind"].as_str().unwrap(),
                &row["payable"]
            ),
            (number, kind, payable)
        );
        let req = json!({"number":number});
        let customer_xml = customer(&owner, "document", DELEGATE, &req)
            .await
            .bytes()
            .await
            .unwrap();
        let mut staff = june();
        staff["number"] = number.clone();
        assert_eq!(
            customer_xml,
            call(&owner, "document", FINANCE, &staff)
                .await
                .bytes()
                .await
                .unwrap()
        );
        assert!(customer_xml.starts_with(b"<?xml") || customer_xml.starts_with(b"<"));
        assert_eq!(
            customer(&owner, "document", OTHER, &req).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        customer(&owner, "document", OTHER, &json!({"number":"unknown"}))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        customer(&owner, "history", WRITER, &json!({}))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        customer(&owner, "history", "", &json!({})).await.status(),
        StatusCode::UNAUTHORIZED
    );
    access
        .lock()
        .unwrap()
        .insert(DELEGATE.into(), grants("finance", &[], &[]));
    let req = json!({"number":original["number"]});
    assert_eq!(
        customer(&owner, "document", DELEGATE, &req).await.status(),
        StatusCode::NOT_FOUND
    );
    access.lock().unwrap().insert(DELEGATE.into(), Value::Null);
    assert_eq!(
        customer(&owner, "document", DELEGATE, &req).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    owner.kill().await;
    let restored = run(&dir, &url, env).await;
    access
        .lock()
        .unwrap()
        .insert(DELEGATE.into(), grants("finance", &[FLEET], &[]));
    let history = customer(&restored, "history", DELEGATE, &json!({}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(history, pages);
    restored.kill().await;
}

#[tokio::test]
async fn only_the_assigned_agreement_discovers_its_verified_receipts() {
    let (profile, offer) = fixture();
    let agreement = json!({"agreement":"a".repeat(64), "emaid":"driver-a","reference":offer.reference,"point":offer.terms.point});
    let access = CustomerAccess::default();
    access.lock().unwrap().insert(
        DRIVER.into(),
        grants("driver", &[], std::slice::from_ref(&agreement)),
    );
    let url = provider_with_customers(Arc::new(Mutex::new(profile)), Arc::clone(&access)).await;
    let dir = Scratch::new("customer-receipt");
    let owner = run(&dir, &url, env()).await;
    let original = issue(&owner).await;
    let key = statement(&owner, &offer, 1, RECEIPT).await;
    statement(
        &owner,
        &offer,
        2,
        "https://another.example/r/AAAAAAAAAAAAAAAAAAAAAA",
    )
    .await;
    statement(
        &owner,
        &offer,
        3,
        "http://127.0.0.1:9001/r/AAAAAAAAAAAAAAAAAAAAAA?customer=x",
    )
    .await;
    let end = cdr(&offer, 1)
        .ended_at
        .to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let expected = json!([{"agreement":agreement["agreement"],"source":key,"receipt":RECEIPT,"point":offer.terms.point,"kwh":"10","ended_at":end}]);
    let list = customer(&owner, "history", DRIVER, &json!({}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(list["receipts"], expected);
    assert_eq!(list["invoices"], json!([]));
    assert_eq!(list["next_page"], Value::Null);
    assert_eq!(
        customer(
            &owner,
            "document",
            DRIVER,
            &json!({"number":original["number"]})
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    for field in ["emaid", "reference", "agreement", "point"] {
        let mut wrong = agreement.clone();
        wrong[field] = if field == "point" {
            json!("DE*AAA*E1000")
        } else {
            json!("somebody-else")
        };
        access
            .lock()
            .unwrap()
            .insert(DRIVER.into(), grants("driver", &[], &[wrong]));
        let history = customer(&owner, "history", DRIVER, &json!({}))
            .await
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(history["receipts"], json!([]), "{field}");
    }
    owner.kill().await;
}

#[tokio::test]
async fn customer_pages_are_bounded_and_the_receipt_index_replays() {
    let (profile, offer) = fixture();
    let agreement = json!({"agreement":"a".repeat(64), "emaid":"driver-a","reference":offer.reference,"point":offer.terms.point});
    let access = CustomerAccess::default();
    access
        .lock()
        .unwrap()
        .insert(DRIVER.into(), grants("driver", &[], &[agreement]));
    let url = provider_with_customers(Arc::new(Mutex::new(profile)), access).await;
    let dir = Scratch::new("customer-receipt-pages");
    let env = env();
    let owner = run(&dir, &url, Arc::clone(&env)).await;
    for n in 1..=27 {
        statement(&owner, &offer, n, RECEIPT).await;
    }
    owner.kill().await;
    let owner = run(&dir, &url, env).await;
    let first = customer(&owner, "history", DRIVER, &json!({}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    let second = customer(&owner, "history", DRIVER, &json!({"page":1}))
        .await
        .json::<Value>()
        .await
        .unwrap();
    let first_rows = first["receipts"].as_array().unwrap();
    let second_rows = second["receipts"].as_array().unwrap();
    assert_eq!(first_rows.len(), 25);
    assert_eq!(first["next_page"], 1);
    assert_eq!(second_rows.len(), 2);
    assert!(second["next_page"].is_null());
    let all: std::collections::BTreeSet<_> = first_rows
        .iter()
        .chain(second_rows)
        .map(|r| r["source"].as_str().unwrap())
        .collect();
    assert_eq!(all.len(), 27);
    assert_eq!(first_rows[0]["source"], cdr(&offer, 27).key.to_string());
    assert_eq!(second_rows[1]["source"], cdr(&offer, 1).key.to_string());
    owner.kill().await;
}
