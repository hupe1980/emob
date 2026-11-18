//! One simulated charge point on the wire: `ocpp-kit`'s own `Station`, over
//! `wss://` with Basic auth (security profile 2, SR-075), speaking 1.6, 2.0.1
//! or 2.1.
//!
//! The session it plays is `emob-sim`'s: the signed OCMF records and the
//! transaction events a real station would have sent for them
//! ([`emob_sim::ChargedSession::events`]). This module only spells those events
//! in each generation's JSON and lets the kit's typed request check the
//! spelling before it leaves.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use emob_ocpp::{EventKind, TransactionEvent};
use ocpp_kit::transport::{
    BasicAuthPassword, ClientTls, Handle, Handler, SecurityProfile, Station,
};
use ocpp_kit::{RawValue, Version, v1_6, v2_0_1, v2_1};
use serde_json::{Value, json};

/// A station that answers one thing the CSMS asks of it — its display — and
/// keeps what it was asked to show (spec 011). Everything else is not
/// supported: the demo only sends.
#[derive(Clone, Default)]
struct Quiet(Arc<Mutex<Vec<String>>>);

impl Handler for Quiet {
    fn on_request(
        &self,
        _ctx: ocpp_kit::transport::Ctx,
        request: ocpp_kit::engine::IncomingRequest,
    ) -> ocpp_kit::transport::BoxFuture<'_, Result<Box<RawValue>, ocpp_kit::rpc::CallError>> {
        Box::pin(async move {
            if request.action != "SetDisplayMessage" {
                return Err(ocpp_kit::rpc::CallError::not_supported(&request.action));
            }
            let shown: Value = serde_json::from_str(request.payload.get()).unwrap_or_default();
            if let Some(content) = shown["message"]["message"]["content"].as_str() {
                self.0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(content.to_owned());
            }
            RawValue::from_string(r#"{"status":"Accepted"}"#.to_owned())
                .map_err(|error| ocpp_kit::rpc::CallError::internal(error.to_string()))
        })
    }
}

/// How the CSMS answered a driver's card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// The transaction runs, under this id.
    Accepted(String),
    /// Refused at the start, with the wire status (`Blocked`, `Invalid`, …).
    Refused(String),
}

/// One connected charge point.
pub struct Wire {
    /// Its identity.
    pub identity: &'static str,
    /// Its OCPP version.
    pub version: Version,
    handle: Handle,
    /// What the CSMS asked its display to show.
    shown: Quiet,
    /// The meter's public key as the OCA envelope carries it (2.x).
    public_key: String,
    seq: i32,
}

impl Wire {
    /// Connect over `wss://` with the station's password and boot.
    ///
    /// # Errors
    ///
    /// The TLS material cannot be read, or the CSMS does not accept the boot.
    pub async fn connect(
        identity: &'static str,
        version: Version,
        ocpp: std::net::SocketAddr,
        ca: &Path,
        password: &str,
        public_key: &[u8],
    ) -> Result<Self> {
        let shown = Quiet::default();
        let tls = ClientTls::builder()
            .root_file(ca)
            .context("reading the dev CA")?
            .build()
            .context("the station's TLS client")?;
        let handle = Station::builder()
            .identity(identity)?
            .url(format!("wss://{ocpp}/ocpp"))
            .versions([version])
            .security_profile(SecurityProfile::TlsBasicAuth)
            .password(BasicAuthPassword::for_version(version, password)?)
            .tls(tls)
            .reconnect(false)
            .ping_interval(None)
            .handler(shown.clone())
            .build()?
            .spawn()?;
        let wire = Self {
            identity,
            version,
            handle,
            shown,
            public_key: base64::engine::general_purpose::STANDARD.encode(public_key),
            seq: 0,
        };
        wire.boot().await.with_context(|| {
            format!("{identity} ({version}) could not boot against csmsd over wss with profile 2")
        })?;
        Ok(wire)
    }

    async fn boot(&self) -> Result<()> {
        let call = async {
            match self.version {
                Version::V1_6 => {
                    let boot: v1_6::BootNotificationRequest = typed(json!({
                        "chargePointModel": "Musterlader 150",
                        "chargePointVendor": "emob-sim"
                    }))?;
                    self.handle.call(boot).await?;
                }
                Version::V2_0_1 => {
                    let boot: v2_0_1::BootNotificationRequest = typed(json!({
                        "chargingStation": { "model": "Musterlader 22", "vendorName": "emob-sim" },
                        "reason": "PowerUp"
                    }))?;
                    self.handle.call(boot).await?;
                }
                _ => {
                    let boot: v2_1::BootNotificationRequest = typed(json!({
                        "chargingStation": { "model": "Musterlader 150", "vendorName": "emob-sim" },
                        "reason": "PowerUp"
                    }))?;
                    self.handle.call(boot).await?;
                }
            }
            anyhow::Ok(())
        };
        tokio::time::timeout(Duration::from_secs(20), call)
            .await
            .context("no answer to the boot within 20 s")?
    }

    /// What the CSMS asked this station's display to show, oldest first.
    #[must_use]
    pub fn displayed(&self) -> Vec<String> {
        self.shown
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A heartbeat, so Heute never shows a demo station as silent.
    ///
    /// # Errors
    ///
    /// The CSMS did not answer.
    pub async fn heartbeat(&self) -> Result<()> {
        match self.version {
            Version::V1_6 => {
                self.handle.call(v1_6::HeartbeatRequest::new()).await?;
            }
            Version::V2_0_1 => {
                self.handle.call(v2_0_1::HeartbeatRequest::new()).await?;
            }
            _ => {
                self.handle.call(v2_1::HeartbeatRequest::new()).await?;
            }
        }
        Ok(())
    }

    /// Play one transaction event. `card` goes with the opening event;
    /// `transaction` is the id a 2.x station chooses (1.6 is told one).
    ///
    /// Returns the answer to the opening event; `None` for every other.
    ///
    /// # Errors
    ///
    /// The CSMS did not answer.
    pub async fn play(
        &mut self,
        event: &TransactionEvent,
        card: &str,
        transaction: &str,
    ) -> Result<Option<Started>> {
        match self.version {
            Version::V1_6 => self.play_1_6(event, card, transaction).await,
            _ => self.play_2_x(event, card, transaction).await,
        }
    }

    async fn play_1_6(
        &self,
        event: &TransactionEvent,
        card: &str,
        transaction: &str,
    ) -> Result<Option<Started>> {
        let at = rfc3339(event.at);
        let values: Vec<Value> = event
            .signed
            .iter()
            .map(|reading| {
                let document = json!({
                    "signedMeterData": reading.value.signed_meter_data,
                    "signingMethod": "",
                    "encodingMethod": "OCMF",
                });
                json!({
                    "timestamp": at,
                    "sampledValue": [{
                        "value": document.to_string(),
                        "format": "SignedData",
                        "context": reading.context.as_deref().unwrap_or("Sample.Periodic"),
                        "measurand": "Energy.Active.Import.Register",
                    }]
                })
            })
            .collect();
        match event.kind {
            EventKind::Started => {
                let authorize: v1_6::AuthorizeRequest = typed(json!({ "idTag": card }))?;
                self.handle.call(authorize).await?;
                let start: v1_6::StartTransactionRequest = typed(json!({
                    "connectorId": 1, "idTag": card, "meterStart": 0, "timestamp": at
                }))?;
                let answer = self.handle.call(start).await?;
                if answer.id_tag_info.status != v1_6::AuthorizationStatus::Accepted {
                    return Ok(Some(Started::Refused(wire(&answer.id_tag_info.status))));
                }
                let id = answer.transaction_id;
                if !values.is_empty() {
                    let meter: v1_6::MeterValuesRequest = typed(json!({
                        "connectorId": 1, "transactionId": id, "meterValue": values
                    }))?;
                    self.handle.call(meter).await?;
                }
                Ok(Some(Started::Accepted(id.to_string())))
            }
            EventKind::Updated => {
                if !values.is_empty() {
                    let meter: v1_6::MeterValuesRequest = typed(json!({
                        "connectorId": 1, "transactionId": number(transaction)?, "meterValue": values
                    }))?;
                    self.handle.call(meter).await?;
                }
                Ok(None)
            }
            EventKind::Ended => {
                let stop: v1_6::StopTransactionRequest = typed(json!({
                    "meterStop": 0, "timestamp": at, "transactionId": number(transaction)?,
                    "reason": "Local", "transactionData": values
                }))?;
                self.handle.call(stop).await?;
                Ok(None)
            }
        }
    }

    async fn play_2_x(
        &mut self,
        event: &TransactionEvent,
        card: &str,
        transaction: &str,
    ) -> Result<Option<Started>> {
        let at = rfc3339(event.at);
        let meter: Vec<Value> = event
            .signed
            .iter()
            .map(|reading| {
                let context = reading.context.as_deref().unwrap_or("Sample.Periodic");
                json!({
                    "timestamp": at,
                    "sampledValue": [{
                        "value": register(&reading.value.signed_meter_data),
                        "context": context,
                        "measurand": "Energy.Active.Import.Register",
                        "signedMeterValue": {
                            "signedMeterData": reading.value.signed_meter_data,
                            "signingMethod": "",
                            "encodingMethod": "OCMF",
                            "publicKey": self.public_key,
                        }
                    }]
                })
            })
            .collect();
        let clock = event
            .signed
            .iter()
            .any(|reading| reading.context.as_deref() == Some("Sample.Clock"));
        let (kind, trigger, info) = match event.kind {
            EventKind::Started => (
                "Started",
                "Authorized",
                json!({ "transactionId": transaction, "chargingState": "EVConnected" }),
            ),
            EventKind::Updated if event.signed.is_empty() => (
                "Updated",
                "ChargingStateChanged",
                json!({ "transactionId": transaction, "chargingState": "Charging" }),
            ),
            EventKind::Updated => (
                "Updated",
                if clock {
                    "MeterValueClock"
                } else {
                    "MeterValuePeriodic"
                },
                json!({ "transactionId": transaction, "chargingState": "Charging" }),
            ),
            EventKind::Ended => (
                "Ended",
                "StopAuthorized",
                json!({ "transactionId": transaction, "chargingState": "Idle", "stoppedReason": "Local" }),
            ),
        };
        self.seq += 1;
        let mut body = json!({
            "eventType": kind,
            "timestamp": at,
            "triggerReason": trigger,
            "seqNo": self.seq,
            "transactionInfo": info,
        });
        if !meter.is_empty() {
            body["meterValue"] = Value::Array(meter);
        }
        if event.kind == EventKind::Started {
            body["evse"] = json!({ "id": 1, "connectorId": 1 });
            body["idToken"] = json!({ "idToken": card, "type": "ISO14443" });
        }
        let status = if self.version == Version::V2_0_1 {
            let request: v2_0_1::TransactionEventRequest = typed(body)?;
            let answer = self.handle.call(request).await?;
            answer.id_token_info.map(|info| wire(&info.status))
        } else {
            let request: v2_1::TransactionEventRequest = typed(body)?;
            let answer = self.handle.call(request).await?;
            answer.id_token_info.map(|info| wire(&info.status))
        };
        if event.kind != EventKind::Started {
            return Ok(None);
        }
        Ok(Some(match status.as_deref() {
            Some("Accepted") => Started::Accepted(transaction.to_owned()),
            Some(other) => Started::Refused(other.to_owned()),
            None => bail!("{}: the CSMS did not answer the card", self.identity),
        }))
    }
}

/// A request through its own schema: the JSON above is checked by the kit's
/// typed deserialiser before it is sent.
fn typed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("a request the kit's schema refuses")
}

/// An enum's wire spelling.
fn wire(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn number(transaction: &str) -> Result<i32> {
    transaction
        .parse()
        .with_context(|| format!("a 1.6 transaction id is a number, not {transaction:?}"))
}

fn rfc3339(at: time::OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The register reading inside a signed record (`"RV":12.345`), which a 2.x
/// sampled value repeats in the clear. Whatever the record says — a tampered
/// digit included — is what the station reports.
fn register(record: &str) -> Value {
    record
        .split("\"RV\":")
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .and_then(|digits| serde_json::from_str::<Value>(digits.trim()).ok())
        .unwrap_or(json!(0))
}
