//! Spec 005: only the station that knows its password — or holds its
//! certificate — connects, over TLS, within its budget.
//!
//! The real `csmsd` (`daemon::run`) with the demo estate, pointed at the
//! throwaway PKI in `tests/fixtures/tls/` (minted by `mint.sh` with openssl, not
//! by the code under test). The station is `ocpp-kit`'s own `Station` with its
//! own TLS client: somebody else's implementation of the other end.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use csmsd::daemon::{Options, Running, run};
use csmsd::edge::Counters;
use emob_service::estate::{self, Env, SecurityProfile};
use emob_service::{Identity, Shutdown};
use ocpp_kit::transport::{BasicAuthPassword, ClientTls, Handle, SecurityProfile as Wire, Station};
use ocpp_kit::{RawValue, Version, v1_6};

const DEMO: &str = include_str!("../../../deploy/estate.example.toml");
/// The demo station's `AuthorizationKey`: hex of `mock-station-secret`.
const DEMO_KEY: &str = "6d6f636b2d73746174696f6e2d736563726574";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

fn env() -> Env {
    let values: BTreeMap<&str, String> = [
        (
            "EMOB_WEBHOOK_SECRET",
            "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw".to_owned(),
        ),
        ("EMOB_SERVICE_TOKEN", "service-token".to_owned()),
        ("EMOB_OPERATOR_TOKEN", "operator-token".to_owned()),
        ("CSMSD_DATA_KEY", emob_service::vault::DataKey::generate()),
        (
            "CSMSD_SIGNING_KEY",
            emob_service::head::SigningKey::generate(),
        ),
    ]
    .into_iter()
    .collect();
    Arc::new(move |name| values.get(name).cloned())
}

/// The demo estate with `[csms]` pointed at the fixtures.
fn demo() -> csmsd::estate::Estate {
    let mut estate: csmsd::estate::Estate = estate::from_str(DEMO).expect("the demo estate");
    estate.csms.tls_cert = Some(fixture("server.pem"));
    estate.csms.tls_key = Some(fixture("server.key"));
    estate.csms.client_ca = Some(fixture("ca.pem"));
    estate
}

async fn start(estate: csmsd::estate::Estate) -> anyhow::Result<Running> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "emob-005-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    run(Options {
        ocpp: "127.0.0.1:0".parse().unwrap(),
        http: "127.0.0.1:0".parse().unwrap(),
        journal: dir.join("csmsd.ndjson"),
        estate,
        env: env(),
        shutdown: Shutdown::new(),
        identity: Identity::new("csmsd", "test"),
        clock: emob_service::system_clock(),
    })
    .await
}

struct Quiet;

impl ocpp_kit::transport::Handler for Quiet {
    fn on_request(
        &self,
        _ctx: ocpp_kit::transport::Ctx,
        request: ocpp_kit::engine::IncomingRequest,
    ) -> ocpp_kit::transport::BoxFuture<'_, Result<Box<RawValue>, ocpp_kit::rpc::CallError>> {
        Box::pin(async move { Err(ocpp_kit::rpc::CallError::not_supported(&request.action)) })
    }
}

enum Presents<'a> {
    Password(&'a str),
    Certificate(&'a str),
    /// Its identity in the URL and nothing else.
    Nothing,
}

fn station(running: &Running, tls: bool, identity: &str, presents: Presents<'_>) -> Handle {
    let scheme = if tls { "wss" } else { "ws" };
    let mut builder = Station::builder()
        .identity(identity)
        .unwrap()
        .url(format!("{scheme}://127.0.0.1:{}/ocpp", running.ocpp.port()))
        .versions([Version::V1_6])
        .reconnect(false)
        .ping_interval(None)
        .handler(Quiet);
    let mut client = ClientTls::builder().root_file(fixture("ca.pem")).unwrap();
    builder = match presents {
        Presents::Password(key) => builder
            .security_profile(if tls {
                Wire::TlsBasicAuth
            } else {
                Wire::BasicAuth
            })
            .password(BasicAuthPassword::hex(key).unwrap()),
        Presents::Nothing => builder.security_profile(Wire::Unsecured),
        Presents::Certificate(name) => {
            client = client
                .client_certificate(
                    fixture(&format!("{name}.pem")),
                    fixture(&format!("{name}.key")),
                )
                .unwrap();
            builder.security_profile(Wire::TlsClientCertificate)
        }
    };
    if tls {
        builder = builder.tls(client.build().unwrap());
    }
    builder.build().unwrap().spawn().unwrap()
}

async fn boots(handle: &Handle) -> bool {
    let boot = v1_6::BootNotificationRequest::new("Model-1".to_owned(), "ACME".to_owned());
    matches!(
        tokio::time::timeout(Duration::from_secs(5), handle.call(boot)).await,
        Ok(Ok(_))
    )
}

async fn rises(counter: &AtomicU64) -> bool {
    for _ in 0..100 {
        if Counters::get(counter) > 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn sr_040_a_profile_2_station_with_its_password_is_admitted_over_tls_and_a_wrong_one_is_refused()
 {
    let running = start(demo()).await.expect("csmsd starts with TLS");
    let good = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(boots(&good).await, "the right password, over TLS");
    good.shutdown(Duration::from_secs(1)).await;

    let wrong = station(
        &running,
        true,
        "CP-1",
        Presents::Password("00112233445566778899aabbccddeeff"),
    );
    assert!(!boots(&wrong).await, "a wrong password is not admitted");
    assert!(
        rises(&running.edge.counters.refused_credentials).await,
        "SR-040: the 401 is counted"
    );
    running.kill();
}

#[tokio::test]
async fn sr_042_a_profile_2_station_on_a_plain_socket_is_refused_even_with_its_password() {
    let mut estate = demo();
    estate.csms.tls_cert = None;
    estate.csms.tls_key = None;
    estate.csms.client_ca = None;
    let running = start(estate).await.expect("loopback without TLS starts");
    let plain = station(&running, false, "CP-1", Presents::Password(DEMO_KEY));
    assert!(!boots(&plain).await, "profile 2 means TLS");
    assert!(rises(&running.edge.counters.refused_credentials).await);
    running.kill();
}

#[tokio::test]
async fn sr_042_a_public_bind_without_tls_refuses_to_start_naming_the_setting() {
    let mut estate = demo();
    estate.csms.tls_cert = None;
    estate.csms.tls_key = None;
    estate.csms.client_ca = None;
    let error = run(Options {
        ocpp: "0.0.0.0:0".parse().unwrap(),
        http: "127.0.0.1:0".parse().unwrap(),
        // Per run: a signed head another run left behind is under another key.
        journal: std::env::temp_dir().join(format!("emob-005-never-{}.ndjson", std::process::id())),
        estate,
        env: env(),
        shutdown: Shutdown::new(),
        identity: Identity::new("csmsd", "test"),
        clock: emob_service::system_clock(),
    })
    .await
    .err()
    .expect("csmsd refuses");
    let text = format!("{error:#}");
    assert!(text.contains("csms.tls_cert"), "{text}");
    assert!(text.contains("tls_terminated_by_proxy"), "{text}");
}

fn profile_3() -> csmsd::estate::Estate {
    let mut estate = demo();
    estate.stations[0].security_profile = SecurityProfile::TlsClientCertificate;
    estate.stations[0].password_hash = None;
    estate
}

#[tokio::test]
async fn sr_041_a_profile_3_station_is_admitted_by_its_own_certificate() {
    let running = start(profile_3()).await.unwrap();
    let cp1 = station(&running, true, "CP-1", Presents::Certificate("cp-1"));
    assert!(boots(&cp1).await, "CN=CP-1 under the configured CA");
    running.kill();
}

#[tokio::test]
async fn sr_041_a_certificate_naming_another_station_is_refused() {
    let running = start(profile_3()).await.unwrap();
    let impostor = station(&running, true, "CP-1", Presents::Certificate("cp-2"));
    assert!(!boots(&impostor).await, "CN=CP-2 presented as CP-1");
    assert!(rises(&running.edge.counters.refused_credentials).await);
    running.kill();
}

#[tokio::test]
async fn sr_041_a_certificate_from_another_ca_is_refused() {
    let running = start(profile_3()).await.unwrap();
    let rogue = station(&running, true, "CP-1", Presents::Certificate("rogue-cp-1"));
    assert!(
        !boots(&rogue).await,
        "the TLS handshake refuses a foreign chain"
    );
    running.kill();
}

#[tokio::test]
async fn sr_041_a_profile_3_station_without_a_certificate_is_refused() {
    let running = start(profile_3()).await.unwrap();
    let bare = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(!boots(&bare).await, "a password is not a certificate");
    assert!(rises(&running.edge.counters.refused_credentials).await);
    running.kill();
}

#[tokio::test]
async fn sr_043_a_connection_past_the_cap_is_refused_and_counted() {
    let mut estate = demo();
    estate.csms.max_connections = 1;
    let running = start(estate).await.unwrap();
    let first = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(boots(&first).await);
    let second = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(!boots(&second).await, "the cap is one session");
    assert!(
        rises(&running.edge.counters.refused_capacity).await,
        "SR-043: the 503 is counted"
    );
    running.kill();
}

#[tokio::test]
async fn sr_043_a_message_past_the_budget_is_refused_and_counted() {
    let mut estate = demo();
    estate.csms.messages_per_minute = 2;
    let running = start(estate).await.unwrap();
    let chatty = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(boots(&chatty).await, "the first message");
    let heartbeat = || chatty.call(v1_6::HeartbeatRequest::new());
    assert!(heartbeat().await.is_ok(), "the second message");
    assert!(
        heartbeat().await.is_err(),
        "the third, at once, is over budget"
    );
    assert_eq!(Counters::get(&running.edge.counters.rate_limited), 1);
    running.kill();
}

#[tokio::test]
async fn sr_043_a_silent_session_is_closed_and_counted() {
    let mut estate = demo();
    estate.csms.idle_timeout_secs = 1;
    let running = start(estate).await.unwrap();
    let silent = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(boots(&silent).await);
    assert!(
        rises(&running.edge.counters.idle_closed).await,
        "closed after a second of silence"
    );
    running.kill();
}

/// CISA ICSA-26-062-08 (2026): a charging backend admitted a WebSocket by the
/// station identity in its URL alone. Here an identity is never a credential:
/// the same identity with no password, over TLS, is refused and counted.
#[tokio::test]
async fn icsa_26_062_08_an_identity_alone_is_never_a_credential() {
    // A plain loopback listener, so the bare connection reaches the gate at
    // all; the kit refuses to send no credential over TLS on its own.
    let mut estate = demo();
    estate.csms.tls_cert = None;
    estate.csms.tls_key = None;
    estate.csms.client_ca = None;
    let running = start(estate).await.expect("loopback without TLS starts");
    let bare = station(&running, false, "CP-1", Presents::Nothing);
    assert!(!boots(&bare).await, "an identity is not a credential");
    assert!(rises(&running.edge.counters.refused_credentials).await);
    running.kill();
}

/// CVE-2026-44383 (2026): a second connection under a live identity took the
/// station's place and starved it. A newer connection supersedes only after it
/// has proven the credential; an impostor without it is refused, and the
/// station it impersonated keeps speaking.
#[tokio::test]
async fn cve_2026_44383_a_second_connection_without_the_credential_cannot_take_the_identity() {
    let running = start(demo()).await.expect("csmsd starts with TLS");
    let real = station(&running, true, "CP-1", Presents::Password(DEMO_KEY));
    assert!(boots(&real).await, "the real station is admitted");
    let impostor = station(
        &running,
        true,
        "CP-1",
        Presents::Password("00112233445566778899aabbccddeeff"),
    );
    assert!(!boots(&impostor).await, "the impostor is refused");
    assert!(
        real.call(ocpp_kit::v1_6::HeartbeatRequest::new())
            .await
            .is_ok(),
        "and the station it impersonated still speaks"
    );
    real.shutdown(Duration::from_secs(1)).await;
    running.kill();
}
