//! The transparency file: what the customer checks the bill against.
//!
//! # Why this is the deliverable
//!
//! `[MessEG §33]` does not say a measured value must be *correct*. It says the
//! affected party must be able to **check** it — and `[PTB-A 50.7]` puts no
//! short limit on when. A platform that verifies a signature internally and
//! reports "verified" has satisfied nobody: the customer cannot repeat the
//! check, and the whole point of the requirement is that they can.
//!
//! What they repeat it with, in Germany, is the S.A.F.E. Transparenzsoftware —
//! an independent verifier the industry maintains and regulators point at. It
//! reads an XML container that holds the signed datasets and the public keys
//! they are checked against:
//!
//! ```xml
//! <values>
//!   <value transactionId="120" context="Transaction.Begin">
//!     <publicKey encoding="hex">3059301306072A8648CE3D…</publicKey>
//!     <signedData format="OCMF" encoding="plain">OCMF|{…}|{"SD":"…"}</signedData>
//!   </value>
//!   <value transactionId="120" context="Transaction.End">…</value>
//! </values>
//! ```
//!
//! The key comes **first**. The verifier's own `values.xsd` declares
//! `<xs:sequence>` with `publicKey` before `signedData`, so a file written the
//! other way round — the order the format's prose example happens to show — is
//! schema-invalid. Both orders happen to unmarshal today, because the reference
//! implementation does not validate against its own schema; writing the valid
//! one costs nothing and does not depend on that staying true.
//!
//! # `transactionId` groups; it does not number
//!
//! The attribute is what the verifier **groups records by**, and getting that
//! wrong is the difference between a driver checking their session and a driver
//! checking two unrelated readings. `MainView` collects
//! `getValues(currentTransactionId)` and hands the whole list to
//! `verifyTransaction`, which is where the begin/end pairing and the energy
//! difference are computed; the reference data set carries the *same*
//! `transactionId` on the `Transaction.Begin` and `Transaction.End` values of
//! one session.
//!
//! So every record of one chain gets **one** id. Numbering them per record —
//! with the pagination counter, say — is schema-valid, passes every test a
//! writer can run against itself, and silently degrades a session into N
//! single-record transactions the verifier cannot pair.
//!
//! OCMF carries no transaction number of its own, so [`to_xml`] derives one
//! from the counter the transaction opened at, and
//! [`to_xml_with_transaction_id`] takes the operator's own number when there is
//! one.
//!
//! [`to_xml`] emits exactly that, from an [`Evidence`] record.
//!
//! # Why it takes an `Evidence` and not a pile of records
//!
//! A transparency file names a key for each dataset, and the binding between a
//! key and a charge point travels out of band `[OCMF §Relation of Serial
//! Numbers]`. A function that took raw records would have to be handed keys
//! from somewhere, and "somewhere" is how a file gets exported with the key
//! that makes it verify rather than the key the station was registered with.
//!
//! So the input is evidence: records whose signatures were checked against keys
//! the registry supplied. **A record that could not be verified has no key
//! binding and therefore no `<value>` element** — which is the honest outcome,
//! and [`Evidence::reasons`] is what explains it to the operator.
//!
//! # The file is emitted whatever the verdict
//!
//! A session that does not bill still gets a file. The customer's right to
//! check does not depend on the answer, and a dispute is precisely the case
//! where the file matters most.

use core::fmt::Write as _;

use crate::evidence::Evidence;
use crate::ocmf::{OcmfRecord, PublicKey, SignatureAlgorithm, TransactionMarker};

/// The two context labels the Transparenzsoftware itself names.
const CONTEXT_BEGIN: &str = "Transaction.Begin";
const CONTEXT_END: &str = "Transaction.End";

/// Emit the S.A.F.E. Transparenzsoftware XML container for a session.
///
/// One `<value>` per verified record, in order, each carrying the record
/// exactly as it arrived and the public key it was checked against.
///
/// ```
/// use emob_eichrecht::{Evidence, KeyRegistry, ocmf, transparency};
/// # let raw_records: Vec<String> = vec![];
/// # let registry = KeyRegistry::new();
/// # let session_start = time::OffsetDateTime::UNIX_EPOCH;
///
/// let records = raw_records.iter().map(|r| ocmf::parse(r)).collect::<Result<Vec<_>, _>>()?;
/// let evidence = Evidence::assemble(&records, &registry, session_start);
///
/// // Hand this to the driver. They check it against the same public key,
/// // in software neither of you wrote.
/// let xml = transparency::to_xml(&evidence);
/// assert!(xml.starts_with(r#"<?xml version="1.0" encoding="UTF-8"?>"#));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn to_xml(evidence: &Evidence) -> String {
    to_xml_with_transaction_id(evidence, derived_transaction_id(evidence))
}

/// The same file, under a transaction number the caller already holds.
///
/// A CPO usually has one — the OCPP `transactionId`, the session's own running
/// number — and using it makes the file a driver receives line up with the one
/// on their invoice. The number is a grouping key for the verifier and nothing
/// more: it is not signed, and changing it changes no verdict.
///
/// Every record in the file is written under this one id, because they are one
/// transaction. See the module documentation for why that matters.
#[must_use]
pub fn to_xml_with_transaction_id(evidence: &Evidence, transaction_id: u64) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<values>\n");

    for verified in &evidence.verified {
        let record = &verified.record;
        // The `context` attribute is optional, and a record that opens *and*
        // closes its transaction is neither a begin nor an end. Labelling it
        // with one of the two — which reading only the first reading does —
        // tells the driver's verifier something the record does not say.
        let context = match context_of(record) {
            Some(label) => format!(" context=\"{}\"", escape_attr(label)),
            None => String::new(),
        };
        let _ = writeln!(out, "  <value transactionId=\"{transaction_id}\"{context}>");
        // `publicKey` before `signedData`, in the order `values.xsd` sequences
        // them.
        let _ = writeln!(
            out,
            "    <publicKey encoding=\"hex\">{}</publicKey>",
            hex::encode_upper(&verified.key.bytes)
        );
        // `encoding="plain"` and the record verbatim: the element must hold no
        // leading or trailing whitespace of its own, or the signature stops
        // matching — which is the same rule the parser lives by, on the way out.
        let _ = writeln!(
            out,
            "    <signedData format=\"OCMF\" encoding=\"plain\">{}</signedData>",
            escape_text(record.as_str())
        );
        out.push_str("  </value>\n");
    }

    out.push_str("</values>\n");
    out
}

/// The transaction number to write when the caller supplied none.
///
/// The pagination counter the transaction **opened** at: the same value for
/// every record of one chain, distinct between consecutive transactions of one
/// component, and already in the file for a reader to check against `PG`. It is
/// a grouping key, not an identity — two meters can open at the same counter,
/// and a file mixing two meters' sessions should be given real ids through
/// [`to_xml_with_transaction_id`].
fn derived_transaction_id(evidence: &Evidence) -> u64 {
    evidence
        .verified
        .first()
        .map_or(0, |v| v.record.payload.pagination.number)
}

/// The `context` label for a record, from the transaction markers it carries.
///
/// `None` when no single label is true of the whole record — a record that both
/// opens and closes its transaction, which is the `MR` configuration
/// `[OCMF §Best Practice]` and the shape of the eBZ LD3 reference data set. The
/// attribute is optional in `values.xsd`, and the reference sample files omit it
/// for exactly that shape.
fn context_of(record: &crate::ocmf::OcmfRecord) -> Option<&'static str> {
    let readings = &record.payload.readings;
    let opens = readings
        .first()
        .and_then(|r| r.transaction)
        .is_some_and(TransactionMarker::begins_transaction);
    let closes = readings
        .last()
        .and_then(|r| r.transaction)
        .is_some_and(TransactionMarker::ends_transaction);

    match (opens, closes) {
        // A whole transaction in one data set is neither half of one.
        (true, true) => None,
        (true, false) => Some(CONTEXT_BEGIN),
        (false, true) => Some(CONTEXT_END),
        (false, false) => Some(match readings.first().and_then(|r| r.transaction) {
            Some(TransactionMarker::Suspended) => "Transaction.Suspended",
            Some(TransactionMarker::TariffChange) => "Transaction.TariffChange",
            Some(TransactionMarker::Exception) => "Transaction.Exception",
            Some(TransactionMarker::Charging) => "Transaction.Charging",
            // A reading with no transaction reference at all is a fiscal one.
            _ => "Fiscal",
        }),
    }
}

// ── Reading one back ────────────────────────────────────────────────────────

/// One `<value>` read out of a transparency container.
#[derive(Debug, Clone, PartialEq)]
pub struct TransparencyValue {
    /// The `transactionId` the file groups this record under, when it names one.
    pub transaction_id: Option<u64>,
    /// The `context` label, when the file carries one.
    pub context: Option<String>,
    /// The record, parsed — and holding the exact bytes the signature covers.
    pub record: OcmfRecord,
    /// The key the **file** says this record was checked against.
    ///
    /// Deliberately not called "the key": it is a claim made by the artefact
    /// under examination, and verifying a record against a key the same file
    /// supplied proves only that whoever wrote the file owned a private key.
    /// The binding between a key and a charge point travels out of band
    /// `[OCMF §Relation of Serial Numbers]`, which is what
    /// [`crate::registry::KeyRegistry`] is for.
    ///
    /// What it is good for is the **comparison**: a driver's file whose key
    /// differs from the one the operator registered is a dispute with an
    /// answer, and one whose key matches narrows the argument to the numbers.
    /// `None` when the file left the key out, which `values.xsd` permits and
    /// the Transparenzsoftware handles by asking the user for one.
    pub claimed_key: Option<PublicKey>,
}

/// Read a S.A.F.E. Transparenzsoftware container.
///
/// # Why the crate reads as well as writes
///
/// The export is only half of `[MessEG §33]`. The other half arrives when a
/// driver disputes a bill and sends back the file they were given: an operator
/// then has to parse it, check the records against **its own registry**, and
/// say whether the key in the file is the key the station was provisioned with.
/// A crate that can only emit leaves that to a script somebody writes twice.
///
/// The records come back with the exact bytes their signatures cover, so
/// [`Evidence::assemble`] can be run over them against a registry — which is
/// the check worth making, and not the one the file invites.
///
/// # Strict on purpose
///
/// This reads the container `values.xsd` describes and refuses everything else
/// rather than guessing: no comments, no CDATA, no namespaces, no nesting the
/// schema does not declare. A transparency file is machine-generated and a
/// verifier that silently accepts a shape it half-understands is the failure
/// this module exists to prevent, pointed inward.
///
/// # Errors
///
/// [`TransparencyError`] when the container is malformed, a record does not
/// parse, or a public key does not decode.
pub fn from_xml(xml: &str) -> Result<Vec<TransparencyValue>, TransparencyError> {
    if xml.contains("<!--") || xml.contains("<![CDATA[") {
        return Err(TransparencyError::Unsupported {
            what: "comments and CDATA sections",
        });
    }

    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(open) = find_element(rest, "value") {
        let (attrs, body, after) = open;
        let close = |name: &'static str| -> Result<Option<(String, String)>, TransparencyError> {
            match find_element(body, name) {
                Some((a, text, _)) => Ok(Some((a.to_owned(), text.to_owned()))),
                None => Ok(None),
            }
        };

        let signed = close("signedData")?.ok_or(TransparencyError::MissingElement {
            element: "signedData",
        })?;
        // The record must come back byte for byte or its signature stops
        // matching, so the only transformation is undoing the escaping the
        // writer applied — and the surrounding whitespace the schema says must
        // not be there, which real files put there anyway.
        let raw = unescape(signed.1.trim())?;
        let record = crate::ocmf::parse(&raw).map_err(|source| TransparencyError::BadRecord {
            detail: source.to_string(),
        })?;

        let claimed_key = match close("publicKey")? {
            Some((key_attrs, text)) => Some(decode_key(
                &record,
                attribute(&key_attrs, "encoding").as_deref(),
                text.trim(),
            )?),
            None => None,
        };

        out.push(TransparencyValue {
            transaction_id: attribute(attrs, "transactionId")
                .and_then(|id| id.trim().parse::<u64>().ok()),
            context: attribute(attrs, "context"),
            record,
            claimed_key,
        });
        rest = after;
    }

    if out.is_empty() && find_element(xml, "values").is_none() {
        return Err(TransparencyError::NotAContainer);
    }
    Ok(out)
}

/// The key a `<publicKey>` element carries, in whichever encoding it used.
///
/// The curve comes from the **record's** `SA`, because `values.xsd` has no field
/// for it and `[OCMF Tab. 23]` pairs the two: a key that does not belong to the
/// declared algorithm is a mismatch [`crate::ocmf::verify`] reports by name.
///
/// `encoding` is honoured when the file states one. When it does not — which
/// the reference data set's own sample omits — or when it says `plain`, which
/// one vendor uses for hex and the schema does not define for keys, the bytes
/// are decoded as hex and then as base64. Guessing is confined to the encoding,
/// where a wrong guess simply fails to decode; nothing about the key's *meaning*
/// is inferred.
fn decode_key(
    record: &OcmfRecord,
    encoding: Option<&str>,
    text: &str,
) -> Result<PublicKey, TransparencyError> {
    let algorithm = SignatureAlgorithm::parse(&record.signature.algorithm)
        .ok()
        .and_then(SignatureAlgorithm::key_type)
        .ok_or_else(|| TransparencyError::UnpairableKey {
            algorithm: record.signature.algorithm.clone(),
        })?;

    let attempts: &[&str] = match encoding {
        Some("hex") => &["hex"],
        Some("base64") => &["base64"],
        None | Some("plain") => &["hex", "base64"],
        Some(other) => {
            return Err(TransparencyError::UnknownKeyEncoding {
                encoding: other.to_owned(),
            });
        }
    };

    let mut last = None;
    for attempt in attempts {
        let decoded = if *attempt == "hex" {
            PublicKey::from_hex(algorithm, text)
        } else {
            PublicKey::from_base64(algorithm, text)
        };
        match decoded {
            Ok(key) => return Ok(key),
            Err(error) => last = Some(error),
        }
    }
    Err(TransparencyError::BadKey {
        detail: last.map_or_else(|| "no encoding attempted".to_owned(), |e| e.to_string()),
    })
}

/// The first `<name …>…</name>` in `input`: its attributes, its text, and what
/// follows the closing tag.
///
/// Deliberately not a parser. The container is flat, machine-generated and
/// declared by a schema with four element names in it, so matching those names
/// is enough — and anything else in the file is refused by the caller rather
/// than skipped.
fn find_element<'a>(input: &'a str, name: &str) -> Option<(&'a str, &'a str, &'a str)> {
    let mut from = 0;
    let open = loop {
        let at = from + input[from..].find(&format!("<{name}"))?;
        // `<value` must not match `<values`: the next character has to end the
        // name.
        let next = input[at + name.len() + 1..].chars().next();
        if next.is_none_or(|c| c.is_whitespace() || c == '>' || c == '/') {
            break at;
        }
        from = at + 1;
    };
    let after_name = open + name.len() + 1;
    let close_bracket = after_name + input[after_name..].find('>')?;
    let attrs = &input[after_name..close_bracket];

    // A self-closing element carries no text.
    if attrs.trim_end().ends_with('/') {
        return Some((
            attrs.trim_end().trim_end_matches('/'),
            "",
            &input[close_bracket + 1..],
        ));
    }

    let end_tag = format!("</{name}>");
    let text_start = close_bracket + 1;
    let text_end = text_start + input[text_start..].find(&end_tag)?;
    Some((
        attrs,
        &input[text_start..text_end],
        &input[text_end + end_tag.len()..],
    ))
}

/// One attribute's value out of an element's attribute text.
fn attribute(attrs: &str, name: &str) -> Option<String> {
    let at = attrs.find(name)?;
    let rest = attrs[at + name.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &rest[1..];
    let end = value.find(quote)?;
    unescape(&value[..end]).ok()
}

/// Undo the escaping [`escape_with`] applies, plus the entities other writers
/// use.
///
/// An unknown entity is an error rather than a passthrough: a record whose text
/// this function guessed at is a record whose signature will fail for a reason
/// nobody can find.
fn unescape(value: &str) -> Result<String, TransparencyError> {
    if !value.contains('&') {
        return Ok(value.to_owned());
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let end = tail.find(';').ok_or_else(|| TransparencyError::BadEntity {
            entity: tail.chars().take(12).collect(),
        })?;
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            numeric if numeric.starts_with('#') => {
                let (digits, radix) = numeric
                    .strip_prefix("#x")
                    .or_else(|| numeric.strip_prefix("#X"))
                    .map_or((&numeric[1..], 10), |hex| (hex, 16));
                u32::from_str_radix(digits, radix)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| TransparencyError::BadEntity {
                        entity: entity.to_owned(),
                    })?
            }
            other => {
                return Err(TransparencyError::BadEntity {
                    entity: other.to_owned(),
                });
            }
        };
        out.push(decoded);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// What can be wrong with a transparency container.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TransparencyError {
    /// The input is not a `<values>` container at all.
    #[error("not a transparency container: no <values> element")]
    NotAContainer,

    /// A `<value>` is missing an element the schema requires.
    #[error("a <value> carries no <{element}>")]
    MissingElement {
        /// Which element.
        element: &'static str,
    },

    /// The container uses a construct this reader refuses to guess at.
    #[error(
        "this container uses {what}, which this reader refuses rather than half-understands: a verifier that guesses is the failure the file exists to prevent"
    )]
    Unsupported {
        /// What was found.
        what: &'static str,
    },

    /// A `<signedData>` did not parse as an OCMF record.
    #[error("a signed data set did not parse: {detail}")]
    BadRecord {
        /// What the OCMF parser said.
        detail: String,
    },

    /// A `<publicKey>` did not decode in any encoding the file permits.
    #[error("a public key did not decode: {detail}")]
    BadKey {
        /// What the decoder said.
        detail: String,
    },

    /// The `encoding` attribute names something `[OCMF]` does not define.
    #[error("a public key claims encoding {encoding}, which is not one this format defines")]
    UnknownKeyEncoding {
        /// What the file claimed.
        encoding: String,
    },

    /// The record's algorithm has no key type this build can hold.
    #[error(
        "a record signed with {algorithm} has no key type this build can verify against, so the key beside it cannot be read"
    )]
    UnpairableKey {
        /// The algorithm the record declared.
        algorithm: String,
    },

    /// An XML entity this reader does not know.
    #[error(
        "unknown XML entity &{entity};: a record whose text was guessed at is one whose signature fails for a reason nobody can find"
    )]
    BadEntity {
        /// The entity's name.
        entity: String,
    },
}

/// Escape text content: `&`, `<`, and `>` for good measure.
///
/// **Not** the quote. An OCMF record is mostly quotes, and escaping them would
/// turn every dataset into an unreadable wall of `&quot;` — legal XML that no
/// human can compare against the record the station emitted, in a file whose
/// entire purpose is being checkable by hand if it comes to that. The
/// Transparenzsoftware's own sample file carries raw quotes here.
///
/// Hand-written rather than pulled from a crate: this is the whole XML surface
/// of the workspace, and an OCMF payload can legitimately contain an `&` or a
/// `<` inside a tariff text — which would otherwise produce a file the verifier
/// refuses to parse, on the one occasion it is most needed.
fn escape_text(value: &str) -> String {
    escape_with(value, false)
}

/// Escape an attribute value: the same, plus the quote that would close it.
fn escape_attr(value: &str) -> String {
    escape_with(value, true)
}

fn escape_with(value: &str, quotes: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quotes => out.push_str("&quot;"),
            '\'' if quotes => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocmf::{self, KeyType, PublicKey};
    use crate::registry::{ComponentRef, KeyRegistry, RegisteredKey};
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use p256::ecdsa::{DerSignature, SigningKey};
    use sha2::{Digest, Sha256};
    use time::macros::datetime;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
    }

    fn key_bytes() -> Vec<u8> {
        signing_key()
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }

    fn sign(payload: &str) -> String {
        let digest = Sha256::digest(payload.as_bytes());
        let sig: DerSignature = signing_key().sign_prehash(&digest).unwrap();
        format!(
            "OCMF|{payload}|{{\"SD\":\"{}\"}}",
            hex::encode(sig.as_bytes())
        )
    }

    fn registry() -> KeyRegistry {
        let mut r = KeyRegistry::new();
        r.insert(
            ComponentRef::Meter {
                serial: "BQ1".into(),
            },
            RegisteredKey::unbounded(
                PublicKey {
                    algorithm: KeyType::Secp256r1,
                    bytes: key_bytes(),
                },
                "type approval 2026-01",
            ),
        )
        .unwrap();
        r
    }

    fn payload(pg: u64, tx: &str, value: &str, minute: u8, extra: &str) -> String {
        format!(
            r#"{{"PG":"T{pg}","MS":"BQ1",{extra}"RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}"#
        )
    }

    fn evidence_of(raws: &[String]) -> Evidence {
        let records: Vec<_> = raws.iter().map(|r| ocmf::parse(r).unwrap()).collect();
        Evidence::assemble(&records, &registry(), datetime!(2026-01-02 10:00 +1))
    }

    fn session() -> Evidence {
        evidence_of(&[
            sign(&payload(1, "B", "2935.600", 0, "")),
            sign(&payload(2, "E", "2965.100", 20, "")),
        ])
    }

    #[test]
    fn the_file_holds_each_record_verbatim_beside_the_key_that_checked_it() {
        let evidence = session();
        let xml = to_xml(&evidence);

        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<values>"));
        assert!(xml.trim_end().ends_with("</values>"));
        assert_eq!(xml.matches("<value ").count(), 2);

        // The record, byte for byte. A reassembled one would hash differently
        // and the verifier would reject it.
        for record in &evidence.verified {
            assert!(
                xml.contains(record.record.as_str()),
                "the record must appear verbatim"
            );
        }
        // …and the key it was actually checked against, not one chosen later.
        assert!(xml.contains(&hex::encode_upper(key_bytes())));
        assert!(xml.contains("format=\"OCMF\" encoding=\"plain\""));
        assert!(xml.contains("<publicKey encoding=\"hex\">"));
    }

    #[test]
    fn the_elements_come_out_in_the_order_the_schema_sequences_them() {
        // `values.xsd` declares `<xs:sequence>` with `publicKey` first. A file
        // written the other way round is schema-invalid, and the only reason it
        // works today is that the reference implementation does not validate
        // against its own schema.
        let xml = to_xml(&session());
        let key = xml.find("<publicKey").expect("a key element");
        let data = xml.find("<signedData").expect("a data element");
        assert!(key < data, "publicKey must precede signedData");
    }

    #[test]
    fn the_context_labels_are_the_ones_the_verifier_names() {
        let xml = to_xml(&session());
        assert!(xml.contains("context=\"Transaction.Begin\""));
        assert!(xml.contains("context=\"Transaction.End\""));
    }

    #[test]
    fn every_record_of_one_session_carries_one_transaction_id() {
        // The attribute the verifier *groups* by. `MainView` collects
        // `getValues(currentTransactionId)` and hands the whole list to
        // `verifyTransaction`, which is where the begin/end pairing and the
        // energy difference happen — so numbering the records instead of naming
        // the transaction degrades one session into two the driver cannot pair.
        // The reference data set carries one id across both halves.
        let xml = to_xml(&session());
        assert_eq!(
            xml.matches("transactionId=\"1\"").count(),
            2,
            "both records belong to the transaction that opened at PG=T1: {xml}"
        );
        assert!(
            !xml.contains("transactionId=\"2\""),
            "the second record's pagination counter is not a second transaction"
        );
    }

    #[test]
    fn the_operators_own_transaction_number_can_be_used_instead() {
        // What makes the driver's file line up with the driver's invoice.
        let xml = to_xml_with_transaction_id(&session(), 4_053_006);
        assert_eq!(xml.matches("transactionId=\"4053006\"").count(), 2);
    }

    #[test]
    fn a_record_that_is_a_whole_transaction_is_labelled_neither_half() {
        // The `MR` configuration, and the shape of the eBZ LD3 reference data
        // set: one signed data set carrying `TX=B` and `TX=E`. Reading only the
        // first reading labelled it `Transaction.Begin`, which is a claim the
        // record does not make. The attribute is optional, and the reference
        // sample files omit it for exactly this shape.
        let whole = evidence_of(&[sign(concat!(
            r#"{"PG":"T1","MS":"BQ1","RD":["#,
            r#"{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":2935.600,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"},"#,
            r#"{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"EF":"","ST":"G"}]}"#,
        ))]);
        assert_eq!(whole.verified.len(), 1);

        let xml = to_xml(&whole);
        assert!(!xml.contains("context="), "{xml}");
        assert!(xml.contains("<value transactionId=\"1\">"), "{xml}");
    }

    #[test]
    fn a_session_that_does_not_bill_still_gets_a_file() {
        // The customer's right to check does not depend on the answer, and a
        // dispute is exactly when the file matters most.
        let evidence = evidence_of(&[
            sign(&payload(1, "B", "2935.600", 0, "")),
            sign(&payload(3, "E", "2965.100", 20, "")), // pagination hole
        ]);
        assert!(!evidence.is_billable());

        let xml = to_xml(&evidence);
        assert_eq!(
            xml.matches("<value ").count(),
            2,
            "both signatures are genuine and both records are handed over"
        );
    }

    #[test]
    fn a_record_that_did_not_verify_has_no_key_binding_to_export() {
        let tampered = sign(&payload(2, "E", "2965.100", 20, "")).replace("2965.100", "9965.100");
        let evidence = evidence_of(&[sign(&payload(1, "B", "2935.600", 0, "")), tampered]);

        let xml = to_xml(&evidence);
        assert_eq!(
            xml.matches("<value ").count(),
            1,
            "a forged record has no key it was checked against"
        );
        assert!(
            evidence.reasons().any(|r| r.contains("record 2")),
            "and the reason says which one"
        );
    }

    #[test]
    fn an_ampersand_in_a_tariff_text_does_not_break_the_file() {
        // OCMF's `TT` is free text. An unescaped `&` produces a file the
        // verifier refuses to parse, on the one occasion it is most needed.
        let evidence = evidence_of(&[
            sign(&payload(
                1,
                "B",
                "2935.600",
                0,
                r#""IS":true,"IL":"TRUSTED","IT":"CENTRAL","TT":"Tarif A & B <night>","#,
            )),
            sign(&payload(2, "E", "2965.100", 20, "")),
        ]);

        let xml = to_xml(&evidence);
        assert!(xml.contains("Tarif A &amp; B &lt;night&gt;"));
        assert!(
            !xml.contains("Tarif A & B"),
            "the raw ampersand must not survive into the XML"
        );
    }

    #[test]
    fn quotes_are_left_alone_in_the_dataset() {
        // A wall of `&quot;` is legal XML that nobody can compare against the
        // record the station emitted — and the reference verifier's own sample
        // file carries raw quotes here.
        let xml = to_xml(&session());
        assert!(xml.contains(r#""PG":"T1""#));
        assert!(!xml.contains("&quot;"));
    }

    #[test]
    fn a_file_this_crate_wrote_reads_back_byte_for_byte() {
        // The round trip that makes the reader worth having: what comes back
        // has to be the same record, because the signature is over those bytes
        // and a reader that normalises anything has produced a different
        // artefact with the same appearance.
        let evidence = session();
        let xml = to_xml_with_transaction_id(&evidence, 4_053_006);
        let values = from_xml(&xml).unwrap();

        assert_eq!(values.len(), 2);
        for (value, verified) in values.iter().zip(&evidence.verified) {
            assert_eq!(value.record.as_str(), verified.record.as_str());
            assert_eq!(value.record.signed_bytes(), verified.record.signed_bytes());
            assert_eq!(value.claimed_key.as_ref(), Some(&verified.key));
            assert_eq!(value.transaction_id, Some(4_053_006));
        }
        assert_eq!(values[0].context.as_deref(), Some("Transaction.Begin"));
        assert_eq!(values[1].context.as_deref(), Some("Transaction.End"));

        // …and the records that came back still verify, which is the whole
        // claim the file makes.
        let reparsed: Vec<_> = values.into_iter().map(|v| v.record).collect();
        let again = Evidence::assemble(&reparsed, &registry(), datetime!(2026-01-02 10:00 +1));
        assert_eq!(
            again.billable_energy().unwrap().to_string(),
            "29.500 kWh",
            "{:?}",
            again.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_escaped_record_survives_the_round_trip() {
        // The one place the file is not the record: `&`, `<` and `>` are
        // escaped on the way out and have to come back exactly, or the payload
        // hashes to something the station never signed.
        let evidence = evidence_of(&[
            sign(&payload(
                1,
                "B",
                "2935.600",
                0,
                r#""IS":true,"IL":"TRUSTED","IT":"CENTRAL","TT":"A & B <night> \"peak\"","#,
            )),
            sign(&payload(2, "E", "2965.100", 20, "")),
        ]);

        let values = from_xml(&to_xml(&evidence)).unwrap();
        assert_eq!(
            values[0].record.as_str(),
            evidence.verified[0].record.as_str()
        );
        assert_eq!(
            values[0]
                .record
                .payload
                .identification
                .as_ref()
                .unwrap()
                .tariff_text
                .as_deref(),
            Some("A & B <night> \"peak\"")
        );

        let reparsed: Vec<_> = values.into_iter().map(|v| v.record).collect();
        assert!(
            Evidence::assemble(&reparsed, &registry(), datetime!(2026-01-02 10:00 +1))
                .is_billable(),
            "an escaped record must still verify after the round trip"
        );
    }

    /// A container the reference implementation ships as its own test fixture
    /// (`src/test/resources/xml/OCMF_Test_Data_00.xml`, © S.A.F.E. e.V.,
    /// Apache-2.0), reduced to one `<value>`.
    ///
    /// Written by somebody else, and it differs from this crate's output in
    /// four ways at once: `signedData` comes **first**, neither element carries
    /// a `format` or `encoding` attribute, the key is base64 with no encoding
    /// stated, and both halves of the session share one `transactionId`.
    const REFERENCE_CONTAINER: &str = concat!(
        r#"<?xml version="1.0"?>"#,
        r#"<values><value transactionId="848182519" context="Transaction.Begin"><signedData>"#,
        r#"OCMF|{"FV" : "1.0","GI" : "Nano CH-10311C","GS" : "060643","GV" : "v017","PG" : "T198","MV" : "DZG","MM" : "DVH4013","MS" : "1DZG0033016824","IS" : true,"IL" : "VERIFIED","IF" : ["RFID_NONE","OCPP_NONE","ISO15118_NONE","PLMN_NONE"],"IT" : "EMAID","ID" : "04ab076a345b85","CT" : "CBIDC","CI" : "CI","#,
        r#""RD" : [{"TM" : "2021-10-26T10:20:52,000+0200 I","TX" : "B","RV" : "       9.038","RI" : "01-00:01.08.00.FF","RU" : "kWh","RT" : "AC","EF" : "","ST" : "G"}]}"#,
        r#"|{"SA" : "ECDSA-secp256k1-SHA256","SD" : "3046022100A4C188533ECA1793336520F7F99E010E62DEC32ABD344A562B00D396F65DFFE9022100CB0FB3782E406525641D689F4326D2118365A722EE75AAAB976C14B090BE49DA"}"#,
        r#"</signedData><publicKey>MFYwEAYHKoZIzj0CAQYFK4EEAAoDQgAEqHEykfqZhspgok6zCQh/329B38xine8ujzT8p5Nh7lek47cYeZj507aN6E4/QirF1b7Q57ln4VGfK6h0d0GOQA==</publicKey></value></values>"#,
    );

    #[test]
    fn a_container_this_crate_did_not_write_reads_and_verifies() {
        // The question a round trip cannot answer: does the reader agree with
        // somebody else's writer? Four differences from this crate's own output
        // in one file, and the key it carries actually checks the record.
        let values = from_xml(REFERENCE_CONTAINER).unwrap();
        assert_eq!(values.len(), 1);

        let value = &values[0];
        assert_eq!(value.transaction_id, Some(848_182_519));
        assert_eq!(value.context.as_deref(), Some("Transaction.Begin"));
        assert_eq!(
            value.record.payload.meter_serial.as_deref(),
            Some("1DZG0033016824")
        );
        assert_eq!(
            value.record.payload.readings[0].value.unwrap().to_string(),
            "9.038"
        );

        // The key was written base64 with no `encoding` attribute at all, and
        // the curve came from the record's own `SA`.
        let key = value.claimed_key.as_ref().expect("the file carries a key");
        assert_eq!(key.algorithm, KeyType::Secp256k1);
        crate::ocmf::verify(&value.record, key)
            .expect("the key the file carries must actually check the record it sits beside");
    }

    #[test]
    fn a_container_this_reader_does_not_understand_is_refused_rather_than_guessed() {
        assert!(matches!(
            from_xml("just some text"),
            Err(TransparencyError::NotAContainer)
        ));
        assert!(matches!(
            from_xml("<values><!-- a comment --></values>"),
            Err(TransparencyError::Unsupported { .. })
        ));
        assert!(matches!(
            from_xml("<values><value><signedData>not OCMF</signedData></value></values>"),
            Err(TransparencyError::BadRecord { .. })
        ));
        assert!(matches!(
            from_xml("<values><value><publicKey>00</publicKey></value></values>"),
            Err(TransparencyError::MissingElement {
                element: "signedData"
            })
        ));

        // An entity nobody defined is refused, because a record whose text was
        // guessed at fails its signature for a reason nobody can find.
        let evidence = session();
        let broken = to_xml(&evidence).replace("OCMF|", "OCMF&nbsp;|");
        assert!(matches!(
            from_xml(&broken),
            Err(TransparencyError::BadEntity { .. })
        ));

        // An empty container is a container, not a fault.
        assert!(
            from_xml(&to_xml(&Evidence::assemble(
                &[],
                &registry(),
                datetime!(2026-01-02 10:00 +1)
            )))
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn a_key_the_file_left_out_is_absent_rather_than_invented() {
        // `values.xsd` makes `publicKey` optional and the Transparenzsoftware
        // asks the user for one. A reader that substituted anything here would
        // be inventing the binding the whole out-of-band rule exists to protect.
        let without = REFERENCE_CONTAINER
            .split("<publicKey>")
            .next()
            .unwrap()
            .to_owned()
            + "</value></values>";
        let values = from_xml(&without).unwrap();
        assert_eq!(values.len(), 1);
        assert!(values[0].claimed_key.is_none());
    }

    #[test]
    fn an_empty_evidence_record_produces_a_well_formed_empty_file() {
        let evidence = Evidence::assemble(&[], &registry(), datetime!(2026-01-02 10:00 +1));
        let xml = to_xml(&evidence);
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<values>\n</values>\n"
        );
    }
}
