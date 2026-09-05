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

use ocmf::{PublicKey, Record, RecordBuf};

use crate::chain::{self, ChainFinding, ChainReport, Disqualifies};
use crate::error::EichrechtError;
use crate::registry::KeyRegistry;

/// Why a session could not be billed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceProblem {
    /// A record's signature did not verify, or could not be checked — or no
    /// key could be found to check it against.
    Signature {
        /// Which record, by pagination counter.
        pagination: u64,
        /// What went wrong.
        error: EichrechtError,
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
    /// The record, owning its own text.
    ///
    /// [`ocmf::RecordBuf`] rather than a parsed tree, because the signature
    /// covers **the bytes as they were written** and a structure that had to be
    /// re-serialised to be checked again would already have lost. An evidence
    /// artefact is re-checked years later; it keeps the text.
    pub record: RecordBuf,
    /// SHA-256 of the payload the signature covers — a stable content address.
    pub payload_digest: [u8; 32],
    /// The key it was checked against.
    ///
    /// The key itself and not only a description of it, because the customer's
    /// own verifier needs it: a transparency file that named a key without
    /// carrying it would leave the driver to find it, which is the step
    /// `[MessEG §33]` exists to remove.
    pub key: PublicKey,
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
        records: &[Record<'_>],
        registry: &KeyRegistry,
        at: time::OffsetDateTime,
    ) -> Self {
        let mut verified = Vec::with_capacity(records.len());
        let mut problems = Vec::new();

        for record in records {
            let pagination = record.payload().pagination().map_or(0, |p| p.number());
            let outcome = registry
                .key_for_record(record, at)
                .map_err(EichrechtError::Key)
                .and_then(|registered| {
                    ocmf::verify(record, &registered.key)
                        .map(|_| registered)
                        .map_err(EichrechtError::Signature)
                });
            match outcome {
                Ok(registered) => {
                    match RecordBuf::new(
                        record.as_str().to_owned(),
                        ocmf::Profile::Interop,
                        ocmf::Limits::default(),
                    ) {
                        Ok(owned) => verified.push(VerifiedRecord {
                            record: owned,
                            payload_digest: record.payload_digest(),
                            key: registered.key.clone(),
                            key_provenance: registered.provenance.clone(),
                        }),
                        Err(error) => problems.push(EvidenceProblem::Signature {
                            pagination,
                            error: EichrechtError::Parse(error),
                        }),
                    }
                }
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

    /// The intervals the signed records say the transaction was active and not
    /// charging `[OCMF Tab. 7, TX]`.
    ///
    /// What `[AFIR Art. 5(4)]`'s occupancy fee prices, stated by the component
    /// that signed the meter values rather than by the protocol the operator
    /// also controls. Empty for a station that never emits `TX=S`, which is
    /// most of them — the marker is optional — so this is evidence *for* a fee
    /// where it exists, never a precondition of one.
    #[must_use]
    pub fn suspended_intervals(&self) -> Vec<(time::OffsetDateTime, time::OffsetDateTime)> {
        self.chain
            .as_ref()
            .map(ChainReport::suspended_intervals)
            .unwrap_or_default()
    }

    /// The instants the signed records mark a tariff change at
    /// `[OCMF Tab. 7, TX]`.
    #[must_use]
    pub fn tariff_change_instants(&self) -> Vec<time::OffsetDateTime> {
        self.chain
            .as_ref()
            .map(ChainReport::tariff_change_instants)
            .unwrap_or_default()
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

    /// The reasons one **quantity** may not be billed.
    ///
    /// The whole claim of this crate is that a chain answers four questions and
    /// a fault takes away only what it disqualifies: an unsynchronised clock
    /// leaves the energy billable and takes the duration `[OCMF Tab. 19]`, a
    /// failed assignment takes the payer and leaves both. [`Self::reasons`] is
    /// the undifferentiated list — everything wrong with these records — and
    /// until now it was the only list a caller could get, so *"why can I not
    /// bill the minutes"* had no answer sharper than *"here is everything"*.
    ///
    /// A **signature** failure is in every answer. It stops the chain from being
    /// validated at all, so there is no finding to attribute and nothing about
    /// these records may be billed.
    ///
    /// Empty is the ordinary case, and it means the quantity is billable.
    pub fn reasons_for(&self, what: Disqualifies) -> Vec<String> {
        // No chain means the signatures did not hold, and a record nobody could
        // verify disqualifies everything.
        let Some(chain) = &self.chain else {
            return self.reasons().collect();
        };
        self.problems
            .iter()
            .filter(|problem| matches!(problem, EvidenceProblem::Signature { .. }))
            .map(ToString::to_string)
            .chain(chain.disqualifying(what).map(ToString::to_string))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ComponentRef, RegisteredKey};
    use ocmf::{Curve, PublicKey};
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
                PublicKey::from_sec1(
                    Curve::Secp256r1,
                    signing_key()
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes(),
                )
                .unwrap(),
                "test fixture",
            ),
        )
        .unwrap();
        r
    }

    fn payload(pg: u64, tx: &str, value: &str, minute: u8) -> String {
        format!(
            r#"{{"PG":"T{pg}","MS":"BQ1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}"#
        )
    }

    fn session_texts() -> Vec<String> {
        vec![
            sign(&payload(1, "B", "2935.600", 0)),
            sign(&payload(2, "E", "2965.100", 20)),
        ]
    }

    /// Parse a set of texts and assemble the evidence over them.
    ///
    /// The texts have to outlive the records, because a `Record` borrows the
    /// bytes its signature covers — which is the whole premise of the format
    /// and is why this helper takes them by reference rather than building them
    /// inside.
    fn assemble(texts: &[String], registry: &KeyRegistry) -> Evidence {
        let records: Vec<ocmf::Record<'_>> = texts
            .iter()
            .map(|t| ocmf::Record::parse(t).expect("a record these tests signed"))
            .collect();
        Evidence::assemble(&records, registry, AT)
    }

    const AT: time::OffsetDateTime = datetime!(2026-01-02 10:00 +1);

    #[test]
    fn a_genuine_session_bills() {
        let evidence = assemble(&session_texts(), &registry());
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
        let mut texts = session_texts();
        texts[1] = texts[1].replace("2965.100", "9965.100");

        let evidence = assemble(&texts, &registry());
        assert!(!evidence.is_billable());
        assert_eq!(evidence.billable_energy(), None);
        assert!(
            matches!(
                &evidence.problems[0],
                EvidenceProblem::Signature {
                    pagination: 2,
                    error: EichrechtError::Signature(_)
                }
            ),
            "{:?}",
            evidence.problems
        );
        let reason = evidence.reasons().next().unwrap();
        assert!(reason.contains("record 2"), "{reason}");
    }

    #[test]
    fn an_unregistered_station_cannot_bill() {
        let evidence = assemble(&session_texts(), &KeyRegistry::new());
        assert!(!evidence.is_billable());
        assert!(
            evidence.chain.is_none(),
            "no chain report over unverified records"
        );
        assert!(matches!(
            &evidence.problems[0],
            EvidenceProblem::Signature {
                error: EichrechtError::Key(_),
                ..
            }
        ));
    }

    #[test]
    fn a_deleted_middle_record_is_caught_after_verification() {
        let texts = vec![
            sign(&payload(1, "B", "2935.600", 0)),
            sign(&payload(3, "E", "2965.100", 20)),
        ];
        let evidence = assemble(&texts, &registry());
        // Every signature is genuine…
        assert!(
            evidence
                .problems
                .iter()
                .all(|p| matches!(p, EvidenceProblem::Chain(_))),
            "signatures are fine; the chain is not: {:?}",
            evidence.problems
        );
        // …and the session still does not bill.
        assert!(!evidence.is_billable());
    }

    #[test]
    fn a_bad_clock_is_a_problem_that_still_bills_the_energy() {
        // The reason `problems` is not the same list as "reasons this cannot be
        // invoiced": OCMF states the trustworthiness of the clock separately
        // from the register, and so does the verdict. This is the split
        // `Disqualifies` exists for, and the one `ocmf::session` leaves to us.
        let texts: Vec<String> = [
            payload(1, "B", "2935.600", 0),
            payload(2, "E", "2965.100", 20),
        ]
        .iter()
        .map(|p| sign(&p.replace(":00,000+0100 S", ":00,000+0100 U")))
        .collect();

        let evidence = assemble(&texts, &registry());

        assert!(!evidence.problems.is_empty(), "there is something to say");
        assert_eq!(
            evidence.billable_energy().unwrap().to_string(),
            "29.500 kWh",
            "…and it is not about the register"
        );
        assert!(!evidence.is_billable_for_time());
    }

    #[test]
    fn the_identification_comes_off_the_records() {
        let texts: Vec<String> = [
            r#"{"FV":"1.0","PG":"T1","MS":"BQ1","IS":true,"IL":"TRUSTED","IF":[],"IT":"CENTRAL","ID":"A","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":2935.600,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}"#,
            r#"{"FV":"1.0","PG":"T2","MS":"BQ1","IS":true,"IL":"HEARSAY","IF":[],"IT":"ISO14443","ID":"A","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}"#,
        ]
        .iter()
        .map(|p| sign(p))
        .collect();

        let evidence = assemble(&texts, &registry());
        assert_eq!(
            evidence.identification_strength(),
            Some(emob_core::IdentificationStrength::Hearsay),
            "a chain is only as strong as its weakest claim"
        );
        // …and a level that *changed* is its own finding, because a session
        // identified two ways is one nobody can attribute.
        assert!(
            evidence
                .reasons()
                .any(|r| r.contains("identification level changed")),
            "{:?}",
            evidence.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_digest_is_recorded_for_every_verified_record() {
        let texts = session_texts();
        let evidence = assemble(&texts, &registry());
        assert_ne!(
            evidence.verified[0].payload_digest,
            evidence.verified[1].payload_digest
        );
        assert_eq!(
            evidence.verified[0].payload_digest,
            evidence.verified[0]
                .record
                .record()
                .unwrap()
                .payload_digest()
        );
    }
}
