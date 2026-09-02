//! The file the driver checks the bill with.
//!
//! # What `[MessEG §33]` actually asks for
//!
//! The law does not require a measured value to be *correct*. It requires the
//! affected party to be able to **check** it — months later, with software the
//! operator did not write. In this market that software is the S.A.F.E.
//! Transparenzsoftware, and its input is an XML container of `<value>` elements,
//! each carrying one signed record and the public key it verifies against.
//!
//! So the deliverable at the end of a session is not a PDF saying the meter was
//! fine. It is this file.
//!
//! # The container is `ocmf`'s, and the grouping is the reason
//!
//! Writing the container is [`ocmf::xml`]'s job, and the part that is easy to
//! get wrong is not the schema. It is `transactionId`: the verifier **groups**
//! by it, and then demands exactly one `Transaction.Begin` and one
//! `Transaction.End` per group. A writer that numbers its records `1, 2, 3…`
//! produces a file that is schema-valid, verbatim in every dataset, beside the
//! right key — and refused, on records that verify perfectly one at a time.
//!
//! No test this workspace can write catches that, because the file is exactly
//! what the writer intended. `ocmf::xml` groups the way S.A.F.E.'s **own 257
//! reference values** are grouped, counted rather than assumed — including the
//! 223 of them that carry a begin *and* an end in one record and are therefore
//! written with no `transactionId` at all.
//!
//! # What is left here
//!
//! One question that is this crate's: which records go in. Only the ones whose
//! signatures verified against a **registered** key belong in a file offered as
//! evidence, and [`Evidence`] is where that was decided. A container built from
//! raw records would hand a driver a file whose verifier says "valid" about a
//! record no registry ever vouched for.

use ocmf::xml::{Values, XmlError};

use crate::evidence::Evidence;

/// The S.A.F.E. transparency container for a session's verified records.
///
/// Every record that verified, each beside the key it verified against, grouped
/// the way the reference verifier reads them. Records that did **not** verify
/// are absent: a file offered as evidence states what held up, and a driver
/// running the official tool over it should see the same verdict this crate
/// reached.
///
/// # Errors
///
/// [`XmlError`] only when a verified record cannot be re-borrowed, which cannot
/// happen for a record that already parsed once — it is threaded through rather
/// than unwrapped because a panic on the evidence path is worse than an error.
pub fn to_xml(evidence: &Evidence) -> Result<String, TransparencyError> {
    let parsed: Vec<_> = evidence
        .verified
        .iter()
        .map(|v| v.record.record().map(|record| (record, &v.key)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(TransparencyError::Unreadable)?;

    Values::from_records(parsed.iter().map(|(record, key)| (record, Some(*key))))
        .to_xml()
        .map_err(TransparencyError::Xml)
}

/// Read a container back, for a dispute.
///
/// The other direction, and it exists because the file is evidence rather than
/// output: two years on, the question is what the driver was given, and the
/// answer has to be readable without the session that produced it.
///
/// # Errors
///
/// [`TransparencyError::Xml`] when the document is malformed or is not a
/// `<values>` file.
pub fn from_xml(xml: &str) -> Result<Values, TransparencyError> {
    Values::parse(xml).map_err(TransparencyError::Xml)
}

/// What can go wrong producing or reading a transparency container.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransparencyError {
    /// The container is malformed, or is not a `<values>` file.
    #[error(transparent)]
    Xml(XmlError),

    /// A verified record could not be re-borrowed from its own text.
    #[error("a verified record could not be read back: {0}")]
    Unreadable(ocmf::ParseError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ComponentRef, KeyRegistry, RegisteredKey};
    use ocmf::PublicKey;
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use time::macros::datetime;

    /// A record signed here and now, so the file this module writes is one a
    /// real key produced rather than a constant.
    fn signed(pagination: &str, marker: &str, value: &str) -> (String, PublicKey) {
        use p256::ecdsa::SigningKey;
        use sha2::{Digest as _, Sha256};

        let signing = SigningKey::from_bytes(&[7u8; 32].into()).expect("a valid scalar");
        let payload = format!(
            r#"{{"FV":"1.0","PG":"{pagination}","MS":"M-1","IS":true,"IL":"VERIFIED","IF":[],"IT":"NONE","RD":[{{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"{marker}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}"#
        );
        let digest: [u8; 32] = Sha256::digest(payload.as_bytes()).into();
        let signature: p256::ecdsa::DerSignature =
            signing.sign_prehash(&digest).expect("signing a digest");
        let text = format!(
            "OCMF|{payload}|{{\"SA\":\"ECDSA-secp256r1-SHA256\",\"SD\":\"{}\"}}",
            hex::encode(signature.as_bytes())
        );
        let key = PublicKey::from_sec1(
            ocmf::Curve::Secp256r1,
            p256::ecdsa::VerifyingKey::from(&signing)
                .to_encoded_point(false)
                .as_bytes(),
        )
        .expect("a valid key");
        (text, key)
    }

    fn evidence_of(texts: &[String], key: &PublicKey) -> Evidence {
        let mut registry = KeyRegistry::new();
        registry
            .insert(
                ComponentRef::Meter {
                    serial: "M-1".to_owned(),
                },
                RegisteredKey::unbounded(key.clone(), "test"),
            )
            .expect("one key, no overlap");
        let records: Vec<_> = texts
            .iter()
            .map(|t| ocmf::Record::parse(t).expect("a record this test signed"))
            .collect();
        Evidence::assemble(&records, &registry, datetime!(2026-01-02 10:00 +1))
    }

    #[test]
    fn the_file_holds_every_verified_record_and_the_key_it_verified_against() {
        let (begin, key) = signed("T1", "B", "10.0");
        let (end, _) = signed("T2", "E", "25.0");
        let evidence = evidence_of(&[begin.clone(), end.clone()], &key);
        assert!(evidence.is_billable(), "{:?}", evidence.problems);

        let xml = to_xml(&evidence).expect("a container");
        assert!(xml.contains(&begin), "the record goes in verbatim");
        assert!(xml.contains(&end));
        assert_eq!(from_xml(&xml).expect("readable").entries.len(), 2);

        // …and it reads back, which is what a dispute two years on needs.
        let back = from_xml(&xml).expect("the container this module wrote");
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].signed_data, begin);
    }

    #[test]
    fn a_session_is_one_transaction_and_not_two() {
        // The failure D74 records: the verifier *groups* by `transactionId` and
        // then wants exactly one begin and one end per group. Numbering the
        // records `1, 2` produces a schema-valid file it refuses.
        let (begin, key) = signed("T1", "B", "10.0");
        let (end, _) = signed("T2", "E", "25.0");
        let xml = to_xml(&evidence_of(&[begin, end], &key)).expect("a container");
        let back = from_xml(&xml).expect("readable");

        let ids: Vec<_> = back
            .entries
            .iter()
            .filter_map(|e| e.transaction_id.clone())
            .collect();
        assert_eq!(ids.len(), 2, "both halves carry an id");
        assert_eq!(ids[0], ids[1], "and it is the *same* id: one transaction");
    }

    #[test]
    fn a_record_that_did_not_verify_is_not_offered_as_evidence() {
        let (begin, key) = signed("T1", "B", "10.0");
        let (end, _) = signed("T2", "E", "25.0");
        // One digit of the closing register, changed after signing.
        let forged = end.replace("25.0", "99.0");

        let evidence = evidence_of(&[begin, forged], &key);
        assert!(!evidence.is_billable());
        let xml = to_xml(&evidence).expect("a container");
        assert_eq!(
            from_xml(&xml).expect("readable").entries.len(),
            1,
            "only the record that held up"
        );
        assert!(!xml.contains("99.0"));
    }
}
