//! Spec 003: a receipt the driver can check.
//!
//! A real station settles a real session — the Open Charge Alliance's DZG
//! record, signed on secp256k1 — and the receipt `csmsd` serves for it is read
//! the way a driver's phone reads it: through the shell's router, with the
//! token from the operator read model. A second station signs against a key
//! the registry does not hold, and its receipt says why it was not billed.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use csmsd::receipt::{self, Receipts, Site};
use csmsd::{Answer, Authorize, ChargePoint, Csmsd, Presented, Provisioning, Shared, Status};
use emob_core::{Currency, PartyId};
use emob_eichrecht::registry::{ComponentRef, KeyRegistry, RegisteredKey};
use emob_ocpp::fixtures::{OCA_1_6_SAMPLED_VALUE, OCA_KEY_HEX};
use emob_service::journal::{Journal as _, Memory};
use emob_service::{Identity as ShellIdentity, Readiness, Server, Shutdown};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::PublicKey;
use ocpp_kit::transport::{
    Admission, Auth, AuthOutcome, BasicAuthPassword, Csms, Handle, SecurityProfile, Station,
};
use ocpp_kit::types::{DateTime, Identity};
use ocpp_kit::{RawValue, Version, v1_6};
use rust_decimal::Decimal;
use tower_service::Service as _;

const UID: &str = "HRWWBX8";
const CONTRACT: &str = "DE-MSP-C00000001-W";

struct QuietStation;

impl ocpp_kit::transport::Handler for QuietStation {
    fn on_request(
        &self,
        _ctx: ocpp_kit::transport::Ctx,
        request: ocpp_kit::engine::IncomingRequest,
    ) -> ocpp_kit::transport::BoxFuture<'_, Result<Box<RawValue>, ocpp_kit::rpc::CallError>> {
        Box::pin(async move { Err(ocpp_kit::rpc::CallError::not_supported(&request.action)) })
    }
}

struct Provider;

impl Authorize for Provider {
    fn ask<'a>(
        &'a self,
        uid: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Answer> + Send + 'a>> {
        Box::pin(async move {
            Answer {
                status: Status::Accepted,
                reason: "a contract in force".to_owned(),
                token: Some(Presented {
                    issuer: PartyId::new("DE", "MSP").unwrap(),
                    uid: uid.to_owned(),
                    token_type: emob_roam::TokenType::Rfid,
                    contract_id: CONTRACT.parse().unwrap(),
                }),
                reference: Some("a".repeat(64)),
            }
        })
    }
}

fn registry(key_hex: &str) -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    registry
        .insert(
            ComponentRef::Meter {
                serial: "1DZG0028225179".into(),
            },
            RegisteredKey::unbounded(
                PublicKey::from_text(key_hex, Some(ocmf::Curve::Secp256k1)).unwrap(),
                "type approval — DZG GSH01.1K2L",
            ),
        )
        .unwrap();
    registry
}

fn csmsd(registry: KeyRegistry) -> Arc<Csmsd<Memory>> {
    Arc::new(
        Csmsd::open(
            PartyId::new("DE", "ABC").unwrap(),
            registry,
            Arc::new(Provider),
            Memory::new(),
            emob_core::TimeZone::utc(),
        )
        .expect("the journal folds")
        .issuing_receipts(Receipts::ephemeral("https://csms.example").expect("randomness")),
    )
}

fn tariff() -> Tariff {
    Tariff::simple(
        "ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![
            PriceComponent::new(Dimension::Energy, Decimal::from_str_exact("0.49").unwrap())
                .with_vat(Decimal::from(19)),
        ],
    )
}

fn sites() -> BTreeMap<emob_core::EvseId, Site> {
    BTreeMap::from([(
        "DE*ABC*E00001".parse().unwrap(),
        Site {
            name: "Depot Mitte".into(),
            address: "Invalidenstrasse 1, 10115 Berlin".into(),
            zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        },
    )])
}

async fn serve(csmsd: Arc<Csmsd<Memory>>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let fleet = Provisioning::new().with(
        Identity::new("CP-1").unwrap(),
        ChargePoint {
            evse_id: "DE*ABC*E00001".parse().unwrap(),
            rated_power_kw: Decimal::from(150),
            tariff: emob_tariff::TariffHistory::single(tariff()).unwrap(),
            ocpp: Version::V1_6,
        },
    );
    let csms = Csms::builder()
        .bind("127.0.0.1:0".parse().unwrap())
        .versions([Version::V1_6])
        .authenticate(move |auth: Auth| {
            let point = fleet.get(&auth.identity).cloned();
            async move {
                match point {
                    Some(point) => Admission::new().context(point).into(),
                    None => AuthOutcome::Unknown,
                }
            }
        })
        .handler(Shared(csmsd))
        .build()
        .expect("a CSMS");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = csms.serve_on(listener).await;
    });
    (addr, task)
}

async fn one_session(csmsd: &Arc<Csmsd<Memory>>) {
    let (addr, server) = serve(Arc::clone(csmsd)).await;
    let handle: Handle = Station::builder()
        .identity("CP-1")
        .unwrap()
        .url(format!("ws://{addr}/ocpp"))
        .versions([Version::V1_6])
        .security_profile(SecurityProfile::BasicAuth)
        .password(BasicAuthPassword::utf8("0123456789abcdef").unwrap())
        .reconnect(false)
        .handler(QuietStation)
        .build()
        .unwrap()
        .spawn()
        .unwrap();
    handle
        .call(v1_6::BootNotificationRequest::new(
            "GSH01.1K2L".to_owned(),
            "DZG".to_owned(),
        ))
        .await
        .unwrap();
    let _ = handle.wait_ready().await;
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            UID.to_owned(),
            0,
            DateTime::parse("2023-05-19T13:52:39Z").unwrap(),
        ))
        .await
        .unwrap();
    let stop = v1_6::StopTransactionRequest::new(
        108_814,
        DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
        started.transaction_id,
    )
    .with_reason(v1_6::Reason::Local)
    .with_transaction_data(vec![v1_6::MeterValue::new(
        DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
        vec![
            v1_6::SampledValue::new(OCA_1_6_SAMPLED_VALUE.to_owned())
                .with_format(v1_6::ValueFormat::SignedData)
                .with_context(v1_6::ReadingContext::TransactionEnd)
                .with_measurand(v1_6::Measurand::EnergyActiveImportRegister),
        ],
    )]);
    handle.call(stop).await.unwrap();
    handle.shutdown(Duration::from_secs(2)).await;
    server.abort();
}

/// The shell as a driver's phone reaches it: the receipt routes and nothing
/// else of this test's making.
fn shell(csmsd: &Arc<Csmsd<Memory>>) -> axum::Router {
    Server::new(
        ShellIdentity::new("csmsd", "0.0.0"),
        Readiness::new(),
        Shutdown::new(),
    )
    .with(receipt::router(Arc::clone(csmsd), sites()))
    .router()
}

struct Answered {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn get(router: &mut axum::Router, path: &str) -> Answered {
    let response = router
        .call(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap()
        .to_vec();
    Answered {
        status,
        headers,
        body,
    }
}

/// The one receipt the session left, by the path the read model lists.
fn the_receipt(csmsd: &Csmsd<Memory>) -> (String, String) {
    let key = csmsd.with_state(|state| {
        let issued: Vec<_> = state.receipts.values().collect();
        assert_eq!(issued.len(), 1, "one transaction, one receipt");
        issued[0].key.clone()
    });
    let path = csmsd.receipt_path(&key).expect("this CSMS issues receipts");
    let token = path.trim_start_matches("/r/").to_owned();
    (path, token)
}

/// Text a person reads: markup and attribute values removed.
fn visible(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn journal_text(csmsd: &Csmsd<Memory>) -> String {
    csmsd.with_journal(|journal| {
        journal
            .records()
            .expect("the journal reads")
            .iter()
            .map(|record| record.body.clone())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_billed_session_reads_verdict_first_and_checks_on_the_device() {
    let csmsd = csmsd(registry(OCA_KEY_HEX));
    one_session(&csmsd).await;
    assert_eq!(csmsd.settled(), 1);
    let (path, token) = the_receipt(&csmsd);
    let mut router = shell(&csmsd);

    let page = get(&mut router, &path).await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    let text = visible(&html);

    // Verdict first, then the figures the CDR carries.
    let verdict = text
        .find("Gemessen, signiert, geprüft")
        .expect("the verdict");
    let energy = text
        .find("0,636 kWh")
        .expect("the signed difference, in German");
    assert!(verdict < energy, "the verdict leads");
    let (gross, energy_kwh) = csmsd.with_cdrs(|ledger| {
        let cdr = ledger.iter().next().unwrap();
        (
            cdr.cost.as_ref().unwrap().gross().amount().to_string(),
            cdr.total_energy.kwh().to_string(),
        )
    });
    assert_eq!(energy_kwh, "0.636");
    assert!(
        text.contains(&format!("{}\u{a0}€", gross.replace('.', ","))),
        "{gross}: {text}"
    );
    assert!(text.contains("[MessEG §33]"));
    assert!(text.contains("DE*ABC*E00001") && text.contains("Invalidenstrasse 1"));
    assert!(
        text.contains("19.05.2023, 15:52"),
        "local time at the point: {text}"
    );

    // The key, in groups, with the instruction — the driver's one trust anchor.
    let key = PublicKey::from_text(OCA_KEY_HEX, Some(ocmf::Curve::Secp256k1)).unwrap();
    let hex = hex::encode_upper(key.sec1_bytes());
    assert!(text.contains(&hex[..4]) && text.contains(&format!("{} {}", &hex[..4], &hex[4..8])));
    assert!(text.contains("am Ladepunkt aufgedruckt"));

    // SR-068: what the page hands the device verifies there with the same
    // check the WebAssembly module runs.
    let data_key = attr(&html, "data-key").expect("the self-check carries the key");
    let records = attrs(&html, "data-ocmf");
    assert!(!records.is_empty());
    for record in &records {
        let check = emob_verify::check(&unescape(record), &data_key);
        assert_eq!(check.verdict, emob_verify::Verdict::Verified, "{record}");
    }
    assert!(html.contains("src=\"/r/verify.js\""));
    assert!(
        html.contains("id=\"self-check-run\" hidden"),
        "no JS, no button"
    );

    // SR-066: the policy, and nothing from another origin.
    assert_eq!(
        page.headers[header::CONTENT_SECURITY_POLICY],
        emob_service::ui::CSP_WITH_WASM
    );
    assert_eq!(page.headers["x-robots-tag"], "noindex, nofollow");
    assert_eq!(page.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(page.headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(page.headers[header::X_FRAME_OPTIONS], "DENY");
    assert!(!html.contains("http://") && !html.contains("https://"));

    // SR-064: no name, no card, no contract on the page.
    assert!(!html.contains(CONTRACT));
    assert!(!text.contains(UID), "the card is not page text");

    // The budget: a page a phone on a bad link opens at once.
    assert!(page.body.len() < 30 * 1024, "{} bytes", page.body.len());

    // English is typed against German.
    let english = get(&mut router, &format!("{path}?lang=en")).await;
    assert!(visible(&english.text()).contains("Measured, signed, checked"));

    // SR-069: the file is the evidence, verbatim, beside the registered key.
    let file = get(&mut router, &format!("/r/{token}/transparenz.xml")).await;
    assert_eq!(file.status, StatusCode::OK);
    assert!(
        file.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    assert_eq!(file.headers["x-robots-tag"], "noindex, nofollow");
    let values = emob_eichrecht::transparency::from_xml(&file.text()).expect("a container");
    let signed = csmsd.with_state(|state| {
        state
            .receipts
            .values()
            .next()
            .unwrap()
            .receipt
            .signed
            .clone()
    });
    assert_eq!(
        values
            .entries
            .iter()
            .map(|e| e.signed_data.clone())
            .collect::<Vec<_>>(),
        signed
    );
    for entry in &values.entries {
        let check = emob_verify::check(&entry.signed_data, entry.public_key.as_deref().unwrap());
        assert!(check.verdict.holds());
    }
    assert!(!file.text().contains(CONTRACT));

    // The verifier is served, and is a module.
    let wasm = get(&mut router, receipt::VERIFIER_PATH).await;
    assert_eq!(wasm.headers[header::CONTENT_TYPE], "application/wasm");
    assert_eq!(&wasm.body[..4], b"\0asm");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_060_sr_062_the_journal_holds_the_digest_and_never_the_token() {
    let csmsd = csmsd(registry(OCA_KEY_HEX));
    one_session(&csmsd).await;
    let (_, token) = the_receipt(&csmsd);

    assert_eq!(token.len(), 22, "16 bytes, base64url");
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    let journal = journal_text(&csmsd);
    assert!(!journal.contains(&token), "the token is never journalled");
    assert!(journal.contains(&receipt::digest(&token)), "its SHA-256 is");
    assert!(!journal.contains("/r/"), "nor a link");

    // SR-062: the link reaches billd at dispatch, not through the journal.
    let delivery = csmsd.with_state(|state| {
        state
            .pending()
            .find(|d| d.to == csmsd::Target::Billd)
            .cloned()
            .expect("a delivery owed to billd")
    });
    assert!(!delivery.body.contains("receipt"));
    let sent = csmsd::outbox::with_receipt(&csmsd, &delivery);
    let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(
        body["data"]["receipt"],
        format!("https://csms.example/r/{token}"),
        "billd gets the absolute link"
    );
    assert_eq!(
        sent.body,
        csmsd::outbox::with_receipt(&csmsd, &delivery).body,
        "same bytes every attempt"
    );

    // Filed under the record's key, so it is found without a search (FR-003).
    let key = csmsd
        .with_state(|state| state.receipt_for.get(&delivery.id).cloned())
        .expect("the billed record's receipt is indexed");
    assert_eq!(csmsd.receipt_path(&key), Some(format!("/r/{token}")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_061_an_unknown_token_is_the_same_404_as_a_path_nobody_serves() {
    let csmsd = csmsd(registry(OCA_KEY_HEX));
    one_session(&csmsd).await;
    let (_, token) = the_receipt(&csmsd);
    let mut router = shell(&csmsd);

    // Well-formed and never issued: the issued token with its last character
    // changed.
    let mut forged = token.clone();
    let last = forged.pop().unwrap();
    forged.push(if last == 'A' { 'B' } else { 'A' });

    // Compared as the bytes a client receives, less the `date` and request id
    // lines.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let nothing = wire(addr, "/nothing-here").await;
    assert!(nothing.starts_with("HTTP/1.1 404"), "{nothing}");
    for path in [
        format!("/r/{forged}"),
        "/r/short".to_owned(),
        format!("/r/{token}x"),
        format!("/r/{forged}/transparenz.xml"),
    ] {
        assert_eq!(wire(addr, &path).await, nothing, "{path}");
    }
    // …and the issued token is what opens it.
    assert!(
        get(&mut router, &format!("/r/{token}"))
            .await
            .status
            .is_success()
    );
    server.abort();
}

async fn wire(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: csms\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8_lossy(&bytes)
        .lines()
        // Per request, never per path: the date, and the id a log line cites.
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            !line.starts_with("date:") && !line.starts_with("x-request-id:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_063_no_token_reaches_a_log_line() {
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();

    let csmsd = csmsd(registry(OCA_KEY_HEX));
    one_session(&csmsd).await;
    let (path, token) = the_receipt(&csmsd);
    let mut router = shell(&csmsd);
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!("a line, so an empty capture is not mistaken for a pass");
        let _ = get(&mut router, &path).await;
        let _ = get(&mut router, &format!("{path}/transparenz.xml")).await;
        let _ = get(&mut router, "/r/AAAAAAAAAAAAAAAAAAAAAA").await;
    }
    let mut out = captured.0.lock().unwrap().clone();
    out.flush().unwrap();
    let log = String::from_utf8_lossy(&out);
    assert!(log.contains("a line"), "the capture works: {log}");
    assert!(!log.contains(&token), "{log}");
    assert!(!log.contains("AAAAAAAAAAAAAAAAAAAAAA"), "{log}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_signed_with_a_key_nobody_registered_says_why_it_was_not_billed() {
    // The registry holds another meter's key: every signature fails against it.
    let other = "3059301306072A8648CE3D020106082A8648CE3D030107034200043AEEB45C392357820A58FDFB0857BD77ADA31585C61C430531DFA53B440AFBFDD95AC887C658EA55260F808F55CA948DF235C2108A0D6DC7D4AB1A5E1A7955BE";
    let mut wrong = KeyRegistry::new();
    wrong
        .insert(
            ComponentRef::Meter {
                serial: "1DZG0028225179".into(),
            },
            RegisteredKey::unbounded(
                PublicKey::from_text(other, Some(ocmf::Curve::Secp256r1)).unwrap(),
                "not this meter's",
            ),
        )
        .unwrap();
    let csmsd = csmsd(wrong);
    one_session(&csmsd).await;
    assert_eq!(csmsd.settled(), 0);
    let (path, token) = the_receipt(&csmsd);
    let mut router = shell(&csmsd);

    let page = get(&mut router, &path).await;
    assert_eq!(page.status, StatusCode::OK);
    let text = visible(&page.text());
    assert!(text.contains("Nicht abgerechnet"), "{text}");
    assert!(text.contains("[MessEG §33]"));
    assert!(!text.contains("\u{a0}€"), "no price on a refusal: {text}");
    assert!(text.contains("Signierte Messwerte"));
    // Nothing verified against the registry, so no file is offered as evidence.
    assert!(!page.text().contains("transparenz.xml"));
    let file = get(&mut router, &format!("/r/{token}/transparenz.xml")).await;
    assert_eq!(file.status, StatusCode::NOT_FOUND);

    // A refusal files no record's receipt; the operator view lists the session.
    assert!(csmsd.with_state(|state| state.receipt_for.is_empty()));
    let view = csmsd::read::Today::of(&csmsd, &[], &csmsd::read::Params::default()).unwrap();
    assert_eq!(view.refused.total, 1);
}

fn attr(html: &str, name: &str) -> Option<String> {
    attrs(html, name).into_iter().next()
}

fn attrs(html: &str, name: &str) -> Vec<String> {
    let needle = format!("{name}=\"");
    html.match_indices(&needle)
        .filter_map(|(at, _)| {
            let rest = &html[at + needle.len()..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect()
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
