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

use csmsd::{
    Answer, Authorize, ChargePoint, Csmsd, Outcome, Presented, Provisioning, Shared, Status,
};
use emob_core::{Currency, PartyId};
use emob_eichrecht::registry::{ComponentRef, KeyRegistry, RegisteredKey};
use emob_ocpp::fixtures::{OCA_1_6_SAMPLED_VALUE, OCA_KEY_HEX};
use emob_service::Actor;
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::{File, Journal, Memory};
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

/// `empd`, as these tests need it: every card is a contract of `DE*MSP`.
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
                    contract_id: "DE-MSP-C00000001-W".parse().unwrap(),
                }),
                reference: Some("a".repeat(64)),
            }
        })
    }
}

/// A CSMS over an in-memory journal.
fn csmsd(registry: KeyRegistry, journal: Memory) -> Arc<Csmsd<Memory>> {
    csmsd_over(registry, journal)
}

/// A CSMS over any journal.
fn csmsd_over<J: Journal>(registry: KeyRegistry, journal: J) -> Arc<Csmsd<J>> {
    Arc::new(
        Csmsd::open(
            PartyId::new("DE", "ABC").unwrap(),
            registry,
            Arc::new(Provider),
            journal,
            emob_core::TimeZone::utc(),
        )
        .expect("the journal folds"),
    )
}

fn provisioning() -> Provisioning {
    Provisioning::new().with(
        Identity::new("CP-1").unwrap(),
        ChargePoint {
            evse_id: "DE*ABC*E00001".parse().unwrap(),
            rated_power_kw: Decimal::from(150),
            tariff: tariff(),
            ocpp: Version::V1_6,
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
async fn serve<J: Journal>(csmsd: Arc<Csmsd<J>>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
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
    let csmsd = csmsd(registry(), Memory::new());
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
            gap,
        } => {
            assert_eq!(
                energy, "0.636 kWh",
                "the signed transaction difference, not the protocol's register"
            );
            assert!(key.contains("CP-1"), "{key}");
            assert_eq!(*retries, 0, "a quiet link retries nothing");
            assert!(gap.is_empty(), "…and drops nothing: {gap:?}");
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

    // SR-101: every fact the session caused is journalled under the identity
    // the socket authenticated as — not under anything the messages said.
    let actors: Vec<Actor> = csmsd.with_journal(|journal| {
        journal
            .records()
            .expect("the chain holds")
            .into_iter()
            .map(|record| record.actor)
            .collect()
    });
    assert!(!actors.is_empty());
    assert!(
        actors
            .iter()
            .all(|actor| *actor == Actor::station("CP-1").unwrap()),
        "{actors:?}"
    );
    assert_eq!(actors[0].to_string(), "station:CP-1");
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
    let csmsd = csmsd(stale, Memory::new());
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
async fn a_station_whose_claimed_key_does_not_decode_is_named_rather_than_skipped() {
    // `[OCA SMV §3.2.2]` has a station send its own public key beside each
    // signed value, and the comparison against the registry is what names a
    // meter swapped without anybody being told. A key that does not decode at
    // all stops that comparison happening — silently, on every session from
    // that station — so it is its own finding rather than a `continue`.
    //
    // The record itself is untouched: only the `publicKey` envelope is
    // corrupted, so the session still assembles and still bills. The claim
    // decides nothing; being unable to *check* it is the fault.
    let (prefix, _) = OCA_1_6_SAMPLED_VALUE
        .split_once(r#""publicKey":""#)
        .expect("the OCA fixture carries a claimed key");
    let corrupt = format!("{prefix}\"publicKey\":\"////not-a-key////\"}}");

    let csmsd = csmsd(registry(), Memory::new());
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
    handle
        .call(
            v1_6::StopTransactionRequest::new(
                OCPP_METER_STOP_WH,
                DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
                started.transaction_id,
            )
            .with_reason(v1_6::Reason::Local)
            .with_transaction_data(vec![v1_6::MeterValue::new(
                DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
                vec![
                    v1_6::SampledValue::new(corrupt)
                        .with_format(v1_6::ValueFormat::SignedData)
                        .with_context(v1_6::ReadingContext::TransactionEnd)
                        .with_measurand(v1_6::Measurand::EnergyActiveImportRegister),
                ],
            )]),
        )
        .await
        .expect("the CSMS answers a stop");
    handle.shutdown(Duration::from_secs(2)).await;
    server.abort();

    let outcomes = csmsd.outcomes();
    assert!(
        outcomes
            .iter()
            .any(|o| matches!(o, Outcome::UnreadableClaimedKey { .. })),
        "a claimed key nobody can read has to be named: {outcomes:?}"
    );
    // …and the session itself is untouched: the key that decides anything comes
    // from the registry, out of band.
    assert_eq!(csmsd.settled(), 1, "{outcomes:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_station_nobody_provisioned_is_answered_unknown_rather_than_admitted() {
    // `[OCPP 2.0.1 Part 4 §3.1.1]` wants a 404 rather than a 401, so an operator
    // can tell a typo from a bad password — and a station nobody provisioned has
    // no EVSE id, so its sessions would be attributed to a point that does not
    // exist.
    let csmsd = csmsd(registry(), Memory::new());
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
    let csmsd = csmsd(registry(), Memory::new());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retransmitted_stop_does_not_settle_a_second_time_or_refuse_the_first() {
    // OCPP guarantees delivery by retrying, so a station that loses one answer
    // sends its `StopTransaction` again. The record settled on the first
    // delivery; the second used to find no transaction under the key — it had
    // just been removed — open a fresh one holding that single message, and put
    // it through the chain. What came out was a **refusal**, naming a window the
    // record did not have and a clock that cannot carry a duration: a settled
    // session in the operator's failure queue, over a metrology fault that
    // never happened (D306).
    //
    // The operational ledger knew: `Ingested::Duplicate` says in as many words
    // "the station retried; do not bill it twice". `csmsd` was discarding the
    // answer.
    let csmsd = csmsd(registry(), Memory::new());
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
    for _ in 0..3 {
        handle
            .call(stop_transaction(started.transaction_id))
            .await
            .expect("the CSMS answers every stop, retried or not");
    }
    handle.shutdown(Duration::from_secs(2)).await;
    server.abort();

    let outcomes = csmsd.outcomes();
    assert_eq!(
        outcomes.len(),
        1,
        "one transaction, one outcome, however many times it was sent: {outcomes:?}"
    );
    assert!(
        matches!(&outcomes[0], Outcome::Settled { energy, .. } if energy == "0.636 kWh"),
        "{outcomes:?}"
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Refused { .. })),
        "a retry is not a refusal: {outcomes:?}"
    );
    assert_eq!(csmsd.settled(), 1, "and it is billed once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_value_arriving_after_the_stop_is_named_rather_than_billed_as_a_session() {
    // Not a retry — a *new* message, after the station's own `Ended`. It must
    // not open a second transaction, and it must not be silent either: the
    // signed value it carries is energy that is **not** on the record that
    // settled, and the fix for that is at the station.
    let csmsd = csmsd(registry(), Memory::new());
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
    handle
        .call(stop_transaction(started.transaction_id))
        .await
        .expect("the CSMS answers a stop");

    // A minute after the transaction ended, the station sends one more.
    handle
        .call(
            v1_6::MeterValuesRequest::new(
                1,
                vec![v1_6::MeterValue::new(
                    DateTime::parse("2023-05-19T13:56:48Z").unwrap(),
                    vec![
                        v1_6::SampledValue::new(OCA_1_6_SAMPLED_VALUE.to_owned())
                            .with_format(v1_6::ValueFormat::SignedData)
                            .with_context(v1_6::ReadingContext::SamplePeriodic)
                            .with_measurand(v1_6::Measurand::EnergyActiveImportRegister),
                    ],
                )],
            )
            .with_transaction_id(started.transaction_id),
        )
        .await
        .expect("the CSMS answers it");
    handle.shutdown(Duration::from_secs(2)).await;
    server.abort();

    let outcomes = csmsd.outcomes();
    assert!(
        matches!(&outcomes[0], Outcome::Settled { energy, .. } if energy == "0.636 kWh"),
        "{outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            Outcome::AfterTheEnd { transaction, .. } if transaction == &started.transaction_id.to_string()
        )),
        "a straggler after the end is named: {outcomes:?}"
    );
    assert_eq!(csmsd.settled(), 1, "and it opens no second record");
}

/// What a restart is: the facts, folded into a process that never saw them.
fn replay(from: &Csmsd<Memory>) -> Memory {
    let replay = Memory::new();
    from.with_journal(|journal| {
        for record in journal.records().expect("the chain holds") {
            replay
                .append(&record.kind, &record.actor, &record.body)
                .expect("the same facts");
        }
    });
    replay
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_keeps_the_ledger_the_outcomes_and_the_1_6_counter() {
    // Before the journal these were `Mutex`es: a restart emptied the CDR
    // ledger, and the next 1.6 station got transaction id 1 again.
    let first = csmsd(registry(), Memory::new());
    let (addr, server) = serve(Arc::clone(&first)).await;
    run_one_session(&connect(&addr, "CP-1")).await;
    server.abort();
    assert_eq!(first.settled(), 1);

    let restarted = csmsd(registry(), replay(&first));
    assert_eq!(restarted.settled(), 1, "the record survives");
    assert_eq!(
        restarted.outcomes(),
        first.outcomes(),
        "and so does what it meant"
    );
    assert_eq!(
        restarted.with_state(|state| state.next_1_6),
        2,
        "the counter continues rather than handing out an id already spent"
    );
    assert_eq!(
        restarted.with_state(|state| state.pending().count()),
        2,
        "the deliveries the record owes to billd and roamd are still owed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transaction_open_across_a_restart_settles_after_it() {
    let first = csmsd(registry(), Memory::new());
    let (addr, server) = serve(Arc::clone(&first)).await;
    let handle = connect(&addr, "CP-1");
    handle.call(boot()).await.expect("a boot");
    handle.wait_ready().await;
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            "HRWWBX8".to_owned(),
            0,
            DateTime::parse("2023-05-19T13:52:39Z").unwrap(),
        ))
        .await
        .expect("a start");
    handle.shutdown(Duration::from_secs(1)).await;
    server.abort();

    // The process dies with the transaction open; the station reconnects to
    // the one that replaced it and closes it there.
    let restarted = csmsd(registry(), replay(&first));
    let (addr, server) = serve(Arc::clone(&restarted)).await;
    let handle = connect(&addr, "CP-1");
    handle.call(boot()).await.expect("a boot");
    handle.wait_ready().await;
    handle
        .call(stop_transaction(started.transaction_id))
        .await
        .expect("a stop");
    handle.shutdown(Duration::from_secs(1)).await;
    server.abort();

    assert_eq!(restarted.settled(), 1, "{:?}", restarted.outcomes());
}

/// SC-001, SR-096: a journal cut at its end is a history shorter than one this
/// daemon vouched for, and the chain alone cannot see it — the signed head can.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_096_a_journal_cut_by_one_record_does_not_start() {
    const SEED: [u8; 32] = [7; 32];
    let dir = std::env::temp_dir().join(format!(
        "emob-096-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let path = dir.join("csmsd.ndjson");
    let open = || {
        Signing::open(
            File::open(&path).expect("a journal file"),
            SigningKey::from_seed(&SEED, "csmsd"),
            head::beside(&path),
        )
    };

    let first = csmsd_over(registry(), open().expect("a fresh journal opens"));
    let (addr, server) = serve(Arc::clone(&first)).await;
    run_one_session(&connect(&addr, "CP-1")).await;
    server.abort();
    assert_eq!(first.settled(), 1);
    let held = first.with_journal(|journal| journal.records().expect("the chain holds").len());
    drop(first);
    assert!(open().is_ok(), "the journal as written opens");

    // The last line removed: what is left still chains.
    let text = std::fs::read_to_string(&path).expect("the journal");
    let cut = text
        .trim_end_matches('\n')
        .rfind('\n')
        .map_or(0, |at| at + 1);
    std::fs::write(&path, &text[..cut]).expect("the journal, cut");

    let refused = open().expect_err("a journal one record short of its head does not open");
    let message = refused.to_string();
    assert!(
        message.contains(&format!("holds {} records", held - 1)),
        "{message}"
    );
    assert!(
        message.contains(&format!("signed a head at {held}")),
        "{message}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A station sets its own clock from the `currentTime` it is answered, and
/// every timestamp on a transaction follows from it.
#[tokio::test]
async fn a_station_is_told_the_time_and_not_the_epoch() {
    let csmsd = csmsd(registry(), Memory::new());
    let (addr, _server) = serve(Arc::clone(&csmsd)).await;
    let handle = connect(&addr, "CP-1");

    let before = time::OffsetDateTime::now_utc().unix_timestamp();
    let booted = handle.call(boot()).await.expect("the CSMS answers a boot");
    let after = time::OffsetDateTime::now_utc().unix_timestamp();
    let told = booted.current_time.timestamp().as_second();
    assert!(
        (before - 1..=after + 1).contains(&told),
        "the boot answered {told}, which is not now ({before}..={after})"
    );

    handle.wait_ready().await;
    let beat = handle
        .call(v1_6::HeartbeatRequest::new())
        .await
        .expect("the CSMS answers a heartbeat");
    let told = beat.current_time.timestamp().as_second();
    assert!(
        (before - 1..=time::OffsetDateTime::now_utc().unix_timestamp() + 1).contains(&told),
        "the heartbeat answered {told}, which is not now"
    );
}

/// Spec 012: a station is known — online once, what it said at boot, a firmware
/// change as one fact naming both versions, its reports, and offline once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_136_sr_138_sr_140_a_station_is_known_by_its_transitions_and_its_word() {
    let csmsd = csmsd(registry(), Memory::new());
    let (addr, _server) = serve(Arc::clone(&csmsd)).await;
    let handle = connect(&addr, "CP-1");

    let mut first = boot();
    first.firmware_version = Some("1.2.0".to_owned());
    first.charge_point_serial_number = Some("SN-0001\u{0007}".to_owned());
    handle.call(first).await.expect("the CSMS answers a boot");
    handle.wait_ready().await;
    for _ in 0..10 {
        handle
            .call(v1_6::HeartbeatRequest::new())
            .await
            .expect("a heartbeat is answered");
    }
    let mut second = boot();
    second.firmware_version = Some("1.3.1".to_owned());
    second.charge_point_serial_number = Some("SN-0001\u{0007}".to_owned());
    handle
        .call(second)
        .await
        .expect("the CSMS answers a second boot");
    handle
        .call(v1_6::SecurityEventNotificationRequest::new(
            "TamperDetectionActivated".to_owned(),
            DateTime::parse("2026-11-20T08:00:00Z").unwrap(),
        ))
        .await
        .expect("a security event is acknowledged, not refused");
    handle
        .call(v1_6::FirmwareStatusNotificationRequest::new(
            v1_6::FirmwareStatus::Installed,
        ))
        .await
        .expect("a firmware status is acknowledged, not refused");

    let known = csmsd.known("CP-1").expect("the station is known");
    let (identity, _) = known.identity.clone().expect("it said what it is");
    assert_eq!(identity.firmware.as_deref(), Some("1.3.1"));
    assert_eq!(
        identity.serial.as_deref(),
        Some("SN-0001"),
        "control characters removed"
    );
    assert_eq!(known.boots.len(), 2);
    assert_eq!(known.firmware.len(), 1, "{:?}", known.firmware);
    assert_eq!(known.firmware[0].from.as_deref(), Some("1.2.0"));
    assert_eq!(known.firmware[0].to, "1.3.1");
    assert_eq!(known.reports.len(), 2, "{:?}", known.reports);

    // Ten heartbeats and two boots wrote exactly one `online`.
    let onlines = csmsd.with_journal(|journal| {
        journal
            .records()
            .unwrap()
            .iter()
            .filter(|record| record.kind == "online")
            .count()
    });
    assert_eq!(onlines, 1);

    // Silent past the heartbeat: offline once, however often the sweep runs.
    let later = time::OffsetDateTime::now_utc() + time::Duration::minutes(10);
    assert_eq!(csmsd.sweep(later).unwrap(), 1);
    assert_eq!(csmsd.sweep(later + time::Duration::minutes(1)).unwrap(), 0);
    assert!(matches!(
        csmsd.known("CP-1").unwrap().presence,
        Some(csmsd::journal::Presence::Offline { .. })
    ));
    handle.shutdown(Duration::from_secs(2)).await;
}

/// SR-136: the meter's identity comes from the signed records of a session that
/// billed — the OCA's DZG record names its own meter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_136_a_billed_session_names_the_meter_that_signed_it() {
    let csmsd = csmsd(registry(), Memory::new());
    let (addr, _server) = serve(Arc::clone(&csmsd)).await;
    let handle = connect(&addr, "CP-1");
    run_one_session(&handle).await;
    let known = csmsd.known("CP-1").expect("the station is known");
    let (_, (meter, _)) = known.meters.iter().next().expect("one point's meter");
    assert!(meter.serial.is_some(), "{meter:?}");
}

/// SR-136: a record that does not verify — here, against an empty registry —
/// bills nothing and names no meter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sr_136_an_unverified_record_names_no_meter() {
    let csmsd = csmsd(KeyRegistry::new(), Memory::new());
    let (addr, _server) = serve(Arc::clone(&csmsd)).await;
    let handle = connect(&addr, "CP-1");
    run_one_session(&handle).await;
    let known = csmsd.known("CP-1").expect("the station is known");
    assert!(known.meters.is_empty(), "{:?}", known.meters);
}
