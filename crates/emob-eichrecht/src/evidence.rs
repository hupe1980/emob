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

use emob_core::{Energy, IdentificationStrength};

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
    /// The key it was checked against.
    ///
    /// The key itself and not only a description of it, because the customer's
    /// own verifier needs it: a transparency file that named a key without
    /// carrying it would leave the driver to find it, which is the step
    /// `[MessEG §33]` exists to remove.
    pub key: crate::ocmf::PublicKey,
    /// Where that key came from — a type approval, a provisioning run.
    pub key_provenance: String,
}

/// Everything a session leaves behind, and the verdict it supports.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// The records whose signatures checked out, in order.
    pub verified: Vec<VerifiedRecord>,
    /// Everything wrong with these records.
    ///
    /// Not the same as "everything standing between them and an invoice", and
    /// the difference matters: a clock this build cannot bill a duration
    /// against is a problem that leaves the **energy** perfectly billable. Ask
    /// [`Self::billable_energy`] and [`Self::billable_duration`] what may
    /// actually be charged; read this to find out why not, and to see what a
    /// dispute will be about.
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
                        key: registered.key.clone(),
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
    /// `None` whenever a signature failed, or any chain finding disqualifies
    /// the energy. **A value that does not verify does not bill** — and because
    /// the only way to reach the number is through this method, that is a
    /// property of the type rather than a rule somebody has to remember.
    ///
    /// A chain report exists only when every record verified, so delegating to
    /// it is the whole check: `None` here covers a forged record and a deleted
    /// one alike.
    #[must_use]
    pub fn billable_energy(&self) -> Option<Energy> {
        self.chain.as_ref()?.billable_energy
    }

    /// The duration this session may be billed for.
    ///
    /// A **different** question, with a different answer. A session on an
    /// unsynchronised clock `[OCMF Tab. 19]`, or one whose `EF` flags mark the
    /// time unusable, has an energy an invoice may use and a duration it may
    /// not — and `[AFIR Art. 5(4)]` lets a tariff charge for both, so the two
    /// have to be answerable separately or a per-minute occupancy fee gets
    /// billed off a clock nobody can defend.
    #[must_use]
    pub fn billable_duration(&self) -> Option<time::Duration> {
        self.chain.as_ref()?.billable_duration
    }

    /// Which way the energy this session measured was flowing, when its
    /// register says so `[OCMF Tab. 25]`.
    ///
    /// The claim the CDR is cross-checked against. A session recorded as a draw
    /// whose signed register says `C2` is a V2G discharge being billed as
    /// consumption, and the two directions must never net.
    #[must_use]
    pub fn direction(&self) -> Option<emob_core::Direction> {
        self.chain.as_ref()?.direction
    }

    /// The cable loss compensated out of this session's register, when the
    /// meter reported it `[OCMF Tab. 7, CL]`.
    #[must_use]
    pub fn compensated_loss(&self) -> Option<Energy> {
        self.chain.as_ref()?.compensated_loss
    }

    /// How strongly the signed records say the user was identified.
    ///
    /// The weakest level any record asserted. `None` when no record carries a
    /// user assignment, or when the chain did not hold up.
    ///
    /// This is the number the CDR cross-check reads. Taking it from the signed
    /// record rather than from whatever a caller passed in is the difference
    /// between a check and a formality: a hand-filled field can be filled with
    /// the answer that makes the CDR build.
    #[must_use]
    pub fn identification_strength(&self) -> Option<IdentificationStrength> {
        self.chain.as_ref()?.identification
    }

    /// The SHA-256 digest of every verified record's payload, in order.
    #[must_use]
    pub fn payload_digests(&self) -> Vec<[u8; 32]> {
        self.verified.iter().map(|v| v.payload_digest).collect()
    }

    /// Whether this session's energy may be billed at all.
    #[must_use]
    pub fn is_billable(&self) -> bool {
        self.billable_energy().is_some()
    }

    /// Whether a time-priced tariff may be applied to this session.
    #[must_use]
    pub fn is_billable_for_time(&self) -> bool {
        self.billable_duration().is_some()
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
    fn a_bad_clock_is_a_problem_that_still_bills_the_energy() {
        // The reason `problems` is not the same list as "reasons this cannot be
        // invoiced": OCMF states the trustworthiness of the clock separately
        // from the register, and so does the verdict.
        let unsynchronised: Vec<OcmfRecord> = [
            payload(1, "B", "2935.600", 0),
            payload(2, "E", "2965.100", 20),
        ]
        .iter()
        .map(|p| ocmf::parse(&sign(&p.replace(":00,000+0100 S", ":00,000+0100 U"))).unwrap())
        .collect();

        let evidence = Evidence::assemble(&unsynchronised, &registry(), AT);

        assert!(!evidence.problems.is_empty(), "there is something to say");
        assert_eq!(
            evidence.billable_energy().unwrap().to_string(),
            "29.500 kWh",
            "…and it is not about the register"
        );
        assert!(!evidence.is_billable_for_time());
        assert!(
            evidence
                .reasons()
                .any(|r| r.contains("energy is unaffected")),
            "the message has to say so, or an operator escalates a session that bills"
        );
    }

    #[test]
    fn the_identification_comes_off_the_records() {
        let records: Vec<OcmfRecord> = [
            r#"{"PG":"T1","MS":"BQ1","IS":true,"IL":"TRUSTED","IT":"CENTRAL","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":2935.600,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}"#,
            r#"{"PG":"T2","MS":"BQ1","IS":true,"IL":"HEARSAY","IT":"ISO14443","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}"#,
        ]
        .iter()
        .map(|p| ocmf::parse(&sign(p)).unwrap())
        .collect();

        let evidence = Evidence::assemble(&records, &registry(), AT);
        assert_eq!(
            evidence.identification_strength(),
            Some(emob_core::IdentificationStrength::Hearsay),
            "a chain is only as strong as its weakest claim"
        );
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
