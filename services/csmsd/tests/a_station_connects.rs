//! M2c: a station connects, and the kilowatt-hour it signed reaches a CDR.
//!
//! Both ends are real. `ocpp-kit`'s `Station` speaks to `ocpp-kit`'s `Csms`
//! over a real TCP WebSocket with a real handshake, a real subprotocol
//! negotiation and a real RPC engine; `csmsd` is the handler on the other side,
//! running the same chain `emob-sim` drives. The only thing imaginary is the
//! hardware — and the record it signs is not imaginary either: it is the Open
//! Charge Alliance's own example message, from a DZG GSH01.1K2L.
//!
//! What the tests assert is the seam. The OCPP `meterStop` the station sends is
//! **108814** — the meter's lifetime register, in watt-hours — and the CDR that
//! comes out the other end bills **0.636 kWh**, the signed transaction
//! difference. Nothing between the socket and the ledger can see the first
//! number, because `emob_ocpp::TransactionEvent` has no field for it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use csmsd::{ChargePoint, Csmsd, Outcome, Provisioning};
use emob_core::{Currency, PartyId};
use emob_eichrecht::registry::{ComponentRef, KeyRegistry, RegisteredKey};
use emob_ocpp::fixtures::{OCA_1_6_SAMPLED_VALUE, OCA_KEY_HEX};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::PublicKey;
use ocpp_kit::transport::{
    Auth, AuthOutcome, BasicAuthPassword, Csms, Handle, SecurityProfile, SessionContext, Station,
};
use ocpp_kit::types::{DateTime, Identity};
use ocpp_kit::{RawValue, Version, v1_6};
use rust_decimal::Decimal;

/// What the OCPP message says the lifetime register read — the number a CSMS
/// billing the protocol would have used.
const OCPP_METER_STOP_WH: i32 = 108_814;

/// A station that answers nothing, because these tests never ask it anything.
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

/// The handler behind an `Arc`, so a test can read the outcomes afterwards.
struct Shared(Arc<Csmsd>);

impl ocpp_kit::transport::Handler for Shared {
    fn on_request(
        &self,
        ctx: ocpp_kit::transport::Ctx,
        request: ocpp_kit::engine::IncomingRequest,
    ) -> ocpp_kit::transport::BoxFuture<'_, Result<Box<RawValue>, ocpp_kit::rpc::CallError>> {
        self.0.on_request(ctx, request)
    }
}

fn provisioning() -> Provisioning {
    Provisioning::new().with(
        Identity::new("CP-1").unwrap(),
        ChargePoint {
            evse_id: "DE*ABC*E00001".parse().unwrap(),
            rated_power_kw: Decimal::from(150),
        },
    )
}

/// The key out of band — a type approval, never the socket.
fn registry() -> KeyRegistry {
    registry_holding(OCA_KEY_HEX, "type approval — DZG GSH01.1K2L")
}

fn registry_holding(key_hex: &str, provenance: &str) -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    registry
        .insert(
            ComponentRef::Meter {
                serial: "1DZG0028225179".into(),
            },
            RegisteredKey::unbounded(
                PublicKey::from_text(key_hex, Some(ocmf::Curve::Secp256k1)).unwrap(),
                provenance,
            ),
        )
        .unwrap();
    registry
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

/// The `StopTransaction.req` the OCA publishes, as a station would send it.
///
/// The signed data goes in a sampled value with context `Transaction.End`,
/// inside `transactionData` `[OCA SMV §3.1]` — and `meterStop` beside it is the
/// lifetime register the protocol carries and nothing bills.
fn stop_transaction(transaction_id: i32) -> v1_6::StopTransactionRequest {
    v1_6::StopTransactionRequest::new(
        OCPP_METER_STOP_WH,
        DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
        transaction_id,
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
    )])
}

/// A CSMS on an ephemeral port, and the task serving it.
async fn serve(csmsd: Arc<Csmsd>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let fleet = provisioning();
    let csms = Csms::builder()
        .bind("127.0.0.1:0".parse().unwrap())
        .versions([Version::V1_6])
        .authenticate(move |auth: Auth| {
            // An identity nobody provisioned is answered 404 rather than 401,
            // so an operator can tell a typo from a bad password
            // `[OCPP 2.0.1 Part 4 §3.1.1]`.
            let point = fleet.get(&auth.identity).cloned();
            async move {
                match point {
                    Some(point) => AuthOutcome::Accept(SessionContext::new(point)),
                    None => AuthOutcome::Unknown,
                }
            }
        })
        .handler(Shared(csmsd))
        .build()
        .expect("a CSMS");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let addr = listener.local_addr().expect("an address");
    let task = tokio::spawn(async move {
        let _ = csms.serve_on(listener).await;
    });
    (addr, task)
}

/// A real station, on the other end of a real socket.
fn connect(addr: &SocketAddr, identity: &str) -> Handle {
    Station::builder()
        .identity(identity)
        .expect("a valid identity")
        .url(format!("ws://{addr}/ocpp"))
        .versions([Version::V1_6])
        // Security profile 1 over loopback: the transport still runs the whole
        // handshake, and TLS is `csmsd`'s configuration rather than its logic.
        .security_profile(SecurityProfile::BasicAuth)
        .password(BasicAuthPassword::utf8("0123456789abcdef").expect("a password"))
        .reconnect(false)
        .handler(QuietStation)
        .build()
        .expect("a station")
        .spawn()
        .expect("the station connects")
}

fn boot() -> v1_6::BootNotificationRequest {
    v1_6::BootNotificationRequest::new("GSH01.1K2L".to_owned(), "DZG".to_owned())
}

/// Boot, open a transaction, close it with the OCA's signed data.
async fn run_one_session(handle: &Handle) {
    handle.call(boot()).await.expect("the CSMS answers a boot");
    handle.wait_ready().await;

    // 1.6 has the CSMS assign the transaction id, in the *response*.
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            "HRWWBX8".to_owned(),
            0,
            DateTime::parse("2023-05-19T13:52:39Z").unwrap(),
        ))
        .await
        .expect("the CSMS answers a start");
    assert_eq!(
        started.id_tag_info.status,
        v1_6::AuthorizationStatus::Accepted
    );

    handle
        .call(stop_transaction(started.transaction_id))
        .await
        .expect("the CSMS answers a stop");
    handle.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_station_connects_and_its_signed_kilowatt_hours_reach_a_cdr() {
    let csmsd = Arc::new(Csmsd::new(
        PartyId::new("DE", "ABC").unwrap(),
        registry(),
        tariff(),
    ));
    let (addr, server) = serve(Arc::clone(&csmsd)).await;
    run_one_session(&connect(&addr, "CP-1")).await;
    server.abort();

    let outcomes = csmsd.outcomes();
    assert_eq!(
        outcomes.len(),
        1,
        "one transaction, one outcome: {outcomes:?}"
    );
    match &outcomes[0] {
        Outcome::Settled {
            energy,
            key,
            retries,
        } => {
            assert_eq!(
                energy, "0.636 kWh",
                "the signed transaction difference, not the protocol's register"
            );
            assert!(key.contains("CP-1"), "{key}");
            assert_eq!(*retries, 0, "a quiet link retries nothing");
        }
        other => panic!("the session did not settle: {other:?}"),
    }
    assert_eq!(csmsd.settled(), 1);

    // The number the wire carried is three orders of magnitude away, and never
    // reached the record.
    let billed_wh = csmsd.with_cdrs(|ledger| {
        ledger
            .iter()
            .next()
            .map(|cdr| cdr.total_energy.wh())
            .expect("a record")
    });
    assert_eq!(billed_wh, Decimal::from_str_exact("636.000").unwrap());
    assert!(billed_wh < Decimal::from(OCPP_METER_STOP_WH));

    // …and the record is priced, taxed and settleable.
    let priced = csmsd.with_cdrs(|ledger| {
        let cdr = ledger.iter().next().expect("a record");
        (
            cdr.total_cost().map(|m| m.to_string()),
            emob_cdr::validate(cdr).is_settleable(),
        )
    });
    assert_eq!(priced.0.as_deref(), Some("0.31 EUR"));
    assert!(priced.1, "a genuine session settles");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_station_signing_with_an_unprovisioned_key_is_named_before_the_audit() {
    // The chain refuses this session anyway, against the registry. The mismatch
    // is reported separately because it has a **different fix**: a meter was
    // swapped and nobody told the registry, and every session from this station
    // will be unbillable until somebody does.
    let stale = registry_holding(
        // A different, perfectly valid secp256k1 key — the one the OCA's §5.3
        // example composes.
        concat!(
            "3056301006072a8648ce3d020106052b8104000a03420004460a02ba2766d9c44f023ecc",
            "0e4e58644a87add1aadd6317e5fe4dccdb29b163a01d8a6297c84bc530f86431e92f8d46",
            "ab37830247c05cbd92fac252929e7f61",
        ),
        "a stale provisioning record",
    );
    let csmsd = Arc::new(Csmsd::new(
        PartyId::new("DE", "ABC").unwrap(),
        stale,
        tariff(),
    ));
    let (addr, server) = serve(Arc::clone(&csmsd)).await;
    run_one_session(&connect(&addr, "CP-1")).await;
    server.abort();

    let outcomes = csmsd.outcomes();
    assert!(
        outcomes
            .iter()
            .any(|o| matches!(o, Outcome::KeyMismatch { .. })),
        "the swapped meter has to be named: {outcomes:?}"
    );
    // …and the session still does not settle, because the registry is what
    // verification uses.
    assert_eq!(csmsd.settled(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_station_nobody_provisioned_is_answered_unknown_rather_than_admitted() {
    // `[OCPP 2.0.1 Part 4 §3.1.1]` wants a 404 rather than a 401, so an operator
    // can tell a typo from a bad password — and a station nobody provisioned has
    // no EVSE id, so its sessions would be attributed to a point that does not
    // exist.
    let csmsd = Arc::new(Csmsd::new(
        PartyId::new("DE", "ABC").unwrap(),
        registry(),
        tariff(),
    ));
    let (addr, server) = serve(csmsd).await;
    let handle = connect(&addr, "CP-NOBODY");

    // The handshake is refused, so nothing this station sends is ever answered.
    let result = tokio::time::timeout(Duration::from_secs(3), handle.call(boot())).await;
    assert!(
        matches!(result, Ok(Err(_)) | Err(_)),
        "an unprovisioned station must not get a boot acceptance"
    );

    handle.shutdown(Duration::from_secs(1)).await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_station_claiming_signed_data_and_sending_none_is_named_not_ignored() {
    // The quietest failure in the stack: a station that sets
    // `format: SignedData` and puts something unparseable in the value looks,
    // to anything reading only the billable events, exactly like a station
    // sending no signed data at all — and the operator finds out when a month
    // of sessions turns out to be unbillable.
    //
    // `ocpp-kit` reports it as a `WarningKind::UnreadableSignedData` on the
    // observation, and `csmsd` turns that into a refusal with a reason.
    let csmsd = Arc::new(Csmsd::new(
        PartyId::new("DE", "ABC").unwrap(),
        registry(),
        tariff(),
    ));
    let (addr, server) = serve(Arc::clone(&csmsd)).await;
    let handle = connect(&addr, "CP-1");

    handle.call(boot()).await.expect("the CSMS answers a boot");
    handle.wait_ready().await;
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            "HRWWBX8".to_owned(),
            0,
            DateTime::parse("2023-05-19T13:52:39Z").unwrap(),
        ))
        .await
        .expect("the CSMS answers a start");

    let stop = v1_6::StopTransactionRequest::new(
        OCPP_METER_STOP_WH,
        DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
        started.transaction_id,
    )
    .with_reason(v1_6::Reason::Local)
    .with_transaction_data(vec![v1_6::MeterValue::new(
        DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
        vec![
            v1_6::SampledValue::new("not a signed meter value document".to_owned())
                .with_format(v1_6::ValueFormat::SignedData)
                .with_context(v1_6::ReadingContext::TransactionEnd),
        ],
    )]);

    // The message is still answered — refusing the RPC would only make the
    // station retry the same broken payload.
    handle.call(stop).await.expect("the CSMS answers a stop");
    handle.shutdown(Duration::from_secs(2)).await;
    server.abort();

    let outcomes = csmsd.outcomes();
    assert!(
        outcomes.iter().any(|o| matches!(
            o,
            Outcome::Refused { reasons, .. }
                if reasons.iter().any(|r| r.contains("unreadable signed data"))
        )),
        "the claim has to be named: {outcomes:?}"
    );
    assert_eq!(csmsd.settled(), 0, "nothing signed it");
}
