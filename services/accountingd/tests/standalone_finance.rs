//! Drive the sealed accounting owner over HTTP, including retries after a process restart.
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use accountingd::{Estate, Options};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use emob_service::{Identity, Shutdown, estate::Env, head::SigningKey, vault::DataKey};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[derive(Default)]
struct Feed {
    rows: Mutex<Vec<Value>>,
    offline: std::sync::atomic::AtomicBool,
}
#[derive(Deserialize)]
struct Cursor {
    after: usize,
}

async fn documents(
    State(feed): State<Arc<Feed>>,
    headers: HeaderMap,
    Query(cursor): Query<Cursor>,
) -> Result<Json<Value>, StatusCode> {
    if !matches!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer finance-secret" | "Bearer writer-secret")
    ) {
        return Err(StatusCode::FORBIDDEN);
    }
    if feed.offline.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let data = feed.rows.lock().unwrap();
    if cursor.after > data.len() {
        return Err(StatusCode::CONFLICT);
    }
    let page: Vec<_> = data.iter().skip(cursor.after).take(100).collect();
    let next = cursor.after + page.len();
    Ok(Json(
        json!({"documents":page,"next_after":next,"has_more":next<data.len()}),
    ))
}

fn doc(position: u64, number: &str, amount: i64) -> Value {
    json!({"position":position,"number":number,"issuer":"DE*MSP","account":"PAYER","currency":"EUR","kind":"invoice","amount_minor":amount,"predecessor":null})
}

struct Fixture {
    estate: Estate,
    env: Env,
    dir: PathBuf,
    feed: Arc<Feed>,
    feeder: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        static ID: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "accountingd-test-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let feed = Arc::new(Feed::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/billd/receivables", get(documents))
            .with_state(feed.clone());
        let feeder = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let estate=serde_json::from_value(json!({
            "parties":{"provider":"DE*MSP","operator":"DE*CPO"},
            "services":{"billd":origin,"empd":origin,"roamd":origin,"webhook_secret_env":"WEBHOOK","service_token_env":"SVC"},
            "operators":[{"name":"clerk","finance":true,"token_env":"FINANCE"},{"name":"general","write":true,"token_env":"GENERAL"},{"name":"finance writer","write":true,"finance":true,"token_env":"WRITER"}],
            "retail_accounts":[{"id":"PAYER","kind":"private","name":"Payer","street":"1 Street","post_code":"12345","city":"City","country":"DE"}]
        })).unwrap();
        let key = DataKey::generate();
        let sign = SigningKey::generate();
        let env: Env = Arc::new(move |var| match var {
            "FINANCE" => Some("finance-secret".into()),
            "GENERAL" => Some("ordinary-secret".into()),
            "WRITER" => Some("writer-secret".into()),
            "ACCOUNTINGD_DATA_KEY" => Some(key.clone()),
            "ACCOUNTINGD_SIGNING_KEY" => Some(sign.clone()),
            _ => None,
        });
        Self {
            estate,
            env,
            dir,
            feed,
            feeder,
        }
    }
    async fn start(&self) -> accountingd::Running {
        accountingd::run(Options {
            estate: self.estate.clone(),
            env: self.env.clone(),
            http: "127.0.0.1:0".parse().unwrap(),
            journal: self.dir.join("accountingd.ndjson"),
            shutdown: Shutdown::new(),
            identity: Identity::new("accountingd", "test"),
        })
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.feeder.abort();
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

async fn post(
    server: &accountingd::Running,
    name: &str,
    token: &str,
    body: &Value,
) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/accountingd/{name}", server.http))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[tokio::test]
async fn finance_confirmation_is_once_after_concurrent_submission_and_restart() {
    // receivables-allocation/SR-353, receivables-allocation/SR-355
    let f = Fixture::new().await;
    f.feed.rows.lock().unwrap().push(doc(1, "INV-A", 10000));
    let server = f.start().await;
    let scope = json!({"account":"PAYER","currency":"EUR"});
    assert_eq!(
        post(&server, "balance", "ordinary-secret", &scope).await.0,
        403
    );
    let (_, b) = post(&server, "balance", "finance-secret", &scope).await;
    assert_eq!(b["net_due_minor"], 10000);
    let p = json!({"version":1,"source":"BANK","account":"PAYER","currency":"EUR",
        "entries":[{"reference":"SENSITIVE-REF","amount_minor":6000,"status":"booked","direction":"credit"}]});
    assert_eq!(
        post(&server, "import", "finance-secret", &p).await.1["booked"],
        1
    );
    let mut a = json!({"source":"BANK","account":"PAYER","currency":"EUR","number":"INV-A","reference":"SENSITIVE-REF","amount_minor":6000});
    let (s, review) = post(&server, "review", "finance-secret", &a).await;
    assert_eq!(s, 200);
    a["review"] = review["review"].clone();
    let response = tokio::join!(
        post(&server, "confirm", "finance-secret", &a),
        post(&server, "confirm", "finance-secret", &a)
    );
    assert_eq!(response.0, (200, review.clone()));
    assert_eq!(response.0, response.1);
    // The posted receipt survives an unavailable issuer. Changed inputs still need a live owner.
    f.feed.offline.store(true, Ordering::SeqCst);
    assert_eq!(
        post(&server, "confirm", "finance-secret", &a).await,
        response.0
    );
    let mut changed = a.clone();
    changed["amount_minor"] = json!(100);
    assert_eq!(
        post(&server, "confirm", "finance-secret", &changed).await.0,
        503
    );
    server.kill().await;
    let journal = f.dir.join("accountingd.ndjson");
    let raw = std::fs::read_to_string(&journal).unwrap();
    assert!(
        !["SENSITIVE-REF", "INV-A", "PAYER"]
            .iter()
            .any(|value| raw.contains(value))
    );
    let restored = f.start().await;
    assert_eq!(
        post(&restored, "confirm", "finance-secret", &a).await,
        (200, review)
    );
    f.feed.offline.store(false, Ordering::SeqCst);
    let (_, b) = post(&restored, "balance", "finance-secret", &scope).await;
    assert_eq!(b["net_due_minor"], 4000);
    assert_eq!(b["allocations"].as_array().unwrap().len(), 1);
    assert_eq!(b["allocations"][0]["actor"], "operator:clerk");
    let key = DataKey::from_env("ACCOUNTINGD_DATA_KEY", &f.env).unwrap();
    let proof = accountingd::recovery_proof(
        emob_service::journal::File::open(journal).unwrap(),
        Arc::new(key),
        &f.estate.parties.provider,
    )
    .unwrap();
    assert_eq!(proof["allocations"], 1);
    assert_eq!(proof["cursor"], 1);
    restored.kill().await;
}

#[tokio::test]
async fn a_rewound_or_disagreeing_feed_cannot_authorize_a_cached_document() {
    // receivables-allocation/SR-351, receivables-allocation/SR-353
    let f = Fixture::new().await;
    let server = f.start().await;
    let scope = json!({"account":"PAYER","currency":"EUR"});
    f.feed
        .rows
        .lock()
        .unwrap()
        .extend((1..=101).map(|i| doc(i, &format!("INV-{i:03}"), 100)));
    let (_, b) = post(&server, "balance", "finance-secret", &scope).await;
    assert_eq!(b["net_due_minor"], 10100);
    assert_eq!(b["next_page"], 1);
    assert_eq!(b["documents"].as_array().unwrap().len(), 50);
    f.feed.rows.lock().unwrap()[100]["amount_minor"] = json!(200);
    assert_eq!(
        post(&server, "balance", "finance-secret", &scope).await.0,
        503
    );
    f.feed.rows.lock().unwrap().truncate(50);
    assert_eq!(
        post(&server, "balance", "finance-secret", &scope).await.0,
        503
    );
    server.kill().await;
}

#[tokio::test]
async fn withdrawal_evidence_traces_only_the_selected_invoice_and_keeps_the_original_allocation() {
    let f = Fixture::new().await;
    let server = f.start().await;
    let scope = json!({"account":"PAYER","currency":"EUR"});
    assert_eq!(
        post(&server, "balance", "finance-secret", &scope).await.1["net_due_minor"],
        0
    );
    assert_eq!(
        post(
            &server,
            "balance",
            "finance-secret",
            &json!({"account":"UNKNOWN","currency":"EUR"})
        )
        .await
        .0,
        404
    );
    let mut historical = doc(3, "HIST", 750);
    historical["account"] = json!("HISTORICAL");
    f.feed
        .rows
        .lock()
        .unwrap()
        .extend([doc(1, "INV", 2000), doc(2, "OTHER", 2000), historical]);
    assert_eq!(
        post(
            &server,
            "balance",
            "finance-secret",
            &json!({"account":"HISTORICAL","currency":"EUR"})
        )
        .await
        .1["net_due_minor"],
        750
    );
    let import = json!({"version":1,"source":"BANK","account":"PAYER","currency":"EUR",
        "entries":[{"reference":"PAY","amount_minor":1200,"status":"booked","direction":"credit"},
                   {"reference":"UNRELATED","amount_minor":1000,"status":"booked","direction":"credit"}]});
    assert_eq!(
        post(&server, "import", "finance-secret", &import).await.0,
        200
    );
    for (number, reference, amount) in [("INV", "PAY", 1200), ("OTHER", "UNRELATED", 1000)] {
        let mut req = json!({"source":"BANK","reference":reference,"account":"PAYER","currency":"EUR","number":number,"amount_minor":amount});
        req["review"] = post(&server, "review", "finance-secret", &req).await.1["review"].clone();
        assert_eq!(
            post(&server, "confirm", "finance-secret", &req).await.0,
            200
        );
    }
    let mut credit = doc(4, "CN", 2000);
    credit["kind"] = json!("credit");
    credit["predecessor"] = json!("INV");
    f.feed.rows.lock().unwrap().push(credit);
    let route = "withdrawal/evidence";
    for number in ["INV", "CN"] {
        let req = json!({"number":number,"account":"PAYER","currency":"EUR"});
        let (status, evidence) = post(&server, route, "finance-secret", &req).await;
        assert_eq!(status, 200);
        assert_eq!(evidence["open_minor"], 0);
        assert_eq!(
            evidence["allocations"],
            json!([{"number":"INV","source":"BANK","reference":"PAY","amount_minor":1200,"actor":"operator:clerk"}])
        );
        assert_eq!(post(&server, route, "ordinary-secret", &req).await.0, 403);
    }
    assert_eq!(
        post(
            &server,
            route,
            "finance-secret",
            &json!({"number":"INV","account":"HISTORICAL","currency":"EUR"})
        )
        .await
        .0,
        404
    );
    let balance = post(&server, "balance", "finance-secret", &scope).await.1;
    assert_eq!(balance["unapplied_minor"], 0);
    assert_eq!(balance["credit_minor"], 1200);
    assert_eq!(balance["documents"].as_array().unwrap().len(), 3);
    server.kill().await;
}

// customer-credit-offset/SR-030
#[tokio::test]
async fn concurrent_confirmations_post_one_offset_and_the_receipt_outlives_the_feed() {
    let f = Fixture::new().await;
    let server = f.start().await;
    f.feed
        .rows
        .lock()
        .unwrap()
        .extend([doc(1, "A", 10000), doc(2, "B", 8000)]);
    let cash = json!({"version":1,"source":"BANK","account":"PAYER","currency":"EUR",
        "entries":[{"reference":"PAY","amount_minor":10000,"status":"booked","direction":"credit"}]});
    assert_eq!(
        post(&server, "import", "finance-secret", &cash).await.0,
        200
    );
    let mut allocation = json!({"source":"BANK","account":"PAYER","currency":"EUR","number":"A","reference":"PAY","amount_minor":10000});
    allocation["review"] = post(&server, "review", "finance-secret", &allocation)
        .await
        .1["review"]
        .clone();
    assert_eq!(
        post(&server, "confirm", "finance-secret", &allocation)
            .await
            .0,
        200
    );
    let mut credit = doc(3, "AC", 3000);
    credit["kind"] = json!("line_credit");
    credit["predecessor"] = json!("A");
    f.feed.rows.lock().unwrap().push(credit);
    let mut selection = json!({"account":"PAYER","currency":"EUR","source_number":"A","target_number":"B","amount_minor":2000});
    let (status, expected) = post(&server, "offsets/review", "writer-secret", &selection).await;
    assert_eq!(status, 200);
    selection["review"] = expected["review"].clone();
    let (first, second) = tokio::join!(
        post(&server, "offsets/confirm", "writer-secret", &selection),
        post(&server, "offsets/confirm", "writer-secret", &selection)
    );
    assert_eq!(first, (200, expected.clone()));
    assert_eq!(second, first);
    let balance = post(
        &server,
        "balance",
        "finance-secret",
        &json!({"account":"PAYER","currency":"EUR"}),
    )
    .await
    .1;
    assert_eq!(
        balance["offsets"].as_array().unwrap(),
        std::slice::from_ref(&expected)
    );
    assert_eq!(balance["credit_minor"], 1000);
    f.feed.offline.store(true, Ordering::SeqCst);
    server.kill().await;
    let replay = f.start().await;
    assert_eq!(
        post(&replay, "offsets/confirm", "writer-secret", &selection).await,
        (200, expected)
    );
    assert_eq!(
        post(&replay, "offsets/confirm", "finance-secret", &selection)
            .await
            .0,
        403
    );
    selection["amount_minor"] = json!(100);
    assert_eq!(
        post(&replay, "offsets/confirm", "writer-secret", &selection)
            .await
            .0,
        503
    );
    replay.kill().await;
}
