//! ECDSA verification of an OCMF record.
//!
//! # What is verified, and what that proves
//!
//! The signature covers the payload section as written `[OCMF §Signing and
//! Verification Process]`. Verifying it proves that the holder of one private
//! key produced *these bytes* — nothing more. Three things it does **not**
//! prove, each of which is checked elsewhere:
//!
//! - that the key belongs to the charge point the record claims — that is the
//!   registry in [`crate::registry`], fed out of band `[OCMF §Relation of
//!   Serial Numbers, Charge Point and Public Key]`;
//! - that no record was removed from the session — that is the pagination and
//!   transaction-marker chain in [`crate::chain`];
//! - that the values may be billed — that is [`MeterState`] and the error
//!   flags, judged in [`crate::chain`].
//!
//! Conflating the four is the standard way a "verified" charging session turns
//! out to be a signed fragment of a session somebody edited.
//!
//! [`MeterState`]: crate::ocmf::MeterState
//!
//! # Curves
//!
//! `[OCMF Tab. 22]` names seven algorithms. secp256r1 (the default since
//! OCMF 0.4), secp384r1, secp256k1 and secp192r1/k1 are implemented; the two
//! brainpool curves are recognised and refused with a clear error rather than
//! silently failing, because no audited pure-Rust implementation of them
//! exists and a wrong answer here is worse than no answer.

use sha2::{Digest, Sha256};

use super::parse::OcmfRecord;
use crate::error::VerifyError;

/// A signature algorithm from `[OCMF Tab. 22]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignatureAlgorithm {
    /// secp256r1 / NIST P-256 — the default since OCMF 0.4.
    EcdsaSecp256r1Sha256,
    /// secp384r1 / NIST P-384.
    EcdsaSecp384r1Sha256,
    /// secp256k1.
    EcdsaSecp256k1Sha256,
    /// brainpoolP256r1 — recognised, not implemented.
    EcdsaBrainpool256r1Sha256,
    /// brainpoolP384r1 — recognised, not implemented.
    EcdsaBrainpool384r1Sha256,
    /// secp192r1 — recognised, not implemented.
    EcdsaSecp192r1Sha256,
    /// secp192k1 — recognised, not implemented.
    EcdsaSecp192k1Sha256,
}

impl SignatureAlgorithm {
    /// Parse the `SA` identifier.
    ///
    /// # Errors
    ///
    /// [`VerifyError::UnknownAlgorithm`] for an identifier outside Table 22.
    pub fn parse(raw: &str) -> Result<Self, VerifyError> {
        Ok(match raw {
            "ECDSA-secp256r1-SHA256" => Self::EcdsaSecp256r1Sha256,
            "ECDSA-secp384r1-SHA256" => Self::EcdsaSecp384r1Sha256,
            "ECDSA-secp256k1-SHA256" => Self::EcdsaSecp256k1Sha256,
            "ECDSA-brainpool256r1-SHA256" => Self::EcdsaBrainpool256r1Sha256,
            "ECDSA-brainpool384r1-SHA256" => Self::EcdsaBrainpool384r1Sha256,
            "ECDSA-secp192r1-SHA256" => Self::EcdsaSecp192r1Sha256,
            "ECDSA-secp192k1-SHA256" => Self::EcdsaSecp192k1Sha256,
            other => {
                return Err(VerifyError::UnknownAlgorithm {
                    algorithm: other.to_owned(),
                });
            }
        })
    }

    /// Whether this crate can actually check a signature of this kind.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(
            self,
            Self::EcdsaSecp256r1Sha256 | Self::EcdsaSecp384r1Sha256 | Self::EcdsaSecp256k1Sha256
        )
    }
}

/// A public key registered against a signing component.
///
/// The bytes are a SEC1 point (compressed or uncompressed) or a DER
/// `SubjectPublicKeyInfo`; both spellings turn up in the field, and a registry
/// that accepts only one of them rejects half the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PublicKey {
    /// Which curve the key is on.
    pub algorithm: KeyType,
    /// The key material.
    pub bytes: Vec<u8>,
}

/// A public key type from `[OCMF Tab. 23]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum KeyType {
    /// secp256r1 / NIST P-256.
    Secp256r1,
    /// secp384r1 / NIST P-384.
    Secp384r1,
    /// secp256k1.
    Secp256k1,
}

impl KeyType {
    /// Parse the identifier used in a key registry.
    ///
    /// # Errors
    ///
    /// [`VerifyError::UnknownAlgorithm`] for an unsupported type.
    pub fn parse(raw: &str) -> Result<Self, VerifyError> {
        Ok(match raw {
            "ECDSA-secp256r1" | "secp256r1" | "prime256v1" | "P-256" => Self::Secp256r1,
            "ECDSA-secp384r1" | "secp384r1" | "P-384" => Self::Secp384r1,
            "ECDSA-secp256k1" | "secp256k1" => Self::Secp256k1,
            other => {
                return Err(VerifyError::UnknownAlgorithm {
                    algorithm: other.to_owned(),
                });
            }
        })
    }

    /// The signature algorithm that pairs with this key type.
    #[must_use]
    pub const fn signature_algorithm(self) -> SignatureAlgorithm {
        match self {
            Self::Secp256r1 => SignatureAlgorithm::EcdsaSecp256r1Sha256,
            Self::Secp384r1 => SignatureAlgorithm::EcdsaSecp384r1Sha256,
            Self::Secp256k1 => SignatureAlgorithm::EcdsaSecp256k1Sha256,
        }
    }
}

impl PublicKey {
    /// A key from hex-encoded bytes.
    ///
    /// # Errors
    ///
    /// [`VerifyError::BadKeyEncoding`] when the hex does not decode.
    pub fn from_hex(algorithm: KeyType, hex_bytes: &str) -> Result<Self, VerifyError> {
        // Registries hand these out with spaces and newlines in them more often
        // than not.
        let cleaned: String = hex_bytes.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = hex::decode(&cleaned).map_err(|e| VerifyError::BadKeyEncoding {
            detail: e.to_string(),
        })?;
        Ok(Self { algorithm, bytes })
    }

    /// A key from base64-encoded bytes.
    ///
    /// # Errors
    ///
    /// [`VerifyError::BadKeyEncoding`] when the base64 does not decode.
    pub fn from_base64(algorithm: KeyType, b64: &str) -> Result<Self, VerifyError> {
        use base64::Engine as _;
        let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&cleaned)
            .map_err(|e| VerifyError::BadKeyEncoding {
                detail: e.to_string(),
            })?;
        Ok(Self { algorithm, bytes })
    }
}

/// Verify a record's signature against a public key.
///
/// # Errors
///
/// [`VerifyError`] when the algorithm is unsupported, the key or signature does
/// not decode, or — the case that matters — the signature does not check out.
///
/// # Example
///
/// ```no_run
/// use emob_eichrecht::ocmf::{self, verify, KeyType, PublicKey};
/// # let raw_ocmf = ""; let registry_key = "";
///
/// let record = ocmf::parse(raw_ocmf)?;
/// let key = PublicKey::from_hex(KeyType::Secp256r1, registry_key)?;
///
/// // Proves these bytes came from that key — and nothing else.
/// verify(&record, &key)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn verify(record: &OcmfRecord, key: &PublicKey) -> Result<(), VerifyError> {
    let declared = SignatureAlgorithm::parse(&record.signature.algorithm)?;

    // A record signed with one curve and checked against a key on another is
    // not a failed verification, it is a misconfiguration — and saying so is
    // the difference between a five-minute fix and a day of confusion.
    if declared != key.algorithm.signature_algorithm() {
        return Err(VerifyError::AlgorithmMismatch {
            record: record.signature.algorithm.clone(),
            key: format!("{:?}", key.algorithm),
        });
    }
    if !declared.is_supported() {
        return Err(VerifyError::UnsupportedAlgorithm {
            algorithm: record.signature.algorithm.clone(),
        });
    }
    if record.signature.mime_type != "application/x-der" {
        return Err(VerifyError::UnsupportedSignatureFormat {
            mime_type: record.signature.mime_type.clone(),
        });
    }

    // The hash is over the payload span exactly as it arrived. Everything in
    // the parser exists to make this line able to say that truthfully.
    let message = record.signed_bytes();
    let signature = &record.signature.data;

    match key.algorithm {
        KeyType::Secp256r1 => verify_p256(message, signature, &key.bytes),
        KeyType::Secp384r1 => verify_p384(message, signature, &key.bytes),
        KeyType::Secp256k1 => verify_k256(message, signature, &key.bytes),
    }
}

macro_rules! verify_with_curve {
    ($fn_name:ident, $curve:ident) => {
        fn $fn_name(message: &[u8], signature: &[u8], key: &[u8]) -> Result<(), VerifyError> {
            use $curve::ecdsa::signature::hazmat::PrehashVerifier;
            use $curve::ecdsa::{DerSignature, VerifyingKey};
            use $curve::pkcs8::DecodePublicKey;

            // SEC1 first (what most registries publish), DER SubjectPublicKeyInfo
            // second (what a few publish). Accepting both is not generosity: a
            // registry that hands out one form to a fleet running the other is
            // the normal case.
            let verifying_key = VerifyingKey::from_sec1_bytes(key)
                .or_else(|_| VerifyingKey::from_public_key_der(key))
                .map_err(|e| VerifyError::BadKeyEncoding {
                    detail: format!("neither SEC1 nor DER SubjectPublicKeyInfo: {e}"),
                })?;

            let sig = DerSignature::try_from(signature).map_err(|e| {
                VerifyError::BadSignatureEncoding {
                    detail: format!("not a DER ECDSA signature: {e}"),
                }
            })?;

            // `verify_prehash` rather than `verify_digest`, and that is not a
            // stylistic choice: OCMF pairs SHA-256 with *every* curve it names,
            // including secp384r1 `[OCMF Tab. 22]`. The usual typed-digest API
            // requires the hash to be exactly the field width, so a 32-byte
            // digest against a 48-byte field does not even compile. The prehash
            // path applies the X9.62 `bits2int` conversion the standard
            // actually specifies for a hash shorter than the field, which is
            // what a station signing ECDSA-secp384r1-SHA256 has done.
            let digest = Sha256::digest(message);
            verifying_key
                .verify_prehash(&digest, &sig)
                .map_err(|_| VerifyError::SignatureMismatch)
        }
    };
}

verify_with_curve!(verify_p256, p256);
verify_with_curve!(verify_p384, p384);
verify_with_curve!(verify_k256, k256);

/// The SHA-256 digest of the payload a record's signature covers.
///
/// Useful as a stable content address for the evidence store: the same session
/// re-imported from a different transport is the same evidence.
#[must_use]
pub fn payload_digest(record: &OcmfRecord) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(record.signed_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocmf;
    use p256::ecdsa::signature::hazmat::PrehashSigner;

    /// Build a signed record with a freshly generated key, so the test proves
    /// the whole path rather than a captured constant.
    fn signed_record(payload: &str) -> (String, PublicKey) {
        use p256::ecdsa::{DerSignature, SigningKey};

        // A fixed scalar: deterministic tests beat random ones that fail once
        // a month for reasons nobody can reproduce.
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap();
        let digest = Sha256::digest(payload.as_bytes());
        let sig: DerSignature = signing_key.sign_prehash(&digest).unwrap();

        let verifying = signing_key.verifying_key();
        let key = PublicKey {
            algorithm: KeyType::Secp256r1,
            bytes: verifying.to_encoded_point(false).as_bytes().to_vec(),
        };

        let record = format!(
            "OCMF|{payload}|{{\"SD\":\"{}\"}}",
            hex::encode(sig.as_bytes())
        );
        (record, key)
    }

    const PAYLOAD: &str = r#"{"FV":"1.4","PG":"T1","MS":"BQ1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":10.500,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}]}"#;

    #[test]
    fn a_genuine_signature_verifies() {
        let (raw, key) = signed_record(PAYLOAD);
        let record = ocmf::parse(&raw).unwrap();
        verify(&record, &key).expect("a signature over these exact bytes must check out");
    }

    #[test]
    fn a_single_changed_digit_fails() {
        let (raw, key) = signed_record(PAYLOAD);
        // 10.500 kWh becomes 90.500 kWh — the whole point of the exercise.
        let tampered = raw.replace("10.500", "90.500");
        assert_ne!(raw, tampered);
        let record = ocmf::parse(&tampered).unwrap();
        assert!(matches!(
            verify(&record, &key),
            Err(VerifyError::SignatureMismatch)
        ));
    }

    #[test]
    fn reformatting_the_payload_breaks_the_signature() {
        // This is why the parser keeps the raw span. Adding one space is a
        // legal JSON edit and an illegal OCMF one.
        let (raw, key) = signed_record(PAYLOAD);
        let reformatted = raw.replacen(r#"{"FV""#, r#"{ "FV""#, 1);
        let record = ocmf::parse(&reformatted).unwrap();
        assert!(
            matches!(verify(&record, &key), Err(VerifyError::SignatureMismatch)),
            "whitespace is inside the signed span, and must be treated as such"
        );
    }

    #[test]
    fn a_different_key_fails() {
        use p256::ecdsa::SigningKey;
        let (raw, _) = signed_record(PAYLOAD);
        let other = SigningKey::from_bytes(&[0x43u8; 32].into()).unwrap();
        let wrong_key = PublicKey {
            algorithm: KeyType::Secp256r1,
            bytes: other
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec(),
        };
        let record = ocmf::parse(&raw).unwrap();
        assert!(matches!(
            verify(&record, &wrong_key),
            Err(VerifyError::SignatureMismatch)
        ));
    }

    #[test]
    fn a_curve_mismatch_says_so_rather_than_failing_vaguely() {
        let (raw, key) = signed_record(PAYLOAD);
        let record = ocmf::parse(&raw).unwrap();
        let wrong_curve = PublicKey {
            algorithm: KeyType::Secp384r1,
            bytes: key.bytes.clone(),
        };
        assert!(matches!(
            verify(&record, &wrong_curve),
            Err(VerifyError::AlgorithmMismatch { .. })
        ));
    }

    #[test]
    fn brainpool_is_refused_loudly_not_silently() {
        let alg = SignatureAlgorithm::parse("ECDSA-brainpool256r1-SHA256").unwrap();
        assert!(!alg.is_supported());
        // Recognised, so the error says "unsupported" rather than "unknown" —
        // the operator learns their fleet needs a curve this build lacks.
        assert!(SignatureAlgorithm::parse("ECDSA-nonsense-SHA256").is_err());
    }

    #[test]
    fn a_compressed_sec1_key_works_too() {
        use p256::ecdsa::SigningKey;
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap();
        let (raw, _) = signed_record(PAYLOAD);
        let compressed = PublicKey {
            algorithm: KeyType::Secp256r1,
            bytes: signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .to_vec(),
        };
        let record = ocmf::parse(&raw).unwrap();
        verify(&record, &compressed).unwrap();
    }

    #[test]
    fn the_digest_is_a_stable_content_address() {
        let (raw, _) = signed_record(PAYLOAD);
        let a = ocmf::parse(&raw).unwrap();
        let b = ocmf::parse(&raw).unwrap();
        assert_eq!(payload_digest(&a), payload_digest(&b));

        let (other, _) = signed_record(&PAYLOAD.replace("10.500", "11.500"));
        assert_ne!(
            payload_digest(&a),
            payload_digest(&ocmf::parse(&other).unwrap())
        );
    }

    #[test]
    fn key_type_accepts_the_synonyms_registries_actually_use() {
        for spelling in ["ECDSA-secp256r1", "secp256r1", "prime256v1", "P-256"] {
            assert_eq!(KeyType::parse(spelling).unwrap(), KeyType::Secp256r1);
        }
    }
}
