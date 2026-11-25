//! `emobtest._native` — what a regulator or a standard defines, from emob's own
//! crates, so a test asserts with the code that bills.
//!
//! Every quantity crosses as a decimal **string**, never a float: Python wraps
//! it in `decimal.Decimal`. A value this side refuses is a `ValueError` naming
//! why — the convention `emobtest` keeps throughout: `ValueError` means the
//! test is wrong, `AssertionError` means the system under test is.

mod station;

use emob_core::{Currency, Emaid, Energy, EvseId, TimeZone};
use emob_eichrecht::{ComponentRef, Evidence, KeyRegistry, PublicKey, Record, RegisteredKey};
use emob_sim::{Rng, SessionPlan, VirtualStation};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use rust_decimal::Decimal;

fn refused(why: impl ToString) -> PyErr {
    PyValueError::new_err(why.to_string())
}

fn instant(text: &str) -> PyResult<time::OffsetDateTime> {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).map_err(|e| {
        refused(format!(
            "{text:?} is not RFC 3339 like 2026-12-01T10:00:00+01:00 ({e})"
        ))
    })
}

fn decimal(text: &str, what: &str) -> PyResult<Decimal> {
    text.trim()
        .parse()
        .map_err(|_| refused(format!("{what} {text:?} is not a decimal like 0.59")))
}

// ── The meter ───────────────────────────────────────────────────────────────

/// A calibrated meter that signs OCMF, from `emob-sim`: seeded, so two runs
/// sign the same bytes.
#[pyclass(module = "emobtest._native")]
struct Meter {
    station: VirtualStation,
}

#[pymethods]
impl Meter {
    #[new]
    #[pyo3(signature = (seed = 0, index = 0, stream = "emobtest"))]
    fn new(seed: u64, index: u32, stream: &str) -> Self {
        let mut rng = Rng::stream(seed, stream);
        Self {
            station: VirtualStation::new(index, &mut rng),
        }
    }

    /// The meter's public key, as a registry or a receipt prints it: SEC1 hex.
    #[getter]
    fn public_key(&self) -> String {
        hex::encode_upper(self.station.public_key().sec1_bytes())
    }

    /// The point it meters.
    #[getter]
    fn evse(&self) -> String {
        self.station.evse_id.to_string()
    }

    /// Its serial, which the signed records name.
    #[getter]
    fn serial(&self) -> String {
        self.station.meter_serial.clone()
    }

    /// One signed reading of the import register: `marker` `B` begins a
    /// transaction, `C` is a reading during it, `E` ends it; `page` is the
    /// meter's pagination, which a missing record shows as a gap.
    fn sign(&self, page: u64, marker: char, at: &str, kwh: &str) -> PyResult<String> {
        if !matches!(marker, 'B' | 'C' | 'E') {
            return Err(refused(format!("marker {marker:?} is none of B, C, E")));
        }
        Ok(self
            .station
            .sign_reading(page, marker, instant(at)?, decimal(kwh, "register")?))
    }

    /// Charge `kwh` from `start` for `minutes`; the signed OCMF records, one
    /// per reading, in the order the meter emits them.
    fn charge(&mut self, kwh: &str, start: &str, minutes: i64) -> PyResult<Vec<String>> {
        if minutes <= 0 {
            return Err(refused("a session lasts at least a minute"));
        }
        let started_at = instant(start)?;
        let energy = Energy::from_kwh(decimal(kwh, "energy")?).map_err(refused)?;
        let plan = SessionPlan {
            started_at,
            ended_at: started_at + time::Duration::minutes(minutes),
            energy,
            clock_aligned: true,
            connected_before_charging: time::Duration::seconds(30),
        };
        Ok(self.station.charge(&plan, &[]).records)
    }
}

// ── Verifying ───────────────────────────────────────────────────────────────

/// One record against one key, as the receipt's verifier checks it: the
/// verdict's code and the readings the checked bytes state.
#[pyfunction]
fn verify(record: &str, key: &str) -> (i32, String) {
    let check = emob_verify::check(record, key);
    (check.verdict.code(), check.summary())
}

/// The component a record names, most specific first — what the key registry
/// looks a key up by.
fn component_of(record: &Record<'_>) -> Option<ComponentRef> {
    let payload = record.payload();
    match (payload.gateway_serial(), payload.meter_serial()) {
        (Some(gateway), Some(meter)) => Some(ComponentRef::GatewayAndMeter {
            gateway: gateway.to_owned(),
            meter: meter.to_owned(),
        }),
        (None, Some(meter)) => Some(ComponentRef::Meter {
            serial: meter.to_owned(),
        }),
        (Some(gateway), None) => Some(ComponentRef::Gateway {
            serial: gateway.to_owned(),
        }),
        (None, None) => None,
    }
}

/// A session's records under `[MessEG §33]`, as `csmsd` checks them before it
/// bills: whether the energy and the duration may be billed, the energy that
/// may, and every reason not.
#[pyfunction]
fn evidence(
    records: Vec<String>,
    key: &str,
) -> PyResult<(bool, bool, Option<String>, Vec<String>)> {
    let parsed: Vec<Record<'_>> = records
        .iter()
        .map(|text| Record::parse(text).map_err(|e| refused(format!("not an OCMF record: {e}"))))
        .collect::<PyResult<_>>()?;
    let first = parsed
        .first()
        .ok_or_else(|| refused("a session has at least one record"))?;
    let public = PublicKey::from_text(key.trim(), first.signature().curve())
        .map_err(|e| refused(format!("not a public key: {e}")))?;
    let component = component_of(first)
        .ok_or_else(|| refused("the record names no meter or gateway a key could belong to"))?;
    let mut registry = KeyRegistry::new();
    registry
        .insert(component, RegisteredKey::unbounded(public, "emobtest"))
        .map_err(refused)?;
    let evidence = Evidence::assemble(&parsed, &registry, time::OffsetDateTime::UNIX_EPOCH);
    let mut reasons: Vec<String> = evidence.reasons().collect();
    let contradiction = contradiction(&records, key);
    reasons.extend(contradiction.clone());
    let sound = contradiction.is_none();
    Ok((
        evidence.is_billable() && sound,
        evidence.is_billable_for_time() && sound,
        evidence
            .billable_energy()
            .filter(|_| sound)
            .map(|e| e.kwh().normalize().to_string()),
        reasons,
    ))
}

/// Two signed readings of one register at one instant that state different
/// values: a meter had one value then, so `csmsd`'s meter series refuses the
/// session (`emob-session`'s `ContradictoryReading`), and so does this.
fn contradiction(records: &[String], key: &str) -> Option<String> {
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for record in records {
        for line in emob_verify::check(record, key).summary().lines() {
            let mut fields = line.split('\t');
            let (Some(_), Some(value), Some(_), Some(at)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let value = value
                .parse::<Decimal>()
                .map_or_else(|_| value.to_owned(), |v| v.normalize().to_string());
            match seen.get(at) {
                Some(other) if *other != value => {
                    return Some(format!(
                        "two signed readings state different values for one register at {at}: \
                         {other} kWh and {value} kWh. A meter had one value at that instant"
                    ));
                }
                Some(_) => {}
                None => {
                    seen.insert(at.to_owned(), value);
                }
            }
        }
    }
    None
}

// ── Prices ──────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct TariffSpec {
    id: String,
    currency: String,
    time_zone: String,
    energy: Decimal,
    vat: Decimal,
    per_minute: Option<Decimal>,
    occupancy_per_minute: Option<Decimal>,
    session_fee: Option<Decimal>,
}

/// The tariff as `csmsd` rates with it — the estate's `[[tariffs]]` shape.
fn tariff(spec: &str) -> PyResult<Tariff> {
    let spec: TariffSpec = serde_json::from_str(spec).map_err(refused)?;
    let mut components =
        vec![PriceComponent::new(Dimension::Energy, spec.energy).with_vat(spec.vat)];
    if let Some(per_minute) = spec.per_minute {
        components.push(
            PriceComponent::new(Dimension::Time, per_minute * Decimal::from(60)).with_vat(spec.vat),
        );
    }
    if let Some(fee) = spec.occupancy_per_minute {
        let component = PriceComponent::occupancy_per_minute(fee).ok_or_else(|| {
            refused("occupancy_per_minute requires an exact nonnegative hourly conversion")
        })?;
        components.push(component.with_vat(spec.vat));
    }
    if let Some(fee) = spec.session_fee {
        components.push(PriceComponent::new(Dimension::Flat, fee).with_vat(spec.vat));
    }
    Ok(Tariff::simple(
        spec.id.parse().map_err(refused)?,
        Currency::new(&spec.currency).map_err(refused)?,
        TariffKind::AdHoc,
        TimeZone::new(&spec.time_zone).map_err(refused)?,
        components,
    ))
}

/// What a session of `kwh` at `power_kw` costs from `at`, rated by the
/// function that bills and rounded as an invoice line is.
#[pyfunction]
fn tariff_total(spec: &str, kwh: &str, power_kw: &str, at: &str) -> PyResult<String> {
    let total = emob_tariff::worked_total(
        &tariff(spec)?,
        decimal(kwh, "energy")?,
        decimal(power_kw, "power")?,
        instant(at)?,
    )
    .ok_or_else(|| refused("a session needs a positive energy and power"))?;
    Ok(total.amount().to_string())
}

/// What a driver must be shown, gross, components in `[AFIR Art. 5(4)]`'s
/// order: `(dimension, price, unit)`.
#[pyfunction]
fn tariff_lines(spec: &str, at: &str) -> PyResult<Vec<(String, String, String)>> {
    let shown = emob_tariff::describe_gross(&tariff(spec)?, instant(at)?).map_err(refused)?;
    Ok(shown
        .lines
        .iter()
        .map(|line| {
            // Every decimal the tariff states, at least the currency's two:
            // 0.50 is a price, 0.5 is a number.
            let mut price = line.price.normalize();
            if price.scale() < 2 {
                price.rescale(2);
            }
            (
                line.dimension.as_str().to_owned(),
                price.to_string(),
                line.unit().to_owned(),
            )
        })
        .collect())
}

/// The version's fingerprint and its Preisstand.
#[pyfunction]
fn tariff_version(spec: &str) -> PyResult<(String, String)> {
    let fingerprint = tariff(spec)?.fingerprint();
    Ok((fingerprint.to_string(), fingerprint.code()))
}

// ── Identifiers ─────────────────────────────────────────────────────────────

/// An EVSE id, normalised — or why not.
#[pyfunction]
fn evse_id(text: &str) -> PyResult<String> {
    text.parse::<EvseId>()
        .map(|id| id.to_string())
        .map_err(refused)
}

/// An eMAID with its check digit, normalised — or why not.
#[pyfunction]
fn emaid(text: &str) -> PyResult<String> {
    text.parse::<Emaid>()
        .map(|id| id.canonical())
        .map_err(refused)
}

// ── Events ──────────────────────────────────────────────────────────────────

const SCHEMAS: [(&str, &str); 2] = [
    (
        "de.emob.cdr.issued",
        include_str!("../../schemas/events/de.emob.cdr.issued.json"),
    ),
    (
        "de.emob.invoice.issued",
        include_str!("../../schemas/events/de.emob.invoice.issued.json"),
    ),
];

/// The event types emob emits.
#[pyfunction]
fn event_types() -> Vec<&'static str> {
    SCHEMAS.iter().map(|(name, _)| *name).collect()
}

/// A structured-mode CloudEvent against its published schema: every way it
/// misses, with where.
#[pyfunction]
fn event_errors(text: &str) -> PyResult<Vec<String>> {
    let event: serde_json::Value =
        serde_json::from_str(text).map_err(|e| refused(format!("not JSON: {e}")))?;
    let kind = event["type"].as_str().unwrap_or_default();
    let Some((_, schema)) = SCHEMAS.iter().find(|(name, _)| *name == kind) else {
        return Ok(vec![format!(
            "type {kind:?} is none emob emits — {}",
            event_types().join(", ")
        )]);
    };
    let schema: serde_json::Value = serde_json::from_str(schema).map_err(refused)?;
    let validator = jsonschema::validator_for(&schema).map_err(refused)?;
    Ok(validator
        .iter_errors(&event)
        .map(|error| {
            let at = error.instance_path().to_string();
            format!(
                "{} {error}",
                if at.is_empty() { "/".to_owned() } else { at }
            )
        })
        .collect())
}

// ── DATEX II ────────────────────────────────────────────────────────────────

/// A DATEX II AFIR file held to the Mobilithek profile, as `emob check-feed`
/// holds it: the reader's tolerated deviations, and per publication `None`
/// when the profile takes it or why not.
#[pyfunction]
fn feed_report(bytes: &[u8]) -> PyResult<(Vec<String>, Vec<Option<String>>)> {
    let json = bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'{' || *b == b'[');
    let document = if json {
        datex2::v3::json::read(bytes)
    } else {
        datex2::v3::read(bytes)
    }
    .map_err(|e| refused(format!("not DATEX II v3: {e}")))?;
    Ok((
        document.warnings.iter().map(ToString::to_string).collect(),
        document
            .publications
            .iter()
            .map(|p| {
                datex2::v3::json::mobilithek::to_string(p)
                    .err()
                    .map(|e| e.to_string())
            })
            .collect(),
    ))
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Meter>()?;
    m.add_class::<station::NativeStation>()?;
    m.add_function(wrap_pyfunction!(verify, m)?)?;
    m.add_function(wrap_pyfunction!(evidence, m)?)?;
    m.add_function(wrap_pyfunction!(tariff_total, m)?)?;
    m.add_function(wrap_pyfunction!(tariff_lines, m)?)?;
    m.add_function(wrap_pyfunction!(tariff_version, m)?)?;
    m.add_function(wrap_pyfunction!(evse_id, m)?)?;
    m.add_function(wrap_pyfunction!(emaid, m)?)?;
    m.add_function(wrap_pyfunction!(event_types, m)?)?;
    m.add_function(wrap_pyfunction!(event_errors, m)?)?;
    m.add_function(wrap_pyfunction!(feed_report, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
