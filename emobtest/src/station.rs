//! A charging station a CSMS can bill: `ocpp-kit`'s `Wallbox` for the physics
//! and the protocol, emob's meter for the signatures.
//!
//! The wallbox holds a vehicle (`ocpp-kit`'s `Ev`) on virtual time, answers the
//! CSMS's charging profiles and produces what a station sends. Two things a CSMS
//! that bills needs and a wallbox deliberately leaves out are added here: the
//! card the driver presents, and an OCMF signature on every register reading,
//! by an `emob-sim` meter whose key the CSMS's registry can hold. The session
//! is played as having happened over the last `minutes`, so no timestamp is in
//! the future.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use emob_sim::{Rng, VirtualStation};
use ocpp_kit::engine::{CallOptions, IncomingRequest};
use ocpp_kit::rpc::CallError;
use ocpp_kit::testkit::{Ev, Outgoing, Wallbox};
use ocpp_kit::transport::{
    BasicAuthPassword, BoxFuture, ClientTls, Ctx, Ended, Handle, Handler, SecurityProfile, Station,
};
use ocpp_kit::{RawValue, Version};
use pyo3::exceptions::{PyConnectionError, PyValueError};
use pyo3::prelude::*;
use rust_decimal::Decimal;
use serde_json::{Value, json};

const REGISTER: &str = "Energy.Active.Import.Register";

fn refused(why: impl ToString) -> PyErr {
    PyValueError::new_err(why.to_string())
}

fn lost(why: impl ToString) -> PyErr {
    PyConnectionError::new_err(why.to_string())
}

fn version(text: &str) -> PyResult<Version> {
    match text {
        "1.6" => Ok(Version::V1_6),
        "2.0.1" => Ok(Version::V2_0_1),
        "2.1" => Ok(Version::V2_1),
        other => Err(refused(format!("OCPP {other:?} is not 1.6, 2.0.1 or 2.1"))),
    }
}

/// What the CSMS sent the station, answered: the display's messages are kept,
/// charging profiles go to the wallbox, the rest is not supported.
#[derive(Clone, Default)]
struct Answers {
    wallbox: Arc<Mutex<Option<Wallbox>>>,
    display: Arc<Mutex<Vec<String>>>,
    asked: Arc<Mutex<Vec<String>>>,
    /// What the CSMS holds this station for: `(id, card, until)`.
    reservations: Arc<Mutex<Vec<(i64, String, String)>>>,
}

impl Handler for Answers {
    fn on_request(
        &self,
        _ctx: Ctx,
        request: IncomingRequest,
    ) -> BoxFuture<'_, Result<Box<RawValue>, CallError>> {
        Box::pin(async move {
            let payload = request.payload.get().to_owned();
            if let Ok(mut asked) = self.asked.lock() {
                asked.push(request.action.clone());
            }
            // A reservation is kept and answered `Accepted`; a cancellation
            // of one it holds too, of one it does not `Rejected`.
            if request.action == "ReserveNow" || request.action == "CancelReservation" {
                let asked: Value = serde_json::from_str(&payload).unwrap_or_default();
                let id = asked["reservationId"]
                    .as_i64()
                    .or_else(|| asked["id"].as_i64())
                    .unwrap_or_default();
                let mut held = self
                    .reservations
                    .lock()
                    .map_err(|e| CallError::internal(e.to_string()))?;
                let status = if request.action == "ReserveNow" {
                    let card = asked["idTag"]
                        .as_str()
                        .or_else(|| asked["idToken"]["idToken"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    let until = asked["expiryDate"]
                        .as_str()
                        .or_else(|| asked["expiryDateTime"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    held.retain(|(kept, _, _)| *kept != id);
                    held.push((id, card, until));
                    "Accepted"
                } else if held.iter().any(|(kept, _, _)| *kept == id) {
                    held.retain(|(kept, _, _)| *kept != id);
                    "Accepted"
                } else {
                    "Rejected"
                };
                return RawValue::from_string(format!(r#"{{"status":"{status}"}}"#))
                    .map_err(|e| CallError::internal(e.to_string()));
            }
            if request.action == "SetDisplayMessage" {
                let shown: Value = serde_json::from_str(&payload).unwrap_or_default();
                if let Some(text) = shown["message"]["message"]["content"].as_str()
                    && let Ok(mut display) = self.display.lock()
                {
                    display.push(text.to_owned());
                }
                return RawValue::from_string(r#"{"status":"Accepted"}"#.to_owned())
                    .map_err(|e| CallError::internal(e.to_string()));
            }
            let answer = self
                .wallbox
                .lock()
                .ok()
                .and_then(|mut slot| slot.as_mut()?.handle(&request.action, &payload));
            match answer {
                Some(Ok(text)) => {
                    RawValue::from_string(text).map_err(|e| CallError::internal(e.to_string()))
                }
                Some(Err(error)) => Err(error),
                None => Err(CallError::not_supported(&request.action)),
            }
        })
    }
}

/// A connected station.
#[pyclass(module = "emobtest._native")]
pub struct NativeStation {
    runtime: tokio::runtime::Runtime,
    handle: Handle,
    version: Version,
    meter: VirtualStation,
    answers: Answers,
    /// Sessions played, for a transaction id no earlier session had.
    sessions: std::sync::atomic::AtomicU32,
}

/// One session as the station played it.
struct Played {
    status: String,
    transaction: String,
    records: Vec<String>,
    kwh: Decimal,
}

/// How the session misbehaves, on purpose.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Tampered,
    Dropped,
}

struct Signer<'a> {
    meter: &'a VirtualStation,
    key: String,
    page: u64,
    records: Vec<String>,
    fault: Fault,
    /// The signed register values of this session, for the dropped-record fault.
    signed: usize,
    /// The first and the last register value signed, in Wh.
    first: Option<Decimal>,
    last: Decimal,
}

impl Signer<'_> {
    /// The last signed register value, as a JSON number of Wh.
    fn wh(&self) -> i64 {
        rust_decimal::prelude::ToPrimitive::to_i64(&self.last).unwrap_or_default()
    }

    /// A register reading in Wh, signed with `marker` — or, under a fault,
    /// signed and then changed, or skipped.
    fn sign(&mut self, marker: char, at: &str, wh: &Value) -> Option<String> {
        let at =
            time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok()?;
        let wh: Decimal = match wh {
            Value::Number(n) => n.to_string().parse().ok()?,
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        // A meter's register counts whole Wh, truncated — as the wallbox
        // reports its 1.6 registers, so a sample and a stop agree.
        let wh = wh.trunc();
        self.first.get_or_insert(wh);
        self.last = wh;
        self.page += 1;
        self.signed += 1;
        if self.fault == Fault::Dropped && marker == 'C' && self.signed == 2 {
            // Signed and never delivered: only pagination can tell.
            return None;
        }
        let mut record = self.meter.sign_reading(
            self.page,
            marker,
            at,
            (wh / Decimal::from(1000)).normalize(),
        );
        if self.fault == Fault::Tampered && marker == 'E' {
            // One digit changed after signing: what a signature exists to catch.
            record = record.replacen("\"RV\":", "\"RV\":9", 1);
        }
        self.records.push(record.clone());
        Some(record)
    }
}

impl NativeStation {
    fn call(&self, action: &str, payload: &Value) -> Result<Value, String> {
        let raw = RawValue::from_string(payload.to_string()).map_err(|e| e.to_string())?;
        self.runtime.block_on(async {
            let answer = tokio::time::timeout(
                Duration::from_secs(30),
                self.handle.call_raw(action, raw, CallOptions::default()),
            )
            .await
            .map_err(|_| format!("no answer to {action} within 30 s"))?
            .map_err(|e| format!("{action}: {e}"))?;
            Ok(answer
                .into_payload()
                .and_then(|p| serde_json::from_str(p.get()).ok())
                .unwrap_or(Value::Null))
        })
    }

    /// Sign every register value of one message; a `Started` without one gets
    /// the opening register, an `Ended` without one the closing register.
    fn sign_2x(signer: &mut Signer<'_>, payload: &mut Value, marker: char, register: &Value) {
        let at = payload["timestamp"].as_str().unwrap_or_default().to_owned();
        if payload["meterValue"].as_array().is_none_or(Vec::is_empty) && marker != 'C' {
            payload["meterValue"] = json!([{ "timestamp": at, "sampledValue": [{
                "value": register, "measurand": REGISTER,
                "context": if marker == 'B' { "Transaction.Begin" } else { "Transaction.End" },
            }]}]);
        }
        let Some(values) = payload["meterValue"].as_array_mut() else {
            return;
        };
        for value in values {
            let at = value["timestamp"].as_str().unwrap_or(&at).to_owned();
            let Some(samples) = value["sampledValue"].as_array_mut() else {
                continue;
            };
            for sample in samples {
                let measurand = sample["measurand"].as_str().unwrap_or(REGISTER);
                if measurand != REGISTER {
                    continue;
                }
                if let Some(record) = signer.sign(marker, &at, &sample["value"]) {
                    sample["value"] = json!(signer.wh());
                    sample["signedMeterValue"] = json!({
                        "signedMeterData": record,
                        "signingMethod": "",
                        "encodingMethod": "OCMF",
                        "publicKey": signer.key,
                    });
                }
            }
        }
    }

    /// A 1.6 signed sampled value: the OCMF document as `SignedData`.
    fn signed_1_6(
        signer: &mut Signer<'_>,
        marker: char,
        at: &str,
        wh: &Value,
        context: &str,
    ) -> Option<Value> {
        let record = signer.sign(marker, at, wh)?;
        let document =
            json!({ "signedMeterData": record, "signingMethod": "", "encodingMethod": "OCMF" });
        Some(json!({ "timestamp": at, "sampledValue": [{
            "value": document.to_string(), "format": "SignedData", "context": context, "measurand": REGISTER,
        }]}))
    }

    #[allow(clippy::too_many_lines)] // One session, in the order a station plays it.
    fn play(&self, card: &str, ev: Ev, minutes: u32, fault: Fault) -> Result<Played, String> {
        let own_id = format!(
            "ET-{:x}-{}",
            time::OffsetDateTime::now_utc().unix_timestamp(),
            self.sessions
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .saturating_add(1)
        );
        let start =
            time::OffsetDateTime::now_utc() - time::Duration::minutes(i64::from(minutes) + 1);
        let start = start.replace_nanosecond(0).unwrap_or(start);
        let started_at = start
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| e.to_string())?;
        let mut wallbox = Wallbox::new(
            self.version,
            started_at.parse().map_err(|e| format!("{e:?}"))?,
        )
        .sample_every(Some(Duration::from_secs(300)));
        let opened = wallbox.plug_in(ev).map_err(|e| e.to_string())?;
        *self.answers.wallbox.lock().map_err(|e| e.to_string())? = Some(wallbox);

        let mut signer = Signer {
            meter: &self.meter,
            key: base64::engine::general_purpose::STANDARD
                .encode(self.meter.public_key().sec1_bytes()),
            page: 0,
            records: Vec::new(),
            fault,
            signed: 0,
            first: None,
            last: Decimal::ZERO,
        };
        let mut status = String::from("Accepted");
        let mut transaction = String::new();
        let mut queue: Vec<Outgoing> = opened;
        let mut elapsed = 0;
        let mut unplugged = false;

        loop {
            for outgoing in std::mem::take(&mut queue) {
                let mut payload: Value =
                    serde_json::from_str(&outgoing.payload).map_err(|e| e.to_string())?;
                match (self.version, outgoing.action) {
                    (Version::V1_6, "StartTransaction") => {
                        payload["idTag"] = json!(card);
                        let answer = self.call("Authorize", &json!({ "idTag": card }))?;
                        status = answer["idTagInfo"]["status"]
                            .as_str()
                            .unwrap_or("Invalid")
                            .to_owned();
                        let answer = self.call("StartTransaction", &payload)?;
                        status = answer["idTagInfo"]["status"]
                            .as_str()
                            .unwrap_or(&status)
                            .to_owned();
                        let id = answer["transactionId"].clone();
                        transaction = id.to_string();
                        let at = payload["timestamp"].as_str().unwrap_or_default().to_owned();
                        let wh = payload["meterStart"].clone();
                        if let Some(value) =
                            Self::signed_1_6(&mut signer, 'B', &at, &wh, "Transaction.Begin")
                        {
                            self.call("MeterValues", &json!({ "connectorId": 1, "transactionId": id, "meterValue": [value] }))?;
                        }
                        let follow = self
                            .answers
                            .wallbox
                            .lock()
                            .ok()
                            .and_then(|mut slot| {
                                Some(
                                    slot.as_mut()?
                                        .answered("StartTransaction", &answer.to_string()),
                                )
                            })
                            .unwrap_or_default();
                        queue.extend(follow);
                    }
                    (Version::V1_6, "MeterValues") => {
                        let mut signed = Vec::new();
                        for value in payload["meterValue"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                        {
                            let at = value["timestamp"].as_str().unwrap_or_default().to_owned();
                            for sample in value["sampledValue"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                            {
                                if sample["measurand"].as_str().unwrap_or(REGISTER) == REGISTER {
                                    let wh = json!(sample["value"].as_str().unwrap_or("0"));
                                    if let Some(entry) = Self::signed_1_6(
                                        &mut signer,
                                        'C',
                                        &at,
                                        &wh,
                                        "Sample.Periodic",
                                    ) {
                                        signed.push(entry);
                                    }
                                }
                            }
                        }
                        if !signed.is_empty() {
                            payload["meterValue"] = Value::Array(signed);
                            let _ = self.call("MeterValues", &payload);
                        }
                    }
                    (Version::V1_6, "StopTransaction") => {
                        let at = payload["timestamp"].as_str().unwrap_or_default().to_owned();
                        let wh = payload["meterStop"].clone();
                        if let Some(value) =
                            Self::signed_1_6(&mut signer, 'E', &at, &wh, "Transaction.End")
                        {
                            payload["transactionData"] = json!([value]);
                            payload["meterStop"] = json!(signer.wh());
                        }
                        payload["idTag"] = json!(card);
                        self.call("StopTransaction", &payload)?;
                    }
                    (_, "TransactionEvent") => {
                        let kind = payload["eventType"].as_str().unwrap_or_default().to_owned();
                        let marker = match kind.as_str() {
                            "Started" => 'B',
                            "Ended" => 'E',
                            _ => 'C',
                        };
                        let register = payload["meterValue"][0]["sampledValue"]
                            .as_array()
                            .and_then(|s| {
                                s.iter().find(|v| {
                                    v["measurand"].as_str().unwrap_or(REGISTER) == REGISTER
                                })
                            })
                            .map(|v| v["value"].clone())
                            .unwrap_or_else(|| json!(signer.wh()));
                        // A fresh wallbox per session numbers from one again; a
                        // station's transaction ids never repeat.
                        payload["transactionInfo"]["transactionId"] = json!(own_id);
                        if kind == "Started" {
                            payload["idToken"] = json!({ "idToken": card, "type": "ISO14443" });
                            transaction.clone_from(&own_id);
                        }
                        Self::sign_2x(&mut signer, &mut payload, marker, &register);
                        let answer = self.call("TransactionEvent", &payload)?;
                        if kind == "Started"
                            && let Some(said) = answer["idTokenInfo"]["status"].as_str()
                        {
                            status = said.to_owned();
                        }
                    }
                    (_, action) => {
                        // Status and EV needs: a CSMS may not support every one.
                        let _ = self.call(action, &payload);
                    }
                }
            }
            if unplugged {
                break;
            }
            let mut slot = self.answers.wallbox.lock().map_err(|e| e.to_string())?;
            let wallbox = slot.as_mut().ok_or("the session's wallbox is gone")?;
            if status != "Accepted" || elapsed >= minutes {
                let (closing, _) = wallbox.unplug().map_err(|e| e.to_string())?;
                queue = closing;
                unplugged = true;
            } else {
                queue = wallbox.advance(Duration::from_secs(60));
                elapsed += 1;
            }
        }
        let kwh = (signer.last - signer.first.unwrap_or(Decimal::ZERO)) / Decimal::from(1000);
        Ok(Played {
            status,
            transaction,
            records: signer.records,
            kwh: kwh.normalize(),
        })
    }
}

#[pymethods]
impl NativeStation {
    #[new]
    #[pyo3(signature = (url, identity, password, ocpp, ca = None, seed = 0, index = 1, stream = "emobtest"))]
    #[allow(clippy::too_many_arguments)]
    fn connect(
        url: &str,
        identity: &str,
        password: &str,
        ocpp: &str,
        ca: Option<&str>,
        seed: u64,
        index: u32,
        stream: &str,
    ) -> PyResult<Self> {
        let version = version(ocpp)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(lost)?;
        let mut rng = Rng::stream(seed, stream);
        let meter = VirtualStation::new(index, &mut rng);
        let answers = Answers::default();
        // A 1.6 AuthorizationKey is hex on the wire; a text secret is sent as
        // its hex, the way a 1.6 station configured with it would.
        let password = BasicAuthPassword::for_version(version, password)
            .or_else(|_| BasicAuthPassword::for_version(version, &hex::encode(password)))
            .map_err(refused)?;
        let handle = runtime.block_on(async {
            let mut builder = Station::builder()
                .identity(identity)
                .map_err(refused)?
                .url(url)
                .versions([version])
                .password(password)
                .reconnect(false)
                .ping_interval(None)
                .handler(answers.clone());
            if url.starts_with("wss://") {
                let tls = match ca {
                    Some(ca) => ClientTls::builder().root_file(ca).map_err(refused)?.build(),
                    None => ClientTls::builder().webpki_roots().build(),
                }
                .map_err(refused)?;
                builder = builder
                    .security_profile(SecurityProfile::TlsBasicAuth)
                    .tls(tls);
            } else {
                builder = builder.security_profile(SecurityProfile::BasicAuth);
            }
            let handle = builder.build().map_err(refused)?.spawn().map_err(lost)?;
            Ok::<_, PyErr>(handle)
        })?;
        let station = Self {
            runtime,
            handle,
            version,
            meter,
            answers,
            sessions: std::sync::atomic::AtomicU32::new(0),
        };
        let boot = match version {
            Version::V1_6 => json!({
                "chargePointVendor": "emobtest", "chargePointModel": "Wallbox",
                "chargePointSerialNumber": station.meter.meter_serial, "firmwareVersion": "emobtest",
            }),
            _ => json!({ "reason": "PowerUp", "chargingStation": {
                "vendorName": "emobtest", "model": "Wallbox",
                "serialNumber": station.meter.meter_serial, "firmwareVersion": "emobtest",
            }}),
        };
        let answer = station.call("BootNotification", &boot).map_err(|e| {
            // Why the session ended says more than that it did.
            let why = match station.runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), station.handle.closed()).await
            }) {
                Ok(Ended::ConnectFailed(error)) => error.to_string(),
                Ok(Ended::Disconnected(reason)) => reason.to_string(),
                _ => e,
            };
            lost(format!("{identity} could not boot: {why}"))
        })?;
        if answer["status"].as_str() != Some("Accepted") {
            return Err(lost(format!(
                "{identity}'s boot was answered {}",
                answer["status"]
            )));
        }
        Ok(station)
    }

    /// The signing meter's public key, SEC1 hex — what the CSMS's registry holds.
    #[getter]
    fn meter_key(&self) -> String {
        hex::encode_upper(self.meter.public_key().sec1_bytes())
    }

    /// The meter's serial, which the signed records name.
    #[getter]
    fn meter_serial(&self) -> String {
        self.meter.meter_serial.clone()
    }

    /// What the CSMS put on the display, oldest first.
    #[getter]
    fn display(&self) -> Vec<String> {
        self.answers
            .display
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }

    /// What the CSMS holds the station for: `(id, card, until)`.
    #[getter]
    fn reservations(&self) -> Vec<(i64, String, String)> {
        self.answers
            .reservations
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    /// Every action the CSMS sent the station, oldest first.
    #[getter]
    fn asked(&self) -> Vec<String> {
        self.answers
            .asked
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }

    /// Charge a vehicle with `card`: `(status, transaction, kWh, records)`.
    #[pyo3(signature = (card, minutes, battery_wh, soc, target_soc, max_amps, phases, fault))]
    #[allow(clippy::too_many_arguments)]
    fn charge(
        &self,
        card: &str,
        minutes: u32,
        battery_wh: u32,
        soc: u32,
        target_soc: u32,
        max_amps: u32,
        phases: u8,
        fault: &str,
    ) -> PyResult<(String, String, String, Vec<String>)> {
        let fault = match fault {
            "" => Fault::None,
            "tampered" => Fault::Tampered,
            "dropped" => Fault::Dropped,
            other => {
                return Err(refused(format!(
                    "fault {other:?} is none of tampered, dropped"
                )));
            }
        };
        let ev = Ev::builder()
            .battery_wh(battery_wh)
            .soc(soc)
            .target_soc(target_soc)
            .phases(phases)
            .current(6, max_amps)
            .build();
        let played = self.play(card, ev, minutes, fault).map_err(lost)?;
        Ok((
            played.status,
            played.transaction,
            played.kwh.to_string(),
            played.records,
        ))
    }

    /// Close the connection.
    fn close(&self) {
        self.runtime
            .block_on(self.handle.shutdown(Duration::from_secs(2)));
    }
}
