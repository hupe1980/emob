//! Parsing an OCMF record without destroying it.
//!
//! # The one rule everything else follows from
//!
//! The signature is computed over the payload section **exactly as it was
//! written** — "between signing and validation, the payload section must not be
//! manipulated (removing and adding white spaces), otherwise positive
//! validation is not possible" `[OCMF §JSON based OCMF Format]`.
//!
//! A parser that deserialises the JSON into a struct and re-serialises it to
//! verify has already lost: key order, whitespace and number formatting are all
//! free to change, and every one of them changes the hash. This parser
//! therefore keeps the payload's **raw byte span** alongside the typed view,
//! and [`OcmfRecord::signed_bytes`] returns that span rather than anything
//! reconstructed.
//!
//! The same reasoning applies inside the JSON. `RV` is parsed as an exact
//! [`rust_decimal::Decimal`] read from the token's own text, so `2935.600`
//! keeps three decimal places: OCMF says the representation "must not be
//! transformed … since this would change the representation of the physical
//! quantity and thus potentially the number of valid digits"
//! `[OCMF Tab. 7, RV]`.
//!
//! # Abbreviated readings
//!
//! Within one record, a reading may omit `RI`, `RU`, `RT` and `TX` when they
//! are unchanged from the reading before it. The parser resolves that
//! carry-forward so consumers never see the abbreviation — but only within a
//! record, because the specification scopes it that way and carrying a unit
//! across a signature boundary would be inventing data.

use rust_decimal::Decimal;
use serde_json::Value;

use super::model::{
    CurrentType, ErrorFlags, Identification, IdentificationLevel, MeterState, OcmfTime, Pagination,
    Payload, Reading, ReadingUnit, TimeStatus, TransactionMarker,
};
use crate::error::OcmfError;

/// The signature section of a record `[OCMF Tab. 8]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SignatureSection {
    /// `SA` — the algorithm. Defaults to `ECDSA-secp256r1-SHA256` since
    /// OCMF 0.4.
    pub algorithm: String,
    /// `SE` — how the signature bytes are encoded (`hex` or `base64`).
    pub encoding: String,
    /// `SM` — the MIME type of the signature (`application/x-der`).
    pub mime_type: String,
    /// `SD` — the signature itself, decoded to bytes.
    pub data: Vec<u8>,
}

impl SignatureSection {
    /// The default algorithm when `SA` is omitted `[OCMF Tab. 22]`.
    pub const DEFAULT_ALGORITHM: &'static str = "ECDSA-secp256r1-SHA256";
    /// The default encoding when `SE` is omitted `[OCMF Tab. 8]`.
    pub const DEFAULT_ENCODING: &'static str = "hex";
    /// The default MIME type when `SM` is omitted `[OCMF Tab. 8]`.
    pub const DEFAULT_MIME_TYPE: &'static str = "application/x-der";
}

/// A parsed OCMF record: the typed view, and the bytes the signature covers.
#[derive(Debug, Clone, PartialEq)]
pub struct OcmfRecord {
    /// The payload, typed.
    pub payload: Payload,
    /// The signature section.
    pub signature: SignatureSection,
    /// The payload section exactly as it arrived — what the signature covers.
    signed: String,
}

impl OcmfRecord {
    /// The bytes the signature is computed over.
    ///
    /// This is the raw payload section, byte for byte, never a re-serialisation
    /// of [`Self::payload`].
    #[must_use]
    pub fn signed_bytes(&self) -> &[u8] {
        self.signed.as_bytes()
    }

    /// The payload section as text, exactly as it arrived.
    #[must_use]
    pub fn signed_str(&self) -> &str {
        &self.signed
    }
}

/// Parse an OCMF record.
///
/// ```
/// use emob_eichrecht::ocmf;
///
/// let raw = r#"OCMF|{"FV":"1.4","PG":"T1","MS":"BQ1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":10.5,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}]}|{"SD":"3045"}"#;
/// let record = ocmf::parse(raw)?;
///
/// assert_eq!(record.payload.pagination.number, 1);
/// // The signature covers the payload text as written, not a re-serialisation.
/// assert!(record.signed_str().starts_with(r#"{"FV":"1.4""#));
/// # Ok::<(), emob_eichrecht::error::OcmfError>(())
/// ```
///
/// # Errors
///
/// [`OcmfError`] when the framing, the JSON or any typed field is malformed.
pub fn parse(raw: &str) -> Result<OcmfRecord, OcmfError> {
    // ── Framing ─────────────────────────────────────────────────────────────
    // Exactly three sections. The pipe is forbidden *inside* a section by the
    // specification, which is what makes this split unambiguous — and what
    // makes a fourth section a malformed record rather than a trailing
    // extension to ignore.
    let mut sections = raw.splitn(3, '|');
    let header = sections.next().unwrap_or_default();
    let payload_raw = sections
        .next()
        .ok_or(OcmfError::MissingSection { section: "payload" })?;
    let signature_raw = sections.next().ok_or(OcmfError::MissingSection {
        section: "signature",
    })?;

    if header.trim() != "OCMF" {
        return Err(OcmfError::BadHeader {
            found: header.trim().to_owned(),
        });
    }
    if signature_raw.contains('|') {
        return Err(OcmfError::TooManySections);
    }

    let payload_json: Value =
        serde_json::from_str(payload_raw).map_err(|e| OcmfError::BadJson {
            section: "payload",
            detail: e.to_string(),
        })?;
    let signature_json: Value =
        serde_json::from_str(signature_raw).map_err(|e| OcmfError::BadJson {
            section: "signature",
            detail: e.to_string(),
        })?;

    let payload = parse_payload(&payload_json)?;
    let signature = parse_signature(&signature_json)?;

    Ok(OcmfRecord {
        payload,
        signature,
        // The span as it arrived. Not `payload_json.to_string()`, which would
        // reorder keys, drop whitespace and reformat numbers — and produce a
        // hash the station never signed.
        signed: payload_raw.to_owned(),
    })
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn parse_payload(v: &Value) -> Result<Payload, OcmfError> {
    let pagination_raw = v
        .get("PG")
        .and_then(Value::as_str)
        .ok_or(OcmfError::MissingField { field: "PG" })?;
    let pagination = Pagination::parse(pagination_raw)?;

    let identification = parse_identification(v)?;

    let readings_json = v
        .get("RD")
        .and_then(Value::as_array)
        .ok_or(OcmfError::MissingField { field: "RD" })?;
    let readings = parse_readings(readings_json)?;

    Ok(Payload {
        format_version: str_field(v, "FV"),
        gateway_id: str_field(v, "GI"),
        gateway_serial: str_field(v, "GS"),
        gateway_version: str_field(v, "GV"),
        pagination,
        meter_vendor: str_field(v, "MV"),
        meter_model: str_field(v, "MM"),
        meter_serial: str_field(v, "MS"),
        meter_firmware: str_field(v, "MF"),
        controller_firmware: str_field(v, "CF"),
        identification,
        charge_point_id_type: str_field(v, "CT"),
        charge_point_id: str_field(v, "CI"),
        readings,
    })
}

fn parse_identification(v: &Value) -> Result<Option<Identification>, OcmfError> {
    // The section is present exactly when there is a transaction reference, and
    // `IS` is its mandatory anchor. No `IS` means no section, which is a
    // fiscal reading rather than a malformed one.
    let Some(assigned) = v.get("IS") else {
        return Ok(None);
    };
    let assigned = assigned.as_bool().ok_or(OcmfError::BadFieldType {
        field: "IS",
        expected: "boolean",
    })?;

    let level = match v.get("IL").and_then(Value::as_str) {
        Some(raw) => Some(IdentificationLevel::parse(raw)?),
        None => None,
    };

    let flags = v
        .get("IF")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    Ok(Some(Identification {
        assigned,
        level,
        flags,
        id_type: str_field(v, "IT").unwrap_or_default(),
        id_data: str_field(v, "ID"),
        tariff_text: str_field(v, "TT"),
    }))
}

fn parse_readings(items: &[Value]) -> Result<Vec<Reading>, OcmfError> {
    // Carried forward within this record only: "for the readings, fields that
    // have an identical value to the previous reading are omitted. However,
    // this only applies within a signed record" [OCMF Tab. 7 preamble].
    let mut last_obis: Option<String> = None;
    let mut last_unit: Option<ReadingUnit> = None;
    let mut last_current: Option<CurrentType> = None;
    let mut last_tx: Option<TransactionMarker> = None;

    let mut readings = Vec::with_capacity(items.len());
    for item in items {
        let time_raw = item
            .get("TM")
            .and_then(Value::as_str)
            .ok_or(OcmfError::MissingField { field: "TM" })?;
        let time = parse_time(time_raw)?;

        let transaction = match item.get("TX").and_then(Value::as_str) {
            Some(raw) => {
                let marker = TransactionMarker::parse(raw)?;
                last_tx = Some(marker);
                Some(marker)
            }
            None => last_tx,
        };

        let value = match item.get("RV") {
            Some(v) => Some(parse_exact_number(v)?),
            None => None,
        };

        let obis = match item.get("RI").and_then(Value::as_str) {
            Some(raw) => {
                last_obis = Some(raw.to_owned());
                Some(raw.to_owned())
            }
            None => last_obis.clone(),
        };

        let unit = match item.get("RU").and_then(Value::as_str) {
            Some(raw) => {
                let u = ReadingUnit::parse(raw)?;
                last_unit = Some(u);
                Some(u)
            }
            None => last_unit,
        };

        let current_type = match item.get("RT").and_then(Value::as_str) {
            Some("AC") => {
                last_current = Some(CurrentType::Ac);
                Some(CurrentType::Ac)
            }
            Some("DC") => {
                last_current = Some(CurrentType::Dc);
                Some(CurrentType::Dc)
            }
            Some(other) => {
                return Err(OcmfError::UnknownCurrentType {
                    value: other.to_owned(),
                });
            }
            None => last_current,
        };

        let cumulated_loss = match item.get("CL") {
            Some(v) => Some(parse_exact_number(v)?),
            None => None,
        };

        let (error_flags, unknown_error_flags) =
            ErrorFlags::parse(item.get("EF").and_then(Value::as_str).unwrap_or(""));

        let state = MeterState::parse(
            item.get("ST")
                .and_then(Value::as_str)
                .ok_or(OcmfError::MissingField { field: "ST" })?,
        )?;

        readings.push(Reading {
            time,
            transaction,
            value,
            obis,
            unit,
            current_type,
            cumulated_loss,
            error_flags,
            unknown_error_flags,
            state,
        });
    }
    Ok(readings)
}

/// Read a JSON number as an exact decimal, from its own text.
///
/// `serde_json` with the `arbitrary_precision` feature would hand us the token
/// directly; without it, `Value::Number` still preserves the literal in its
/// `Display`, which is what this uses. Going through `f64` would turn
/// `2935.600` into `2935.6` and, worse, `0.1` into something that is not `0.1`.
fn parse_exact_number(v: &Value) -> Result<Decimal, OcmfError> {
    let n = v.as_number().ok_or(OcmfError::BadFieldType {
        field: "RV",
        expected: "number",
    })?;
    n.to_string()
        .parse::<Decimal>()
        .map_err(|e| OcmfError::BadNumber {
            value: n.to_string(),
            detail: e.to_string(),
        })
}

/// Parse `"2018-07-24T13:22:04,000+0200 S"` — ISO 8601 with a comma decimal
/// separator, a space, and a status letter.
fn parse_time(raw: &str) -> Result<OcmfTime, OcmfError> {
    let (instant_part, status_part) = raw.rsplit_once(' ').ok_or(OcmfError::BadTime {
        value: raw.to_owned(),
        detail: "expected '<ISO 8601> <status letter>'".to_owned(),
    })?;
    let status = TimeStatus::parse(status_part.trim())?;

    // ISO 8601 permits both `,` and `.` as the decimal separator; `time`'s
    // parser accepts only `.`, so the comma is normalised here. This touches
    // the *parsed* view only — `raw` keeps the original for the evidence
    // record, and the signature is over the untouched payload regardless.
    let normalised = instant_part.replace(',', ".");
    let format = time::format_description::well_known::Iso8601::PARSING;
    let instant =
        time::OffsetDateTime::parse(&normalised, &format).map_err(|e| OcmfError::BadTime {
            value: raw.to_owned(),
            detail: e.to_string(),
        })?;

    Ok(OcmfTime {
        instant,
        status,
        raw: raw.to_owned(),
    })
}

fn parse_signature(v: &Value) -> Result<SignatureSection, OcmfError> {
    let algorithm =
        str_field(v, "SA").unwrap_or_else(|| SignatureSection::DEFAULT_ALGORITHM.to_owned());
    let encoding =
        str_field(v, "SE").unwrap_or_else(|| SignatureSection::DEFAULT_ENCODING.to_owned());
    let mime_type =
        str_field(v, "SM").unwrap_or_else(|| SignatureSection::DEFAULT_MIME_TYPE.to_owned());

    let sd = v
        .get("SD")
        .and_then(Value::as_str)
        .ok_or(OcmfError::MissingField { field: "SD" })?;

    let data = match encoding.as_str() {
        "hex" => hex::decode(sd).map_err(|e| OcmfError::BadSignatureEncoding {
            encoding: "hex",
            detail: e.to_string(),
        })?,
        "base64" => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(sd)
                .map_err(|e| OcmfError::BadSignatureEncoding {
                    encoding: "base64",
                    detail: e.to_string(),
                })?
        }
        other => {
            return Err(OcmfError::UnknownSignatureEncoding {
                encoding: other.to_owned(),
            });
        }
    };

    Ok(SignatureSection {
        algorithm,
        encoding,
        mime_type,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocmf::model::PaginationContext;

    /// The example from the specification `[OCMF §Example File]`, with a short
    /// signature (the document's own is a placeholder too).
    const SPEC_EXAMPLE: &str = r#"OCMF|{
    "FV": "1.4",
    "GI": "ABL SBC-301",
    "GS": "808829900001",
    "GV": "1.4p3",
    "PG": "T12345",
    "MV": "Phoenix Contact",
    "MM": "EEM-350-D-MCB",
    "MS": "BQ27400330016",
    "MF": "1.0",
    "IS": true,
    "IL": "VERIFIED",
    "IF": ["RFID_PLAIN", "OCPP_RS_TLS"],
    "IT": "ISO14443",
    "ID": "1F2D3A4F5506C7",
    "TT": "Tarif 1",
    "RD": [
        {"TM": "2018-07-24T13:22:04,000+0200 S", "TX": "B", "RV": 2935.600, "RI": "01-0B:01.08.00*FF", "RU": "kWh", "RT": "DC", "EF": "", "ST": "G"},
        {"TM": "2018-07-24T13:26:04,000+0200 S", "TX": "E", "RV": 2965.100, "CL": 0.5, "EF": "", "ST": "G"}
    ]
}|{"SD": "1234ABCD"}"#;

    #[test]
    fn parses_the_specification_example() {
        let r = parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(r.payload.format_version.as_deref(), Some("1.4"));
        assert_eq!(r.payload.pagination.number, 12345);
        assert_eq!(r.payload.pagination.context, PaginationContext::Transaction);
        assert_eq!(r.payload.meter_serial.as_deref(), Some("BQ27400330016"));
        assert_eq!(r.payload.readings.len(), 2);

        let ident = r.payload.identification.as_ref().unwrap();
        assert!(ident.assigned);
        assert_eq!(ident.level, Some(IdentificationLevel::Verified));
        assert_eq!(ident.flags, vec!["RFID_PLAIN", "OCPP_RS_TLS"]);
        assert_eq!(ident.id_type, "ISO14443");
    }

    #[test]
    fn the_signed_span_is_the_text_that_arrived() {
        let r = parse(SPEC_EXAMPLE).unwrap();
        // Whitespace and key order intact — a re-serialisation would have
        // neither, and would hash to something the station never signed.
        assert!(r.signed_str().contains("\n    \"FV\": \"1.4\","));
        assert!(r.signed_str().starts_with('{'));
        assert!(r.signed_str().ends_with('}'));

        // And it is exactly the slice between the two pipes.
        let expected = &SPEC_EXAMPLE["OCMF|".len()..SPEC_EXAMPLE.rfind('|').unwrap()];
        assert_eq!(r.signed_str(), expected);
    }

    #[test]
    fn reading_values_keep_the_scale_the_meter_stated() {
        let r = parse(SPEC_EXAMPLE).unwrap();
        // 2935.600, not 2935.6: three decimals is a claim about accuracy.
        assert_eq!(r.payload.readings[0].value.unwrap().to_string(), "2935.600");
        assert_eq!(r.payload.readings[1].value.unwrap().to_string(), "2965.100");
        assert_eq!(
            r.payload.readings[1].cumulated_loss.unwrap().to_string(),
            "0.5"
        );
    }

    #[test]
    fn omitted_fields_carry_forward_within_the_record() {
        let r = parse(SPEC_EXAMPLE).unwrap();
        // The second reading omits RI, RU and RT.
        assert_eq!(
            r.payload.readings[1].obis.as_deref(),
            Some("01-0B:01.08.00*FF")
        );
        assert_eq!(r.payload.readings[1].unit, Some(ReadingUnit::KWh));
        assert_eq!(r.payload.readings[1].current_type, Some(CurrentType::Dc));
    }

    #[test]
    fn time_and_its_status_are_parsed_together() {
        let r = parse(SPEC_EXAMPLE).unwrap();
        let t = &r.payload.readings[0].time;
        assert_eq!(t.status, TimeStatus::Synchronized);
        assert_eq!(t.instant.year(), 2018);
        assert_eq!(t.instant.offset().whole_hours(), 2);
        assert_eq!(t.raw, "2018-07-24T13:22:04,000+0200 S");
    }

    #[test]
    fn the_signature_defaults_come_from_the_spec() {
        let r = parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(r.signature.algorithm, "ECDSA-secp256r1-SHA256");
        assert_eq!(r.signature.encoding, "hex");
        assert_eq!(r.signature.mime_type, "application/x-der");
        assert_eq!(r.signature.data, vec![0x12, 0x34, 0xAB, 0xCD]);
    }

    #[test]
    fn base64_signatures_decode_too() {
        let raw = r#"OCMF|{"PG":"T1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","RV":1,"RU":"kWh","ST":"G"}]}|{"SE":"base64","SD":"EjSrzQ=="}"#;
        let r = parse(raw).unwrap();
        assert_eq!(r.signature.data, vec![0x12, 0x34, 0xAB, 0xCD]);
    }

    #[test]
    fn framing_is_strict() {
        assert!(matches!(
            parse("NOTOCMF|{}|{}"),
            Err(OcmfError::BadHeader { .. })
        ));
        assert!(matches!(
            parse("OCMF|{}"),
            Err(OcmfError::MissingSection { .. })
        ));
        // A pipe is forbidden inside a section, so a fourth section is
        // malformed rather than an extension to ignore.
        assert!(matches!(
            parse(r#"OCMF|{"PG":"T1","RD":[]}|{"SD":"00"}|extra"#),
            Err(OcmfError::TooManySections)
        ));
    }

    #[test]
    fn a_fiscal_record_has_no_identification_section() {
        let raw = r#"OCMF|{"PG":"F7","MS":"M1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","RV":1,"RU":"kWh","ST":"G"}]}|{"SD":"00"}"#;
        let r = parse(raw).unwrap();
        assert!(r.payload.identification.is_none());
        assert_eq!(r.payload.pagination.context, PaginationContext::Fiscal);
        assert!(r.payload.readings[0].transaction.is_none());
    }

    #[test]
    fn a_malformed_field_names_itself() {
        let raw = r#"OCMF|{"PG":"T1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","RV":1,"RU":"kWh","ST":"Z"}]}|{"SD":"00"}"#;
        assert!(matches!(
            parse(raw),
            Err(OcmfError::UnknownMeterState { .. })
        ));
    }

    #[test]
    fn missing_mandatory_fields_are_refused() {
        assert!(matches!(
            parse(r#"OCMF|{"RD":[]}|{"SD":"00"}"#),
            Err(OcmfError::MissingField { field: "PG" })
        ));
        assert!(matches!(
            parse(r#"OCMF|{"PG":"T1"}|{"SD":"00"}"#),
            Err(OcmfError::MissingField { field: "RD" })
        ));
        assert!(matches!(
            parse(r#"OCMF|{"PG":"T1","RD":[]}|{}"#),
            Err(OcmfError::MissingField { field: "SD" })
        ));
    }
}
