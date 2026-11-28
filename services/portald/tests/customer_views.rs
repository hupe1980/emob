//! Exercise the portal across HTTP, including what the browser sends to each owner.
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse as _;
use axum::{Json, Router, routing::post};
use emob_service::{Identity, Shutdown, estate};
use portald::{Estate, Options};
use reqwest::{Client, Response};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const ORIGIN: &str = "https://customer.example";
const TOKEN: &str = "e20fa23120dfcc6db5535a56e6ca8c51cd99ac112d03c6814e8d02e5c2c86ab1";
const OTHER_TOKEN: &str = "b673492be1cbf5f7852d692de42097d273150506bdd45348cdfa0fecf7be20c8";
const RECEIPT: &str = "https://receipts.example/r/AAAAAAAAAAAAAAAAAAAAAA";

#[derive(Default)]
struct Directory {
    session: AtomicBool,
    other_session: AtomicBool,
    granted: AtomicBool,
    unavailable: AtomicBool,
}

fn presented(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .is_some_and(|h| h.as_bytes() == format!("Bearer {token}").as_bytes())
        && !headers.contains_key(header::COOKIE)
}

fn authenticated(headers: &HeaderMap, d: &Directory) -> bool {
    d.session.load(Ordering::SeqCst) && presented(headers, TOKEN)
}

async fn login(
    State(d): State<Arc<Directory>>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let (session, token) = if req == json!({"subject":"driver","secret":"fleet test key"}) {
        (&d.session, TOKEN)
    } else if req == json!({"subject":"another-payer","secret":"other test key"}) {
        (&d.other_session, OTHER_TOKEN)
    } else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    session.store(true, Ordering::SeqCst);
    Ok(Json(json!({"session":token})))
}

async fn customer(
    State(d): State<Arc<Directory>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !authenticated(&headers, &d) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(
        json!({"subject":"driver","accounts":["PAYER"],"agreements":[{"agreement":"agreement","emaid":"DE-MSP-123","point":"CP-1","plan":"<basic>","reference":"a".repeat(64)}]}),
    ))
}

async fn history(
    State(d): State<Arc<Directory>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    if !authenticated(&headers, &d) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if d.unavailable.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    assert!(input["page"].is_u64());
    let mut invoices = vec![];
    if d.granted.load(Ordering::SeqCst) {
        invoices.push(json!({"number":"DE-1","period":"2026-10","account":"PAYER","currency":"EUR","kind":"invoice","payable":"4.26"}));
    }
    let receipts = [RECEIPT,"https://foreign.example/r/AAAAAAAAAAAAAAAAAAAAAA"].map(|r|json!({"agreement":"agreement", "source":"CDR","receipt":r,"ended_at":"2026-10-12T10:05:00Z","point":"CP-1","kwh":"4.21"}));
    Ok(Json(
        json!({"invoices":invoices,"receipts":receipts,"next_page":null}),
    ))
}

async fn document(
    State(d): State<Arc<Directory>>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> axum::response::Response {
    if !authenticated(&headers, &d) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !d.granted.load(Ordering::SeqCst) || req != json!({"number":"DE-1"}) {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [(header::CONTENT_TYPE, "application/xml")],
        "<Invoice>issued bytes</Invoice>",
    )
        .into_response()
}

async fn balance(
    State(directory): State<Arc<Directory>>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let account = if authenticated(&headers, &directory) {
        if !directory.granted.load(Ordering::SeqCst) {
            return Err(StatusCode::NOT_FOUND);
        }
        "PAYER"
    } else if directory.other_session.load(Ordering::SeqCst) && presented(&headers, OTHER_TOKEN) {
        "OTHER"
    } else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let number = req["number"].as_str().unwrap();
    assert_eq!(req, json!({"number": number}));
    let (face, credited, paid, incoming, outgoing, due) = match (account, number) {
        ("PAYER", "DE-1") => (10000, 3000, 10000, 0, 2000, -1000),
        ("PAYER", "DE-2") => (8000, 0, 0, 2000, 0, 6000),
        ("OTHER", "DE-3") => (300, 0, 0, 0, 0, 300),
        _ => return Err(StatusCode::NOT_FOUND),
    };
    Ok(Json(json!({
        "number":number, "account":account, "currency":"EUR", "amount_minor":face,
        "credited_minor":credited, "paid_minor":paid, "incoming_minor":incoming,
        "outgoing_minor":outgoing, "due_minor":due
    })))
}

async fn logout(State(d): State<Arc<Directory>>, headers: HeaderMap) -> StatusCode {
    if !authenticated(&headers, &d) {
        return StatusCode::UNAUTHORIZED;
    }
    d.session.store(false, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

struct Fixture {
    running: portald::Running,
    owner: tokio::task::JoinHandle<()>,
    directory: Arc<Directory>,
    base: String,
    client: Client,
}

impl Fixture {
    async fn new() -> Self {
        let directory = Arc::new(Directory::default());
        directory.granted.store(true, Ordering::SeqCst);
        let routes = Router::new()
            .route("/v1/empd/customer/login", post(login))
            .route("/v1/empd/customer/me", post(customer))
            .route("/v1/empd/customer/logout", post(logout))
            .route("/v1/billd/customer/history", post(history))
            .route("/v1/billd/customer/document", post(document))
            .route("/v1/accountingd/customer/balance", post(balance))
            .with_state(directory.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let owner = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let config = format!(
            r#"[services]
empd = "{url}"
billd = "{url}"
accountingd = "{url}"
roamd = "{url}"
webhook_secret_env = "UNUSED_WEBHOOK"
service_token_env = "UNUSED_SERVICE"
[portal]
public_url = "{ORIGIN}"
[receipts]
public_url = "https://receipts.example"
"#
        );
        let running = portald::run(Options {
            http: "127.0.0.1:0".parse().unwrap(),
            estate: estate::from_str(&config).unwrap(),
            identity: Identity::new("portald", "test"),
            shutdown: Shutdown::new(),
        })
        .await
        .unwrap();
        let base = format!("http://{}/customer/", running.http);
        Self {
            running,
            owner,
            directory,
            base,
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }

    async fn form(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        origin: &str,
        cookie: &str,
    ) -> Response {
        self.form_in(path, fields, origin, cookie, "en").await
    }

    async fn form_in(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        origin: &str,
        cookie: &str,
        lang: &str,
    ) -> Response {
        self.client
            .post(format!("{}{path}?lang={lang}", self.base))
            .header(header::ORIGIN, origin)
            .header(header::COOKIE, cookie)
            .form(fields)
            .send()
            .await
            .unwrap()
    }

    async fn page(&self, cookie: &str) -> Response {
        self.client
            .get(format!("{}?lang=en", self.base))
            .header(header::COOKIE, cookie)
            .send()
            .await
            .unwrap()
    }

    async fn stop(self) {
        self.running.kill().await;
        self.owner.abort();
        let _ = self.owner.await;
    }

    async fn sign_in(&self) -> String {
        self.sign_in_as("driver", "fleet test key").await
    }

    async fn sign_in_as(&self, subject: &str, secret: &str) -> String {
        let r = self
            .form(
                "login",
                &[("subject", subject), ("secret", secret)],
                ORIGIN,
                "",
            )
            .await;
        assert_eq!(r.status(), 303);
        assert_eq!(r.headers()[header::LOCATION], "/customer/?lang=en");
        let cookie = r.headers()[header::SET_COOKIE].to_str().unwrap();
        for flag in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/customer/"] {
            assert!(cookie.contains(flag));
        }
        assert!(!cookie.contains("Domain="));
        cookie.split(';').next().unwrap().to_owned()
    }
}

// customer-credit-offset/SR-031. The browser reuses the selected document's current grant.
#[tokio::test]
async fn payer_sees_only_the_credit_movement_on_the_requested_invoice() {
    let f = Fixture::new().await;
    let cookie = f.sign_in().await;
    let overview = f.page(&cookie).await.text().await.unwrap();
    assert!(overview.contains(r#"aria-label="View invoice balance · DE-1""#));
    // fleet-navigation/SR-031. Only the history position travels in the URL.
    let later = f
        .client
        .get(format!("{}?lang=en&page=3", f.base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(later.contains(r#"action="/customer/balance?lang=en&amp;page=3""#));
    let detail = f
        .client
        .post(format!("{}balance?lang=en&page=3", f.base))
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, &cookie)
        .form(&[("number", "DE-1")])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(detail.contains(r#"href="/customer/?lang=en&amp;page=3""#));
    for (number, lang, offset, remainder, cash, other) in [
        (
            "DE-1",
            "en",
            "Credit used on other invoices</dt><dd>20.00",
            "Credit balance</dt><dd>10.00",
            "Payments applied</dt><dd>100.00",
            "DE-2",
        ),
        (
            "DE-2",
            "en",
            "Credit from other invoices</dt><dd>20.00",
            "Amount due</dt><dd>60.00",
            "Payments applied</dt><dd>0.00",
            "DE-1",
        ),
        (
            "DE-1",
            "de",
            "Guthaben für andere Rechnungen verwendet</dt><dd>20,00",
            "Guthaben</dt><dd>10,00",
            "Zugeordnete Zahlungen</dt><dd>100,00",
            "DE-2",
        ),
        (
            "DE-2",
            "de",
            "Guthaben aus anderen Rechnungen</dt><dd>20,00",
            "Offener Betrag</dt><dd>60,00",
            "Zugeordnete Zahlungen</dt><dd>0,00",
            "DE-1",
        ),
    ] {
        let response = f
            .form_in("balance", &[("number", number)], ORIGIN, &cookie, lang)
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        let body = response.text().await.unwrap();
        assert!(body.contains(&format!("<h2>{number}</h2>")));
        for field in [offset, remainder, cash] {
            assert!(body.contains(&format!("<dt>{field} EUR</dd>")), "{field}");
        }
        assert!(!body.contains(other));
    }
    let other_cookie = f.sign_in_as("another-payer", "other test key").await;
    assert_eq!(
        f.form("balance", &[("number", "DE-3")], ORIGIN, &other_cookie)
            .await
            .status(),
        200
    );
    let foreign = f
        .form("balance", &[("number", "DE-1")], ORIGIN, &other_cookie)
        .await;
    assert_eq!(foreign.status(), 404);
    assert_eq!(
        f.form("balance", &[("number", "DE-3")], ORIGIN, &cookie)
            .await
            .status(),
        404
    );
    f.directory.granted.store(false, Ordering::SeqCst);
    let revoked = f
        .form("balance", &[("number", "DE-1")], ORIGIN, &cookie)
        .await;
    assert_eq!(revoked.status(), 404);
    assert_eq!(foreign.text().await.unwrap(), revoked.text().await.unwrap());
    f.stop().await;
}

// customer-invoice-access/SR-344. Only the customer session can cross the browser boundary.
#[tokio::test]
async fn customer_login_scope_download_and_logout_follow_the_owners() {
    let f = Fixture::new().await;
    let login = f.page("").await;
    assert_eq!(login.status(), 200);
    assert!(
        login
            .text()
            .await
            .unwrap()
            .contains("autocomplete=\"current-password\"")
    );
    let cookie = f.sign_in().await;
    let r = f.page(&cookie).await;
    assert_eq!(r.headers()[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(r.headers()[header::REFERRER_POLICY], "same-origin");
    let html = r.text().await.unwrap();
    for s in [
        "My overview",
        "DE-1",
        "DE-MSP-123",
        "&lt;basic&gt;",
        RECEIPT,
        "4.21 kWh",
        "View agreed prices",
    ] {
        assert!(html.contains(s), "{s}");
    }
    assert!(!html.contains(TOKEN) && !html.contains("foreign.example"));
    let r = f
        .form("document", &[("number", "DE-1")], ORIGIN, &cookie)
        .await;
    assert_eq!(r.headers()[header::CONTENT_TYPE], "application/xml");
    assert_eq!(r.text().await.unwrap(), "<Invoice>issued bytes</Invoice>");
    f.directory.granted.store(false, Ordering::SeqCst);
    assert_eq!(
        f.form("document", &[("number", "DE-1")], ORIGIN, &cookie)
            .await
            .status(),
        404
    );
    assert!(!f.page(&cookie).await.text().await.unwrap().contains("DE-1"));
    let signed_out = f.form("logout", &[], ORIGIN, &cookie).await;
    assert!(
        signed_out.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(f.page(&cookie).await.status(), 401);
    f.stop().await;
}

// customer-invoice-access/SR-344. Sign-in itself requires same-origin submission.
#[tokio::test]
async fn cross_origin_forms_are_refused_and_outage_keeps_agreements_readable() {
    let f = Fixture::new().await;
    let creds = [("subject", "driver"), ("secret", "fleet test key")];
    let denied = f.form("login", &creds, "https://other.example", "").await;
    assert_eq!(denied.status(), 403);
    assert!(!f.directory.session.load(Ordering::SeqCst));
    let cookie = f.sign_in().await;
    assert_eq!(f.form("logout", &[], "null", &cookie).await.status(), 403);
    assert_eq!(
        f.form(
            "document",
            &[("number", "DE-1")],
            "https://other.example",
            &cookie
        )
        .await
        .status(),
        403
    );
    f.directory.unavailable.store(true, Ordering::SeqCst);
    let r = f.page(&cookie).await;
    assert_eq!(r.status(), 503);
    let text = r.text().await.unwrap();
    assert!(
        text.contains("DE-MSP-123")
            && text.contains("Billing temporarily unavailable")
            && !text.contains("DE-1")
    );
    f.stop().await;
}

#[test]
fn public_http_is_limited_to_loopback() {
    let estate = serde_json::json!({
        "portal":{"public_url":"http://outside.example"},
        "services":{"empd":"http://127.0.0.1:9585","billd":"http://127.0.0.1:9584",
            "roamd":"http://127.0.0.1:9583","webhook_secret_env":"WEBHOOK","service_token_env":"SERVICE"}
    });
    let mut config: Estate = serde_json::from_value(estate).unwrap();
    assert!(config.validate().is_err());
    for url in [
        "http://127.0.0.1:9589",
        "http://[::1]:9589",
        "https://portal.example",
    ] {
        config.portal.public_url = url.into();
        config.validate().unwrap();
    }
}
