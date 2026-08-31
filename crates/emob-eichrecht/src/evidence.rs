//! The evidence record: what has to survive, and for how long.
//!
//! # The promise
//!
//! Under `[MessEG §33]` a customer may be billed for a measured value only if
//! they can check it — and `[PTB-A 50.7]` puts no short limit on when. So the
//! artefact a session leaves behind is not a number: it is the signed records
//! themselves, the key they were checked against, and the verdict, kept
//! together so that a dispute two years later is answered by replaying the
//! check rather than by trusting a log line.
//!
//! [`Evidence`] is that artefact. It is built by [`Evidence::assemble`], which
//! is the **only** place in the workspace where "this session may be billed for
//! this many kWh" is decided, and it carries the reasons when it decides not.
//!
//! # Ordering
//!
//! Verification runs before chain validation, deliberately. A chain whose
//! records were not all signed by the registered key is not a chain with a
//! finding in it — it is not evidence at all, and reporting "pagination break"
//! about forged records would be answering the wrong question.

use emob_core::Energy;

use crate::chain::{self, ChainFinding, ChainReport};
use crate::error::VerifyError;
use crate::ocmf::{self, OcmfRecord};
use crate::registry::KeyRegistry;

/// Why a session could not be billed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceProblem {
    /// A record's signature did not verify, or could not be checked.
    Signature {
        /// Which record, by pagination counter.
        pagination: u64,
        /// What went wrong.
        error: VerifyError,
    },
    /// The chain of records does not hold together.
    Chain(ChainFinding),
}

impl core::fmt::Display for EvidenceProblem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Signature { pagination, error } => {
                write!(f, "record {pagination}: {error}")
            }
            Self::Chain(finding) => write!(f, "{finding}"),
        }
    }
}

/// One verified record, with the proof that it was verified.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedRecord {
    /// The record.
    pub record: OcmfRecord,
    /// SHA-256 of the payload the signature covers — a stable content address.
    pub payload_digest: [u8; 32],
    /// Where the key that checked it came from.
    pub key_provenance: String,
}

/// Everything a session leaves behind, and the verdict it supports.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// The records whose signatures checked out, in order.
    pub verified: Vec<VerifiedRecord>,
    /// Everything standing between these records and an invoice.
    pub problems: Vec<EvidenceProblem>,
    /// The chain report, when the signatures allowed one to be produced.
    pub chain: Option<ChainReport>,
}

impl Evidence {
    /// Verify every record, then validate the chain, then decide.
    ///
    /// `at` is the instant the key registry is consulted for — normally the
    /// session's own start, so that a station whose key was later replaced is
    /// still checked against the key it actually signed with.
    #[must_use]
    pub fn assemble(
        records: &[OcmfRecord],
        registry: &KeyRegistry,
        at: time::OffsetDateTime,
    ) -> Self {
        let mut verified = Vec::with_capacity(records.len());
        let mut problems = Vec::new();

        for record in records {
            let pagination = record.payload.pagination.number;
            match registry.key_for_record(record, at) {
                Ok(registered) => match ocmf::verify(record, &registered.key) {
                    Ok(()) => verified.push(VerifiedRecord {
                        record: record.clone(),
                        payload_digest: ocmf::payload_digest(record),
                        key_provenance: registered.provenance.clone(),
                    }),
                    Err(error) => problems.push(EvidenceProblem::Signature { pagination, error }),
                },
                Err(error) => problems.push(EvidenceProblem::Signature { pagination, error }),
            }
        }

        // A chain assembled from records that failed verification would be a
        // report about forgeries. Only validate when every record held up.
        let chain = if problems.is_empty() {
            let report = chain::validate(records);
            problems.extend(report.findings.iter().cloned().map(EvidenceProblem::Chain));
            Some(report)
        } else {
            None
        };

        Self {
            verified,
            problems,
            chain,
        }
    }

    /// The energy this session may be billed for.
    ///
    /// `None` whenever anything at all went wrong. **A value that does not
    /// verify does not bill** — and because the only way to reach the number is
    /// through this method, that is a property of the type rather than a rule
    /// somebody has to remember.
    #[must_use]
    pub fn billable_energy(&self) -> Option<Energy> {
        if !self.problems.is_empty() {
            return None;
        }
        self.chain.as_ref()?.billable_energy
    }

    /// Whether this session may be billed at all.
    #[must_use]
    pub fn is_billable(&self) -> bool {
        self.billable_energy().is_some()
    }

    /// A one-line reason per problem, for an operator queue.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.problems.iter().map(ToString::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocmf::{KeyType, PublicKey};
    use crate::registry::{ComponentRef, RegisteredKey};
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use p256::ecdsa::{DerSignature, SigningKey};
    use sha2::{Digest, Sha256};
    use time::macros::datetime;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
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
                    bytes: signing_key()
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes()
                        .to_vec(),
                },
                "test fixture",
            ),
        );
        r
    }

    fn payload(pg: u64, tx: &str, value: &str, minute: u8) -> String {
        format!(
            r#"{{"PG":"T{pg}","MS":"BQ1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}"#
        )
    }

    fn session() -> Vec<OcmfRecord> {
        vec![
            ocmf::parse(&sign(&payload(1, "B", "2935.600", 0))).unwrap(),
            ocmf::parse(&sign(&payload(2, "E", "2965.100", 20))).unwrap(),
        ]
    }

    const AT: time::OffsetDateTime = datetime!(2026-01-02 10:00 +1);

    #[test]
    fn a_genuine_session_bills() {
        let evidence = Evidence::assemble(&session(), &registry(), AT);
        assert!(
            evidence.problems.is_empty(),
            "{:?}",
            evidence.reasons().collect::<Vec<_>>()
        );
        assert_eq!(
            evidence.billable_energy().unwrap().to_string(),
            "29.500 kWh"
        );
        assert_eq!(evidence.verified.len(), 2);
        assert_eq!(evidence.verified[0].key_provenance, "test fixture");
    }

    #[test]
    fn a_tampered_value_rates_nothing_and_says_why() {
        // The headline claim of the whole crate, as a test.
        let mut records = session();
        let tampered =
            ocmf::parse(&sign(&payload(2, "E", "2965.100", 20)).replace("2965.100", "9965.100"))
                .unwrap();
        records[1] = tampered;

        let evidence = Evidence::assemble(&records, &registry(), AT);
        assert!(!evidence.is_billable());
        assert_eq!(evidence.billable_energy(), None);
        assert!(matches!(
            evidence.problems[0],
            EvidenceProblem::Signature {
                pagination: 2,
                error: VerifyError::SignatureMismatch
            }
        ));
        let reason = evidence.reasons().next().unwrap();
        assert!(reason.contains("record 2"), "{reason}");
    }

    #[test]
    fn an_unregistered_station_cannot_bill() {
        let records = session();
        let empty = KeyRegistry::new();
        let evidence = Evidence::assemble(&records, &empty, AT);
        assert!(!evidence.is_billable());
        assert!(
            evidence.chain.is_none(),
            "no chain report over unverified records"
        );
    }

    #[test]
    fn a_deleted_middle_record_is_caught_after_verification() {
        let records = vec![
            ocmf::parse(&sign(&payload(1, "B", "2935.600", 0))).unwrap(),
            ocmf::parse(&sign(&payload(3, "E", "2965.100", 20))).unwrap(),
        ];
        let evidence = Evidence::assemble(&records, &registry(), AT);
        // Every signature is genuine…
        assert!(
            evidence
                .problems
                .iter()
                .all(|p| matches!(p, EvidenceProblem::Chain(_))),
            "signatures are fine; the chain is not"
        );
        // …and the session still does not bill.
        assert!(!evidence.is_billable());
    }

    #[test]
    fn the_digest_is_recorded_for_every_verified_record() {
        let evidence = Evidence::assemble(&session(), &registry(), AT);
        assert_ne!(
            evidence.verified[0].payload_digest,
            evidence.verified[1].payload_digest
        );
        assert_eq!(
            evidence.verified[0].payload_digest,
            ocmf::payload_digest(&evidence.verified[0].record)
        );
    }
}
