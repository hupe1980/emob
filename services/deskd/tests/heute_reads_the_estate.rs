//! Spec 004: Heute, read from the real estate.
//!
//! `empd`, `billd`, `roamd`, `csmsd` and `deskd` start in-process on loopback
//! from the demo estate (`deploy/estate.example.toml`), as in spec 001's
//! `the_estate_runs.rs`. One station charges three times — a good card, a
//! tampered record, a blocked card — and the page is read the way a browser
//! reads it: HTTP Basic with the operator token as its password.
//!
//! The property: the rate's numerator **is** the count of records `billd`
//! received (US2.2, SR-058), because both are the ledger that bills.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use emob_core::Energy;
use emob_service::estate::{self, Env};
use emob_service::{Identity, Shutdown};
use emob_sim::{Fault, Rng, SessionPlan, VirtualStation};
use ocpp_kit::testkit::MockStation;
use ocpp_kit::types::DateTime;
use ocpp_kit::v1_6;
use rust_decimal::Decimal;

const DEMO: &str = include_str!("../../../deploy/estate.example.toml");
const GOOD_CARD: &str = "04A1B2C3D4E5F6";
const BLOCKED_CARD: &str = "04DEADBEEF0001";
const OPERATOR: &str = "operator-token";
const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";

fn env() -> Env {
    let values: BTreeMap<&str, &str> = [
        ("EMOB_WEBHOOK_SECRET", SECRET),
        ("EMOB_SERVICE_TOKEN", "service-token"),
        ("EMOB_OPERATOR_TOKEN", OPERATOR),
        ("EMPD_TOKEN_KEY", "a key nobody outside empd holds"),
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
        ("EMOB_PARTNER_DE_MSP_TOKEN_A", "token-a-for-de-msp"),
        (
            "EMPD_SIGNING_KEY",
            "ERERERERERERERERERERERERERERERERERERERERERE=",
        ),
        (
            "ROAMD_SIGNING_KEY",
            "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=",
        ),
        (
            "CSMSD_SIGNING_KEY",
            "MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM=",
        ),
        (
            "BILLD_SIGNING_KEY",
            "REREREREREREREREREREREREREREREREREREREREREQ=",
        ),
    ]
    .into_iter()
    .collect();
    Arc::new(move |name| values.get(name).map(|value| (*value).to_owned()))
}

fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

struct Estate {
    dir: PathBuf,
    empd: SocketAddr,
    billd: SocketAddr,
    roamd: SocketAddr,
    csmsd: SocketAddr,
    text: String,
}

impl Estate {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "emob-heute-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let (empd, billd, roamd, csmsd) = (free_port(), free_port(), free_port(), free_port());
        let text = DEMO
            .replace("http://127.0.0.1:9584", &format!("http://{billd}"))
            .replace("http://127.0.0.1:9585", &format!("http://{empd}"))
            .replace("http://127.0.0.1:9583", &format!("http://{roamd}"))
            .replace("http://127.0.0.1:9001", &format!("http://{csmsd}"))
            .replace("retry_secs = 5", "retry_secs = 1")
            // `MockStation` speaks profile 1 over `ws://`, as in spec 001's run.
            .replace("security_profile = 2", "security_profile = 1")
            .replace(r#"tls_cert = "../target/dev-tls/server.pem""#, "")
            .replace(r#"tls_key = "../target/dev-tls/server.key""#, "")
            .replace(r#"client_ca = "../target/dev-tls/ca.pem""#, "");
        Self {
            dir,
            empd,
            billd,
            roamd,
            csmsd,
            text,
        }
    }

    fn sections<T: serde::de::DeserializeOwned>(&self) -> T {
        estate::from_str(&self.text).expect("the demo estate is one every daemon runs on")
    }
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

/// Authorise, start, the signed begin, stop with the signed end.
async fn charge(station: &MockStation, card: &str, faults: &[Fault]) {
    let plan = plan();
    let session = VirtualStation::new(1, &mut Rng::stream(2026, "estate")).charge(&plan, faults);
    let handle = station.handle();
    handle
        .call(v1_6::AuthorizeRequest::new(card))
        .await
        .expect("Authorize");
    let started = handle
        .call(v1_6::StartTransactionRequest::new(
            1,
            card.to_owned(),
            0,
            at(plan.started_at),
        ))
        .await
        .expect("a start");
    if started.id_tag_info.status != v1_6::AuthorizationStatus::Accepted {
        return;
    }
    let (first, rest) = session.records.split_first().unwrap();
    let (last, middle) = rest.split_last().unwrap();
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
        .expect("MeterValues");
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
        .expect("a stop");
}

fn basic(password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("operator:{password}"))
    )
}

async fn get(url: &str, authorization: Option<&str>) -> (u16, String) {
    let mut request = reqwest::Client::new().get(url);
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rate_heute_shows_is_the_count_billd_received() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let estate = Estate::new();
        let empd = empd::daemon::run(empd::daemon::Options {
            http: estate.empd,
            journal: estate.dir.join("empd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("empd", "test"),
        })
        .await
        .expect("empd starts");
        let billd = billd::daemon::run(billd::daemon::Options {
            http: estate.billd,
            journal: estate.dir.join("billd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("billd", "test"),
        })
        .await
        .expect("billd starts");
        let roamd = roamd::daemon::run(roamd::daemon::Options {
            http: estate.roamd,
            journal: estate.dir.join("roamd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("roamd", "test"),
        })
        .await
        .expect("roamd starts");
        let csmsd = csmsd::daemon::run(csmsd::daemon::Options {
            ocpp: "127.0.0.1:0".parse().unwrap(),
            http: estate.csmsd,
            journal: estate.dir.join("csmsd.ndjson"),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("csmsd", "test"),
            clock: emob_service::system_clock(),
        })
        .await
        .expect("csmsd starts");
        let desk = deskd::run(deskd::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            estate: estate.sections(),
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("deskd", "test"),
            clock: deskd::system_clock(),
            witness: estate.dir.join("witness.ndjson"),
        })
        .await
        .expect("deskd starts");
        let page = format!("http://{}/", desk.http);

        // SR-098: deskd witnesses every journalled daemon it reads.
        let rounds = desk.watch.round().await;
        assert_eq!(rounds.len(), 4, "{rounds:?}");
        assert!(
            rounds
                .iter()
                .all(|(_, verdict)| verdict.as_ref().is_ok_and(|v| !v.is_alarm())),
            "{rounds:?}"
        );

        // SR-050: no password, no page.
        let (status, body) = get(&page, None).await;
        assert_eq!(status, 401);
        assert!(body.is_empty());
        let (status, _) = get(&page, Some(&basic("not-the-token"))).await;
        assert_eq!(status, 401);
        // SR-056: nothing on this surface changes anything.
        let posted = reqwest::Client::new()
            .post(&page)
            .header("authorization", basic(OPERATOR))
            .send()
            .await
            .unwrap();
        assert_eq!(posted.status().as_u16(), 405);
        // SR-095: a cursor is a number or the page refuses, echoing nothing.
        for query in ["refused=%22%3E%3Cscript%3E", "silent=-1", "invoices=1e3"] {
            let (status, body) = get(&format!("{page}?{query}"), Some(&basic(OPERATOR))).await;
            assert_eq!(status, 400, "{query}");
            assert!(!body.contains('<') && !body.contains("script"), "{body}");
        }
        let (status, _) = get(&format!("{page}?refused=0"), Some(&basic(OPERATOR))).await;
        assert_eq!(status, 200, "a cursor past the oldest is an empty page");

        let station =
            MockStation::connect(&format!("ws://{}/ocpp", csmsd.ocpp), "CP-1").expect("a station");
        station.boot(ocpp_kit::Version::V1_6).await.expect("a boot");
        charge(&station, GOOD_CARD, &[]).await;
        charge(&station, GOOD_CARD, &[Fault::TamperedValue]).await;
        charge(&station, BLOCKED_CARD, &[]).await;

        // The good session reaches billd through csmsd's outbox.
        let billd_view = format!("http://{}/v1/billd/today", estate.billd);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let received = loop {
            let (_, body) = get(&billd_view, Some(&format!("Bearer {OPERATOR}"))).await;
            let view: serde_json::Value = serde_json::from_str(&body).unwrap();
            let received = usize::try_from(view["received"].as_u64().unwrap()).unwrap();
            if received > 0 {
                break received;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "billd never received the record"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // US2.2 / SR-058 / SC-002, at the read model: today's numerator is
        // billd's count — every session here ran today.
        let (_, body) = get(
            &format!("http://{}/v1/csmsd/today", estate.csmsd),
            Some(&format!("Bearer {OPERATOR}")),
        )
        .await;
        let csmsd_view: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            csmsd_view["success"]["today"]["estate"],
            csmsd_view["success"]["week"]["estate"]
        );
        let estate_rate = &csmsd_view["success"]["today"]["estate"];
        assert_eq!(estate_rate["billed"].as_u64().unwrap(), received as u64);
        assert_eq!(estate_rate["attempted"], 3, "{estate_rate}");
        assert_eq!(estate_rate["failed"]["evidence"], 1);
        assert_eq!(estate_rate["failed"]["authorisation"], 1);

        // …and on the page, as an operator reads it.
        let (status, heute) = get(&page, Some(&basic(OPERATOR))).await;
        assert_eq!(status, 200);
        assert!(
            heute.contains(&format!("{received} von 3 Ladevorgängen abgerechnet")),
            "{heute}"
        );
        assert!(heute.contains("Nachweis <span class=\"num\">1</span>"));
        assert!(heute.contains("Autorisierung <span class=\"num\">1</span>"));
        // The tampered session leads the queue, with its reason and paragraph.
        let first_row = heute
            .find("<li id=\"s-CP-1-")
            .expect("a refused session row");
        assert!(first_row < heute.find("id=\"duties\"").unwrap());
        assert!(heute.contains("Eine Signatur passt nicht"));
        assert!(heute.contains("[MessEG §33]"));
        // SR-057: no card, no contract on the page.
        for absent in [GOOD_CARD, BLOCKED_CARD, "DE-MSP-C00000001", OPERATOR] {
            assert!(!heute.contains(absent), "{absent} is on the page");
        }

        // SR-054: a billd nobody answers for — a deskd reading it at a port
        // nothing listens on — is named; the rest still renders.
        let nobody = free_port();
        let blind: deskd::Estate = estate::from_str(&estate.text.replace(
            &format!("http://{}", estate.billd),
            &format!("http://{nobody}"),
        ))
        .unwrap();
        let blind = deskd::run(deskd::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            estate: blind,
            env: env(),
            shutdown: Shutdown::new(),
            identity: Identity::new("deskd", "test"),
            // A page's "today" is the clock it was given, not the host's.
            clock: std::sync::Arc::new(|| time::macros::datetime!(2026-06-01 08:00 UTC)),
            witness: estate.dir.join("witness-blind.ndjson"),
        })
        .await
        .expect("a second deskd starts");
        let (_, heute) = get(&format!("http://{}/", blind.http), Some(&basic(OPERATOR))).await;
        assert!(
            heute.contains("<strong>billd</strong> antwortet nicht"),
            "{heute}"
        );
        assert!(heute.contains("Eine Signatur passt nicht"));
        assert!(!heute.contains("class=\"calm"));
        assert!(
            heute.contains("Montag, 01.06.2026")
                && heute.contains("datetime=\"2026-06-01T08:00:00Z\""),
            "the page's today is its clock's: {heute}"
        );
        blind.kill();

        station.shutdown(Duration::from_secs(1)).await;
        desk.kill();
        billd.kill();
        csmsd.kill();
        roamd.kill();
        empd.kill();
    })
    .await
    .expect("the story ran in under thirty seconds");
}
