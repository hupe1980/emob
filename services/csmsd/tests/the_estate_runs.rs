//! Spec 001: the estate runs — one session, from a station's socket to the
//! month's invoice and the partner's platform, through the real daemons.
//!
//! `empd`, `billd`, `roamd` and `csmsd` are started in-process on loopback by
//! their own `daemon::run`, each with a journal file in a scratch directory and
//! the demo estate (`deploy/estate.example.toml`) pointed at the ports they got.
//! The station is `ocpp-kit`'s `MockStation`; the partner is `ocpi-kit`'s
//! `MockPeer`; the meter is `emob-sim`'s first virtual meter, whose public key
//! the demo estate lists. Nothing here signs its own fixture: the record is the
//! simulator's and the verifier is `ocmf`'s.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use csmsd::{DeliveryState, Event, Outcome, Status, Target};
use emob_core::Energy;
use emob_service::estate::{self, Env};
use emob_service::journal::Journal as _;
use emob_service::{Identity, Shutdown};
use emob_sim::{Fault, Rng, SessionPlan, VirtualStation};
use ocpi_kit::VersionNumber;
use ocpi_kit::client::{ClientConfig, OcpiClient};
use ocpi_kit::server::{InMemoryTokenStore, OcpiRouter};
use ocpi_kit::testkit::MockPeer;
use ocpi_kit::transport::CredentialsToken;
use ocpi_kit::types::{CiString, OcpiString, PartyRef, Url as KitUrl};
use ocpp_kit::testkit::MockStation;
use ocpp_kit::types::DateTime;
use ocpp_kit::v1_6;
use rust_decimal::Decimal;

const DEMO: &str = include_str!("../../../deploy/estate.example.toml");
const GOOD_CARD: &str = "04A1B2C3D4E5F6";
const BLOCKED_CARD: &str = "04DEADBEEF0001";
const OPERATOR: &str = "operator-token";
const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
/// The bootstrap token the demo estate hands the partner out of band.
const TOKEN_A: &str = "token-a-for-de-msp";
/// The token the partner's platform issues to this node.
const TOKEN_B: &str = "token-b-from-the-partner";

// ── the estate ──────────────────────────────────────────────────────────────

fn env() -> Env {
    let mut values: BTreeMap<&str, String> = [
        ("EMOB_WEBHOOK_SECRET", SECRET),
        ("EMOB_SERVICE_TOKEN", "service-token"),
        ("EMOB_OPERATOR_TOKEN", OPERATOR),
        (
            "EMPD_DATA_KEY",
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        ),
        (
            "ROAMD_DATA_KEY",
            "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
        ),
        (
            "CSMSD_DATA_KEY",
            "AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM=",
        ),
        (
            "BILLD_DATA_KEY",
            "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ=",
        ),
        ("EMOB_PARTNER_DE_MSP_TOKEN_A", TOKEN_A),
    ]
    .into_iter()
    .map(|(name, value)| (name, value.to_owned()))
    .collect();
    // Each daemon signs its journal's head (SR-096), under one key for the
    // whole test binary, so a daemon restarted over its journal opens it.
    static SIGNING: std::sync::OnceLock<[(&str, String); 4]> = std::sync::OnceLock::new();
    let signing = SIGNING.get_or_init(|| {
        [
            "CSMSD_SIGNING_KEY",
            "BILLD_SIGNING_KEY",
            "ROAMD_SIGNING_KEY",
            "EMPD_SIGNING_KEY",
        ]
        .map(|name| (name, emob_service::head::SigningKey::generate()))
    });
    values.extend(signing.iter().cloned());
    Arc::new(move |name| values.get(name).cloned())
}

/// A port nobody is listening on yet.
fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

/// Where the four daemons listen, and where they keep their journals.
#[derive(Clone)]
struct Estate {
    dir: PathBuf,
    empd: SocketAddr,
    billd: SocketAddr,
    roamd: SocketAddr,
    csmsd_http: SocketAddr,
    /// The demo estate, pointed at these ports, retrying every second.
    text: String,
}

impl Estate {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "emob-estate-{name}-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let (empd, billd, roamd) = (free_port(), free_port(), free_port());
        let text = DEMO
            .replace("http://127.0.0.1:9584", &format!("http://{billd}"))
            .replace("http://127.0.0.1:9585", &format!("http://{empd}"))
            .replace("http://127.0.0.1:9583", &format!("http://{roamd}"))
            .replace("retry_secs = 5", "retry_secs = 1")
            // `MockStation` speaks profile 1 over `ws://` with the demo
            // password, so this run is loopback without TLS; the TLS path is
            // `a_station_proves_who_it_is.rs`'s.
            .replace("security_profile = 2", "security_profile = 1")
            .replace(r#"tls_cert = "../target/dev-tls/server.pem""#, "")
            .replace(r#"tls_key = "../target/dev-tls/server.key""#, "")
            .replace(r#"client_ca = "../target/dev-tls/ca.pem""#, "");
        Self {
            dir,
            empd,
            billd,
            roamd,
            csmsd_http: free_port(),
            text,
        }
    }

    fn sections<T: serde::de::DeserializeOwned>(&self) -> T {
        estate::from_str(&self.text).expect("the demo estate is one every daemon runs on")
    }

    async fn empd(&self) -> empd::daemon::Running {
        empd::daemon::run(empd::daemon::Options {
            http: self.empd,
            journal: self.dir.join("empd.ndjson"),
            estate: self.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("empd", "test"),
        })
        .await
        .expect("empd starts")
    }

    async fn billd(&self) -> billd::daemon::Running {
        billd::daemon::run(billd::daemon::Options {
            http: self.billd,
            journal: self.dir.join("billd.ndjson"),
            estate: self.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("billd", "test"),
        })
        .await
        .expect("billd starts")
    }

    async fn roamd(&self) -> roamd::daemon::Running {
        roamd::daemon::run(roamd::daemon::Options {
            http: self.roamd,
            journal: self.dir.join("roamd.ndjson"),
            estate: self.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("roamd", "test"),
        })
        .await
        .expect("roamd starts")
    }

    async fn csmsd_with(&self, estate: csmsd::estate::Estate) -> csmsd::daemon::Running {
        csmsd::daemon::run(csmsd::daemon::Options {
            ocpp: "127.0.0.1:0".parse().unwrap(),
            http: self.csmsd_http,
            journal: self.dir.join("csmsd.ndjson"),
            estate,
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("csmsd", "test"),
            clock: emob_service::system_clock(),
        })
        .await
        .expect("csmsd starts")
    }

    async fn csmsd(&self) -> csmsd::daemon::Running {
        self.csmsd_with(self.sections()).await
    }
}

// ── the station ─────────────────────────────────────────────────────────────

/// The demo estate's meter: `emob-sim`'s first virtual station.
fn meter() -> VirtualStation {
    VirtualStation::new(1, &mut Rng::stream(2026, "estate"))
}

fn plan() -> SessionPlan {
    let started_at = time::macros::datetime!(2026-11-03 10:00 +1);
    SessionPlan {
        started_at,
        ended_at: started_at + time::Duration::minutes(30),
        energy: Energy::from_kwh(Decimal::from_str_exact("11.250").unwrap()).unwrap(),
        clock_aligned: false,
        connected_before_charging: time::Duration::seconds(20),
    }
}

fn at(instant: time::OffsetDateTime) -> DateTime {
    DateTime::parse(
        &instant
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    )
    .unwrap()
}

/// A signed record as 1.6 carries it: a `SignedMeterValue` document in the
/// sampled value's `value` `[OCA SMV §3.1]`.
fn sampled(record: &str, context: v1_6::ReadingContext) -> v1_6::SampledValue {
    let document = serde_json::json!({
        "signedMeterData": record,
        "signingMethod": "",
        "encodingMethod": "OCMF",
    });
    v1_6::SampledValue::new(document.to_string())
        .with_format(v1_6::ValueFormat::SignedData)
        .with_context(context)
        .with_measurand(v1_6::Measurand::EnergyActiveImportRegister)
}

/// A station that has booted, against `csmsd`'s OCPP socket.
async fn station(ocpp: SocketAddr) -> MockStation {
    let station = MockStation::connect(&format!("ws://{ocpp}/ocpp"), "CP-1").expect("a station");
    station
        .boot(ocpp_kit::Version::V1_6)
        .await
        .expect("the CSMS answers a boot");
    station
}

/// Authorise, start, send the signed begin, stop with the signed end — the
/// shape `[OCA SMV §3.1]` describes. Returns the start's answer.
async fn charge(
    station: &MockStation,
    card: &str,
    faults: &[Fault],
) -> v1_6::StartTransactionResponse {
    let plan = plan();
    let session = meter().charge(&plan, faults);
    let handle = station.handle();
    let authorized = handle
        .call(v1_6::AuthorizeRequest::new(card))
        .await
        .expect("the CSMS answers Authorize");
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            card.to_owned(),
            0,
            at(plan.started_at),
        ))
        .await
        .expect("the CSMS answers a start");
    assert_eq!(
        authorized.id_tag_info.status, started.id_tag_info.status,
        "the start is asked about too, and gets the same answer"
    );
    if started.id_tag_info.status != v1_6::AuthorizationStatus::Accepted {
        return started;
    }
    let (first, rest) = session.records.split_first().expect("a begin record");
    let (last, middle) = rest.split_last().expect("an end record");
    let mut values = vec![v1_6::MeterValue::new(
        at(plan.started_at),
        vec![sampled(first, v1_6::ReadingContext::TransactionBegin)],
    )];
    values.extend(middle.iter().map(|record| {
        v1_6::MeterValue::new(
            at(plan.started_at),
            vec![sampled(record, v1_6::ReadingContext::SamplePeriodic)],
        )
    }));
    handle
        .call(v1_6::MeterValuesRequest::new(1, values).with_transaction_id(started.transaction_id))
        .await
        .expect("the CSMS answers MeterValues");
    handle
        .call(
            v1_6::StopTransactionRequest::new(0, at(plan.ended_at), started.transaction_id)
                .with_reason(v1_6::Reason::Local)
                .with_transaction_data(vec![v1_6::MeterValue::new(
                    at(plan.ended_at),
                    vec![sampled(last, v1_6::ReadingContext::TransactionEnd)],
                )]),
        )
        .await
        .expect("the CSMS answers a stop");
    started
}

// ── the partner ─────────────────────────────────────────────────────────────

/// `ocpi-kit`'s `MockPeer`, standing in for the eMSP `DE*MSP`'s platform.
async fn partner() -> (MockPeer, SocketAddr) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = KitUrl::new_lenient(format!("http://{addr}/ocpi"));
    let peer = MockPeer::new(
        base.clone(),
        PartyRef::new("DE", "MSP").unwrap(),
        ocpi_kit::v2_3_0::types::Role::Emsp,
    );
    // The token this platform issues to the operator's node, speaking for the
    // operator's party — what its ownership check holds a push to.
    let tokens = Arc::new(InMemoryTokenStore::new());
    tokens.insert(
        CredentialsToken::new(TOKEN_B).unwrap(),
        ocpi_kit::testkit::registered_peer("DE*ABC", vec![PartyRef::new("DE", "ABC").unwrap()]),
    );
    let app = axum::Router::new().nest(
        "/ocpi",
        peer.mount(OcpiRouter::new(VersionNumber::V2_3_0, base, tokens))
            .build(),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (peer, addr)
}

/// The partner registers with `roamd` using the `Token A` from the estate.
async fn register(roamd: SocketAddr, partner: SocketAddr) {
    let client = OcpiClient::with_config(ClientConfig::for_testing()).unwrap();
    let theirs = ocpi_kit::v2_3_0::credentials::Credentials::builder()
        .token(OcpiString::<64>::new(TOKEN_B).unwrap())
        .url(KitUrl::new_lenient(format!(
            "http://{partner}/ocpi/versions"
        )))
        .roles(vec![
            ocpi_kit::v2_3_0::credentials::CredentialsRole::builder()
                .role(ocpi_kit::v2_3_0::types::Role::Emsp)
                .business_details(
                    ocpi_kit::v2_3_0::locations::BusinessDetails::builder()
                        .name("MSP Mobility GmbH")
                        .build(),
                )
                .party_id(CiString::new("MSP").unwrap())
                .country_code(CiString::new("DE").unwrap())
                .build(),
        ])
        .build();
    ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{roamd}/ocpi/versions")),
        CredentialsToken::new(TOKEN_A).unwrap(),
    )
    .discover(client.transport())
    .await
    .expect("roamd publishes its versions to a Token A")
    .select_best(client.transport())
    .await
    .expect("and the details")
    .register(client.transport(), &theirs)
    .await
    .expect("the handshake completes");
}

// ── waiting, and looking ────────────────────────────────────────────────────

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn received(billd: &billd::daemon::Running) -> Vec<billd::Received> {
    billd
        .books
        .lock()
        .unwrap()
        .state()
        .received()
        .cloned()
        .collect()
}

fn consignments(roamd: &roamd::daemon::Running) -> Vec<roamd::Consignment> {
    roamd
        .node
        .lock()
        .unwrap()
        .state()
        .consignments()
        .cloned()
        .collect()
}

fn outbox(csmsd: &csmsd::daemon::Running) -> Vec<(Target, DeliveryState)> {
    csmsd.csmsd.with_state(|state| {
        state
            .outbox
            .values()
            .map(|(delivery, state)| (delivery.to, state.clone()))
            .collect()
    })
}

fn delivered(csmsd: &csmsd::daemon::Running, to: Target) -> bool {
    outbox(csmsd)
        .iter()
        .any(|(target, state)| *target == to && matches!(state, DeliveryState::Delivered { .. }))
}

fn kwh(text: &str) -> Decimal {
    Decimal::from_str_exact(text.trim_matches('"'))
        .unwrap()
        .normalize()
}

async fn get(url: &str, token: Option<&str>) -> (u16, String) {
    let mut request = reqwest::Client::new().get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

// ── the story ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_becomes_an_invoice_line_and_a_consignment_with_the_same_energy() {
    // SC-001: the whole story, in well under thirty seconds.
    tokio::time::timeout(Duration::from_secs(30), async {
        let estate = Estate::new("story");
        let (peer, peer_addr) = partner().await;
        let empd = estate.empd().await;
        let billd = estate.billd().await;
        let roamd = estate.roamd().await;
        register(estate.roamd, peer_addr).await;
        let csmsd = estate.csmsd().await;
        assert!(
            csmsd.readiness.is_ready(),
            "{:?}",
            csmsd.readiness.blockers()
        );

        let station = station(csmsd.ocpp).await;
        let started = charge(&station, GOOD_CARD, &[]).await;
        assert_eq!(
            started.id_tag_info.status,
            v1_6::AuthorizationStatus::Accepted
        );

        until("both deliveries", || {
            delivered(&csmsd, Target::Billd) && delivered(&csmsd, Target::Roamd)
        })
        .await;
        until("the partner's platform to hold the record", || {
            peer.cdrs.len() == 1
        })
        .await;
        until("roamd to record the partner's answer", || {
            consignments(&roamd)
                .iter()
                .all(|consignment| consignment.delivery.is_accepted())
        })
        .await;

        // One record in csmsd, received once by each.
        let cdr = csmsd
            .csmsd
            .with_cdrs(|ledger| ledger.iter().next().cloned())
            .expect("the session settled");
        let in_billd = received(&billd);
        let in_roamd = consignments(&roamd);
        assert_eq!(in_billd.len(), 1, "exactly once in billd");
        assert_eq!(in_roamd.len(), 1, "exactly once in roamd");
        assert_eq!(
            in_billd[0].cdr, cdr,
            "billd holds the record csmsd built, whole"
        );
        assert_eq!(in_roamd[0].key, cdr.key);
        assert_eq!(
            in_billd[0].partner.to_string(),
            "DE*MSP",
            "whose contract it was"
        );

        // The same decimal at the far end of both paths.
        let document = roamd
            .node
            .lock()
            .unwrap()
            .state()
            .document(&cdr.key)
            .cloned()
            .expect("the document the partner was sent");
        let roamd_energy = match &document {
            emob_roam::ocpi::cdr::Outbound::V2_3_0(cdr) => {
                kwh(&serde_json::to_string(&cdr.total_energy).unwrap())
            }
            emob_roam::ocpi::cdr::Outbound::V2_2_1(cdr) => {
                kwh(&serde_json::to_string(&cdr.total_energy).unwrap())
            }
            other => panic!("a document in a version this test does not read: {other:?}"),
        };
        let pushed = peer.cdrs.all();
        let peer_energy = kwh(&serde_json::to_string(&pushed[0].total_energy).unwrap());
        assert_eq!(roamd_energy, cdr.total_energy.kwh().normalize());
        assert_eq!(peer_energy, roamd_energy, "the partner holds what was sent");

        // billd's next closing carries it as a line.
        let number = {
            let mut books = billd.books.lock().unwrap();
            let partner = &billd.estate.partners[0];
            let closing = billd::daemon::closing(
                &billd.estate,
                partner,
                2026,
                time::Month::November,
                time::macros::date!(2026 - 12 - 01),
            );
            books
                .close(&emob_service::Actor::Timer, &closing)
                .expect("the month closes")
                .expect("a document")
        };
        let invoice = billd
            .books
            .lock()
            .unwrap()
            .state()
            .issued(&number)
            .expect("issued")
            .invoice
            .clone();
        assert_eq!(invoice.records(), vec![&cdr.key], "one line, the record");
        assert_eq!(
            in_billd[0].cdr.total_energy.kwh().normalize(),
            roamd_energy,
            "the same decimal in the invoice and the consignment"
        );

        // Spec 003: the invoice line carries the driver's receipt, and the
        // receipt opens on csmsd's shell with the CDR's own figures.
        let path = csmsd
            .csmsd
            .with_state(|state| {
                state
                    .receipts
                    .values()
                    .next()
                    .map(|issued| issued.key.clone())
            })
            .and_then(|key| csmsd.csmsd.receipt_path(&key))
            .expect("the settled session has a receipt");
        assert!(
            invoice.lines.iter().all(|line| line
                .receipt
                .as_deref()
                .is_some_and(|link| link.ends_with(&path))),
            "every line of the record links its receipt: {:?}",
            invoice.lines.iter().map(|l| &l.receipt).collect::<Vec<_>>()
        );
        let (status, page) = get(&format!("http://{}{path}", estate.csmsd_http), None).await;
        assert_eq!(status, 200, "a receipt needs no login");
        assert!(page.contains("Gemessen, signiert, geprüft"), "{page}");
        let energy = cdr.total_energy.kwh().to_string().replace('.', ",");
        assert!(page.contains(&format!("{energy} kWh")), "{energy}: {page}");
        let gross = cdr
            .cost
            .as_ref()
            .unwrap()
            .gross()
            .amount()
            .to_string()
            .replace('.', ",");
        assert!(page.contains(&format!("{gross} €")), "{gross}");
        let token = path.trim_start_matches("/r/");
        let journal = std::fs::read_to_string(estate.dir.join("csmsd.ndjson")).unwrap();
        assert!(
            !journal.contains(token),
            "SR-060: csmsd's journal never holds the token"
        );

        // The bounded views, behind the operator token (SR-034, SR-094); the
        // whole-history ones are gone.
        for (daemon, name) in [
            (estate.csmsd_http, "csmsd"),
            (estate.billd, "billd"),
            (estate.roamd, "roamd"),
        ] {
            let (status, _) = get(&format!("http://{daemon}/v1/{name}/today"), None).await;
            assert_eq!(status, 401, "{name}");
            let (status, _) = get(&format!("http://{daemon}/v1/{name}"), Some(OPERATOR)).await;
            assert_eq!(status, 404, "{name}: the unbounded view is removed");
            // SR-088 / SR-089: a limit past the bound and an unknown parameter
            // are refused, not clamped.
            for query in ["limit=201", "limit=0", "page=2"] {
                let (status, body) = get(
                    &format!("http://{daemon}/v1/{name}/today?{query}"),
                    Some(OPERATOR),
                )
                .await;
                assert_eq!(status, 400, "{name}?{query}: {body}");
            }
        }
        // The invoice is issued and not yet accepted: it is waiting.
        let (status, view) = get(
            &format!("http://{}/v1/billd/today", estate.billd),
            Some(OPERATOR),
        )
        .await;
        assert_eq!(status, 200);
        assert!(view.contains(&number), "{view}");
        assert!(view.contains(r#""received":1"#), "{view}");
        let (status, view) = get(
            &format!("http://{}/v1/csmsd/today", estate.csmsd_http),
            Some(OPERATOR),
        )
        .await;
        assert_eq!(status, 200);
        assert!(!view.contains(GOOD_CARD), "no uid in a read model: {view}");
        assert!(view.contains(r#""billed":1"#), "today's success: {view}");
        let (status, view) = get(
            &format!("http://{}/v1/roamd/today", estate.roamd),
            Some(OPERATOR),
        )
        .await;
        assert_eq!(status, 200);
        assert!(view.contains(r#""registered":true"#), "{view}");

        // SR-048 / SR-046 / SR-044 / SR-087: the card reached billd and
        // roamd, and no journal holds it — nor the partner's tokens — in the
        // clear.
        for journal in ["csmsd", "empd", "roamd", "billd"] {
            let text = std::fs::read_to_string(estate.dir.join(format!("{journal}.ndjson")))
                .expect("the journal is on disk");
            assert!(
                !text.contains(GOOD_CARD),
                "{journal}.ndjson holds the card's uid"
            );
            for token in [TOKEN_A, TOKEN_B] {
                assert!(!text.contains(token), "{journal}.ndjson holds {token}");
            }
        }

        station.shutdown(Duration::from_secs(1)).await;
        csmsd.kill();
        roamd.kill();
        billd.kill();
        empd.kill();
    })
    .await
    .expect("SC-001: the story runs in under thirty seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_031_a_receiver_that_was_down_gets_the_record_once_it_is_up() {
    // US1.2: billd is down when the session closes.
    let estate = Estate::new("billd-down");
    let empd = estate.empd().await;
    let roamd = estate.roamd().await;
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    charge(&station, GOOD_CARD, &[]).await;

    until("roamd's delivery", || delivered(&csmsd, Target::Roamd)).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !delivered(&csmsd, Target::Billd),
        "nobody answered, so nothing was recorded"
    );

    let billd = estate.billd().await;
    until("billd's delivery", || delivered(&csmsd, Target::Billd)).await;
    assert_eq!(received(&billd).len(), 1, "it lands once");

    station.shutdown(Duration::from_secs(1)).await;
    csmsd.kill();
    roamd.kill();
    billd.kill();
    empd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_031_a_csmsd_killed_with_a_delivery_owed_delivers_it_after_the_restart() {
    // SC-002: kill csmsd after the session settled and before billd answered.
    let estate = Estate::new("csmsd-restart");
    let empd = estate.empd().await;
    let roamd = estate.roamd().await;
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    charge(&station, GOOD_CARD, &[]).await;
    until("roamd's delivery", || delivered(&csmsd, Target::Roamd)).await;
    station.shutdown(Duration::from_secs(1)).await;
    csmsd.kill();

    let billd = estate.billd().await;
    let csmsd = estate.csmsd().await;
    assert_eq!(csmsd.csmsd.settled(), 1, "the record is still settled");
    until("billd's delivery from the restarted outbox", || {
        delivered(&csmsd, Target::Billd)
    })
    .await;
    assert_eq!(received(&billd).len(), 1);
    assert_eq!(
        consignments(&roamd).len(),
        1,
        "and roamd was not sent it twice"
    );

    csmsd.kill();
    roamd.kill();
    billd.kill();
    empd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_030_a_receiver_restarted_after_the_receipt_takes_a_redelivery_as_a_duplicate() {
    let estate = Estate::new("receiver-restart");
    let empd = estate.empd().await;
    let billd = estate.billd().await;
    let roamd = estate.roamd().await;
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    charge(&station, GOOD_CARD, &[]).await;
    until("both deliveries", || {
        delivered(&csmsd, Target::Billd) && delivered(&csmsd, Target::Roamd)
    })
    .await;
    billd.kill();
    roamd.kill();

    // Both come back from their journals, and the same delivery — signed now,
    // as a retry would be — changes nothing.
    let billd = estate.billd().await;
    let roamd = estate.roamd().await;
    assert_eq!(received(&billd).len(), 1, "billd folded its receipt");
    assert_eq!(
        consignments(&roamd).len(),
        1,
        "roamd folded its consignment"
    );
    let deliveries: Vec<csmsd::Delivery> = csmsd
        .csmsd
        .with_state(|state| state.outbox.values().map(|(d, _)| d.clone()).collect());
    let dispatcher = csmsd::outbox::Dispatcher::new(
        &format!("http://{}", estate.billd),
        &format!("http://{}", estate.roamd),
        emob_service::webhook::Secret::standard(SECRET).unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    for delivery in &deliveries {
        assert_eq!(
            dispatcher
                .attempt(delivery, time::OffsetDateTime::now_utc())
                .await,
            csmsd::outbox::Attempt::Delivered
        );
    }
    assert_eq!(received(&billd).len(), 1, "a duplicate, not a second line");
    assert_eq!(
        consignments(&roamd).len(),
        1,
        "a duplicate, not a second consignment"
    );

    station.shutdown(Duration::from_secs(1)).await;
    csmsd.kill();
    roamd.kill();
    billd.kill();
    empd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_029_a_delivery_signed_with_another_secret_is_refused_and_counted() {
    let estate = Estate::new("forged");
    let billd = estate.billd().await;
    let roamd = estate.roamd().await;
    let forger = csmsd::outbox::Dispatcher::new(
        &format!("http://{}", estate.billd),
        &format!("http://{}", estate.roamd),
        emob_service::webhook::Secret::raw("not the estate's secret"),
        Duration::from_secs(5),
    )
    .unwrap();
    for to in Target::ALL {
        let attempt = forger
            .attempt(
                &csmsd::Delivery {
                    id: "DE*ABC/CP-1-1".to_owned(),
                    to,
                    body: "{}".to_owned(),
                },
                time::OffsetDateTime::now_utc(),
            )
            .await;
        assert!(
            matches!(&attempt, csmsd::outbox::Attempt::Again(why) if why.starts_with("401")),
            "{attempt:?}"
        );
    }
    assert!(
        received(&billd).is_empty(),
        "nothing a forger sends reaches the books"
    );
    assert!(consignments(&roamd).is_empty());
    for daemon in [estate.billd, estate.roamd] {
        let name = if daemon == estate.billd {
            "billd"
        } else {
            "roamd"
        };
        let (_, view) = get(&format!("http://{daemon}/v1/{name}/today"), Some(OPERATOR)).await;
        assert!(view.contains(r#""refused_deliveries":1"#), "{view}");
    }
    billd.kill();
    roamd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_038_an_oversized_delivery_is_refused_before_it_is_read() {
    let estate = Estate::new("oversized");
    let billd = estate.billd().await;
    // Headers announcing two MiB, and no body: the refusal has to come before
    // anything is read.
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(estate.billd).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /v1/deliveries HTTP/1.1\r\nhost: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                estate.billd,
                2 * 1024 * 1024
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut head = [0_u8; 12];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"HTTP/1.1 413");
    assert!(received(&billd).is_empty());
    billd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_whose_evidence_fails_never_leaves_csmsd_and_says_why() {
    // US1.3: a digit changed after signing.
    let estate = Estate::new("tampered");
    let empd = estate.empd().await;
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    charge(&station, GOOD_CARD, &[Fault::TamperedValue]).await;
    station.shutdown(Duration::from_secs(1)).await;

    assert_eq!(csmsd.csmsd.settled(), 0);
    assert!(outbox(&csmsd).is_empty(), "no CDR leaves csmsd");
    let outcomes = csmsd.csmsd.outcomes();
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            Outcome::Refused { reasons, .. }
                if reasons.iter().any(|reason| reason.contains("signature does not verify"))
        )),
        "the refusal is journalled with its reasons: {outcomes:?}"
    );
    csmsd.kill();
    empd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_the_provider_blocked_is_answered_blocked_and_opens_no_transaction() {
    // US2.1, SR-006.
    let estate = Estate::new("blocked");
    let empd = estate.empd().await;
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    let started = charge(&station, BLOCKED_CARD, &[]).await;
    assert_eq!(
        started.id_tag_info.status,
        v1_6::AuthorizationStatus::Blocked
    );
    station.shutdown(Duration::from_secs(1)).await;

    csmsd.csmsd.with_state(|state| {
        assert!(state.inflight.is_empty(), "no transaction opened");
        assert!(
            state
                .answers
                .iter()
                .all(|(_, answer)| answer.status == Status::Blocked)
        );
    });
    csmsd.kill();
    empd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_032_an_empd_that_does_not_answer_is_an_invalid_with_the_reason_journalled() {
    // US2.2: fail closed. Nobody listens on the estate's empd port.
    let estate = Estate::new("empd-down");
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    let started = charge(&station, GOOD_CARD, &[]).await;
    assert_eq!(
        started.id_tag_info.status,
        v1_6::AuthorizationStatus::Invalid
    );
    station.shutdown(Duration::from_secs(1)).await;

    let journalled = csmsd.csmsd.with_journal(|journal| {
        journal
            .records()
            .unwrap()
            .into_iter()
            .filter(|record| record.kind == "authorized")
            .map(|record| serde_json::from_str::<Event>(&record.body).unwrap())
            .collect::<Vec<_>>()
    });
    assert!(!journalled.is_empty());
    for event in journalled {
        let Event::Authorized { answer, .. } = event else {
            unreachable!("filtered by kind")
        };
        assert_eq!(answer.status, Status::Invalid);
        assert!(answer.reason.contains("empd"), "{}", answer.reason);
    }
    assert!(csmsd.csmsd.with_state(|state| state.inflight.is_empty()));
    csmsd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sr_036_an_estate_with_no_meter_keys_is_not_ready_and_says_why() {
    // US3.2, SR-005.
    let estate = Estate::new("no-keys");
    let mut sections: csmsd::estate::Estate = estate.sections();
    sections.meter_keys.clear();
    let csmsd = estate.csmsd_with(sections).await;
    assert!(!csmsd.readiness.is_ready());
    let (status, body) = get(&format!("http://{}/health/ready", estate.csmsd_http), None).await;
    assert_eq!(status, 503);
    assert!(body.contains("key registry is empty"), "{body}");
    csmsd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_connection_under_an_identity_already_connected_supersedes_the_first() {
    // Spec edge case, as corrected: `ocpp-kit` closes the older session, so a
    // station reconnecting after a partition is not locked out by its own
    // zombie. Telling an impostor from that reconnect is SR-002's credential
    // check (spec 005), not this.
    let estate = Estate::new("twins");
    let empd = estate.empd().await;
    let csmsd = estate.csmsd().await;
    let first = station(csmsd.ocpp).await;
    let second = station(csmsd.ocpp).await;
    assert!(
        second
            .handle()
            .call(v1_6::HeartbeatRequest::new())
            .await
            .is_ok(),
        "the newer connection is the station"
    );
    let older = tokio::time::timeout(
        Duration::from_secs(8),
        first.handle().call(v1_6::HeartbeatRequest::new()),
    )
    .await;
    assert!(
        !matches!(older, Ok(Ok(_))),
        "and the older one no longer speaks for it"
    );
    second.shutdown(Duration::from_secs(1)).await;
    first.shutdown(Duration::from_secs(1)).await;
    csmsd.kill();
    empd.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_roamd_restarted_before_the_partner_registered_pushes_the_record_once_after() {
    // SC-002 for roamd: the consignment is a fact, so a restart between the
    // receipt and the push loses nothing and sends nothing twice.
    let estate = Estate::new("roamd-restart");
    let (peer, peer_addr) = partner().await;
    let empd = estate.empd().await;
    let billd = estate.billd().await;
    let roamd = estate.roamd().await;
    let csmsd = estate.csmsd().await;
    let station = station(csmsd.ocpp).await;
    charge(&station, GOOD_CARD, &[]).await;
    until("roamd's delivery", || delivered(&csmsd, Target::Roamd)).await;
    roamd.kill();

    let roamd = estate.roamd().await;
    register(estate.roamd, peer_addr).await;
    until("the partner to hold the record", || peer.cdrs.len() == 1).await;
    until("roamd to record the answer", || {
        consignments(&roamd)
            .iter()
            .all(|consignment| consignment.delivery.is_accepted())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        peer.cdrs.len(),
        1,
        "an accepted record is sealed and not pushed again"
    );

    station.shutdown(Duration::from_secs(1)).await;
    csmsd.kill();
    roamd.kill();
    billd.kill();
    empd.kill();
}
