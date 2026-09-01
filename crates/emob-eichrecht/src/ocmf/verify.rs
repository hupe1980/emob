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
//! `[OCMF Tab. 22]` names seven algorithms, and four of them are implemented
//! here: secp256r1 (the default since OCMF 0.4), secp384r1, secp256k1 and
//! **secp192r1**.
//!
//! secp192r1 is not a legacy curiosity. The eBZ LD3 data set the S.A.F.E.
//! Transparenzsoftware ships as a reference sample is signed
//! `ECDSA-secp192r1-SHA256`, and that meter is ordinary German charging
//! hardware — a verifier without the curve simply cannot check a real fleet.
//!
//! The other three are recognised and refused **by name** rather than failing
//! vaguely, because the reasons differ and an operator needs to know which one
//! they have hit:
//!
//! | Algorithm | Why not |
//! |---|---|
//! | `ECDSA-brainpool256r1-SHA256` | `RustCrypto`'s `bp256` gates its field arithmetic behind `wip-arithmetic-do-not-use`; there is no usable pure-Rust implementation to verify with |
//! | `ECDSA-brainpool384r1-SHA256` | the same, in `bp384` |
//! | `ECDSA-secp192k1-SHA256` | no pure-Rust implementation is published at all |
//!
//! A wrong answer here is worse than no answer, so none of the three is ever
//! approximated with a neighbouring curve.

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
    /// secp192r1 / NIST P-192 — what the eBZ LD3 reference data set signs with.
    EcdsaSecp192r1Sha256,
    /// brainpoolP256r1 — recognised, not implemented.
    EcdsaBrainpool256r1Sha256,
    /// brainpoolP384r1 — recognised, not implemented.
    EcdsaBrainpool384r1Sha256,
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

    /// The key type a signature of this kind must be checked against
    /// `[OCMF Tab. 22–23]`.
    ///
    /// `None` for the three algorithms this build recognises and cannot verify,
    /// which is the same set [`Self::is_supported`] reports — stated twice
    /// because a caller holding a key and a caller holding a record ask
    /// different questions of the same table.
    #[must_use]
    pub const fn key_type(self) -> Option<KeyType> {
        match self {
            Self::EcdsaSecp256r1Sha256 => Some(KeyType::Secp256r1),
            Self::EcdsaSecp384r1Sha256 => Some(KeyType::Secp384r1),
            Self::EcdsaSecp256k1Sha256 => Some(KeyType::Secp256k1),
            Self::EcdsaSecp192r1Sha256 => Some(KeyType::Secp192r1),
            Self::EcdsaBrainpool256r1Sha256
            | Self::EcdsaBrainpool384r1Sha256
            | Self::EcdsaSecp192k1Sha256 => None,
        }
    }

    /// Whether this crate can actually check a signature of this kind.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(
            self,
            Self::EcdsaSecp256r1Sha256
                | Self::EcdsaSecp384r1Sha256
                | Self::EcdsaSecp256k1Sha256
                | Self::EcdsaSecp192r1Sha256
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
    /// secp192r1 / NIST P-192 / prime192v1.
    Secp192r1,
}

impl KeyType {
    /// Parse the identifier used in a key registry.
    ///
    /// Every spelling a registry, a type-approval document or an OpenSSL dump
    /// actually uses, because a key rejected for being spelled `prime192v1`
    /// instead of `secp192r1` is a session that cannot be billed for a reason
    /// nobody would think to look for.
    ///
    /// # Errors
    ///
    /// [`VerifyError::UnknownAlgorithm`] for an unsupported type.
    pub fn parse(raw: &str) -> Result<Self, VerifyError> {
        Ok(match raw {
            "ECDSA-secp256r1" | "secp256r1" | "prime256v1" | "P-256" => Self::Secp256r1,
            "ECDSA-secp384r1" | "secp384r1" | "P-384" => Self::Secp384r1,
            "ECDSA-secp256k1" | "secp256k1" => Self::Secp256k1,
            "ECDSA-secp192r1" | "secp192r1" | "prime192v1" | "P-192" => Self::Secp192r1,
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
            Self::Secp192r1 => SignatureAlgorithm::EcdsaSecp192r1Sha256,
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
        KeyType::Secp192r1 => verify_p192(message, signature, &key.bytes),
    }
}

// # Why the signature is normalised to low `s`
//
// For every ECDSA signature `(r, s)` over a curve of order `n`, the pair
// `(r, n − s)` verifies the same message under the same key: the verification
// equation squares away the sign. Both are correct signatures, and the standard
// `[OCMF Tab. 22]` names — plain ECDSA with SHA-256 — has nothing to say about
// which one a signer emits.
//
// Bitcoin does. Signature malleability lets a third party rewrite `s` as
// `n − s` and change a transaction's hash without invalidating it, so
// consensus rules require the low half — and `k256`, which exists primarily to
// serve that world, enforces the rule inside `verify`.
//
// A charging meter has never heard of any of this. A DZG DVH4013 behind a Nano
// gateway — in the S.A.F.E. reference data set, on secp256k1 — signs with
// `s` in the high half, `openssl dgst -verify` accepts it, the reference
// verifier accepts it, and this build rejected it as `SignatureMismatch`. Every
// session from such a meter would have been unbillable, with a diagnostic
// pointing at tampering.
//
// Normalising first is not leniency: `normalize_s` maps `(r, s)` to
// `(r, min(s, n − s))`, which accepts exactly the set plain ECDSA accepts and
// nothing more. A forged pair still fails the verification equation whichever
// half of the order it sits in. It is applied on every curve rather than only
// on secp256k1, because "which curve happens to enforce a rule its
// specification does not state" is not a fact this crate should depend on.
macro_rules! verify_with_curve {
    ($fn_name:ident, $curve:ident) => {
        fn $fn_name(message: &[u8], signature: &[u8], key: &[u8]) -> Result<(), VerifyError> {
            use $curve::ecdsa::signature::hazmat::PrehashVerifier;
            use $curve::ecdsa::{Signature, VerifyingKey};
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

            // Canonicalised first — see `canonical_der`. Real meters emit
            // fixed-width INTEGERs that a strict DER reader refuses, and
            // refusing them means refusing to bill a session the reference
            // verifier accepts.
            let der = canonical_der(signature)?;
            let sig = Signature::from_der(&der).map_err(|e| VerifyError::BadSignatureEncoding {
                detail: format!("not a DER ECDSA signature: {e}"),
            })?;
            // …and then normalised to low `s` — see `normalize_s` below.
            let sig = sig.normalize_s().unwrap_or(sig);

            // `verify_prehash` rather than `verify_digest`, and that is not a
            // stylistic choice: OCMF pairs SHA-256 with *every* curve it names
            // `[OCMF Tab. 22]`, and only secp256r1/k1 have a 32-byte field. The
            // usual typed-digest API requires the hash to be exactly the field
            // width, so neither a 32-byte digest against secp384r1's 48-byte
            // field nor the same digest against secp192r1's 24-byte field even
            // compiles. The prehash path applies the X9.62 `bits2int`
            // conversion the standard specifies in both directions — left-pad a
            // short hash, take the leftmost bits of a long one — which is what
            // a station signing either of them has done.
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
verify_with_curve!(verify_p192, p192);

/// Re-encode an ECDSA signature's two integers as canonical DER.
///
/// # Why this is not pedantry
///
/// `SM` says the signature is a "DER encoded ASN.1 structure"
/// `[OCMF Tab. 8]` — a `SEQUENCE { INTEGER r, INTEGER s }`. DER requires each
/// INTEGER to be *minimal* and *signed*: no superfluous leading `0x00`, and a
/// mandatory `0x00` when the top bit of the first octet is set.
///
/// Meters in the field do not do that. The eBZ LD3 data set the S.A.F.E.
/// Transparenzsoftware ships as a reference sample pads both integers to a
/// fixed 24 bytes regardless of sign, so its `r` begins `e1` with no `0x00` in
/// front — which a strict DER reader must read as a *negative* number — and its
/// `s` carries a leading `0x00` it does not need. The signature is perfectly
/// good; the wrapper is not. A verifier that rejects it rejects every session
/// from an ordinary German meter, for a reason that has nothing to do with
/// whether the meter value was tampered with.
///
/// So each INTEGER's content octets are read as an unsigned big-endian
/// magnitude — which is what the signer meant and what the reference
/// implementation effectively does — and re-emitted minimally. Nothing about
/// the signature's strength changes: `r` and `s` still have to satisfy the
/// verification equation, and a forged pair fails it whichever way it was
/// wrapped.
fn canonical_der(signature: &[u8]) -> Result<Vec<u8>, VerifyError> {
    fn bad(detail: &str) -> VerifyError {
        VerifyError::BadSignatureEncoding {
            detail: format!("not a DER ECDSA signature: {detail}"),
        }
    }

    /// Read one `tag`ged TLV, returning its content and the rest.
    fn tlv(input: &[u8], tag: u8) -> Result<(&[u8], &[u8]), VerifyError> {
        let (&found, rest) = input.split_first().ok_or_else(|| bad("truncated"))?;
        if found != tag {
            return Err(bad(&format!("expected tag {tag:#04x}, found {found:#04x}")));
        }
        let (&first, rest) = rest.split_first().ok_or_else(|| bad("truncated length"))?;
        // Long-form lengths are legal ASN.1 and no ECDSA signature this crate
        // will ever see needs more than two length octets, so one is enough.
        let (length, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else if first == 0x81 {
            let (&n, rest) = rest.split_first().ok_or_else(|| bad("truncated length"))?;
            (usize::from(n), rest)
        } else {
            return Err(bad("length is longer than any ECDSA signature"));
        };
        if rest.len() < length {
            return Err(bad("length runs past the end"));
        }
        Ok(rest.split_at(length))
    }

    /// One integer's content octets, minimally re-encoded.
    fn integer(content: &[u8]) -> Vec<u8> {
        let magnitude = content
            .iter()
            .position(|&b| b != 0)
            .map_or(&[][..], |i| &content[i..]);
        let mut out = vec![0x02];
        if magnitude.is_empty() {
            out.extend_from_slice(&[0x01, 0x00]);
            return out;
        }
        // The `0x00` DER demands when the leading octet would read as a sign
        // bit — the one the eBZ firmware omits.
        let needs_pad = usize::from(magnitude[0] & 0x80 != 0);
        out.push(u8::try_from(magnitude.len() + needs_pad).unwrap_or(u8::MAX));
        if needs_pad == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(magnitude);
        out
    }

    let (body, trailing) = tlv(signature, 0x30)?;
    if !trailing.is_empty() {
        return Err(bad("trailing bytes after the SEQUENCE"));
    }
    let (r, rest) = tlv(body, 0x02)?;
    let (s, rest) = tlv(rest, 0x02)?;
    if !rest.is_empty() {
        return Err(bad("the SEQUENCE holds more than two INTEGERs"));
    }

    let mut integers = integer(r);
    integers.extend(integer(s));
    let mut out = vec![0x30];
    if integers.len() < 0x80 {
        out.push(u8::try_from(integers.len()).unwrap_or(u8::MAX));
    } else {
        out.push(0x81);
        out.push(u8::try_from(integers.len()).map_err(|_| bad("signature is implausibly long"))?);
    }
    out.extend(integers);
    Ok(out)
}

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
        for spelling in [
            "ECDSA-brainpool256r1-SHA256",
            "ECDSA-brainpool384r1-SHA256",
            "ECDSA-secp192k1-SHA256",
        ] {
            let alg = SignatureAlgorithm::parse(spelling).unwrap();
            assert!(!alg.is_supported(), "{spelling}");
        }
        // Recognised, so the error says "unsupported" rather than "unknown" —
        // the operator learns their fleet needs a curve this build lacks.
        assert!(SignatureAlgorithm::parse("ECDSA-nonsense-SHA256").is_err());
    }

    /// A genuine eBZ LD3 record and its published key, from the reference data
    /// set the S.A.F.E. Transparenzsoftware ships (`ocmf-compleo-daten.xml`,
    /// © S.A.F.E. e.V., Apache-2.0).
    ///
    /// Every other test in this file signs its own fixture, which proves the
    /// code agrees with itself. This one proves it agrees with a **German meter
    /// in the field** — a different question, and the only one that matters
    /// when a driver arrives with a bill and the reference verifier.
    ///
    /// Three things it exercises at once: secp192r1, a key published as a DER
    /// `SubjectPublicKeyInfo` rather than a SEC1 point, and a SHA-256 digest
    /// wider than the 24-byte field, which is where the X9.62 `bits2int`
    /// truncation the prehash path applies has to be right.
    const EBZ_LD3_RECORD: &str = concat!(
        r#"OCMF|{"FV":"1.0","GI":"eBZ LD3","GS":"1EBZ0300034628","GV":"V207","MS":"1EBZ0300034628","PG":"T120","IS":true,"IL":"TRUSTED","IT":"EMAID","ID":"DKV_Testbox2","CI":"Dies","CT":"EVSEID","#,
        r#""RD":[{"TM":"2022-10-27T19:38:50,000+0200 I","TX":"B","RV":2851485,"RI":"1-b:1.8.e","RU":"Wh","ST":"G"},"#,
        r#"{"TM":"2022-10-27T19:43:38,000+0200 I","TX":"E","RV":2851753,"RI":"1-b:1.8.e","RU":"Wh","ST":"G"}]}"#,
        r#"|{"SA":"ECDSA-secp192r1-SHA256","SD":"30340218e10a077929f593717affdff69a5df0d2862989a6638f873d0218007d21b1c0255c5b24a3c5a01d600839ebae2bb67bcb1159"}"#,
    );

    /// The public key that record is published with — a DER
    /// `SubjectPublicKeyInfo` carrying the `prime192v1` OID.
    const EBZ_LD3_KEY: &str = concat!(
        "3049301306072a8648ce3d020106082a8648ce3d03010103320004",
        "1e155ef46fbcc56005769c08d792127c006c242ccccd96bf",
        "7051b6fbc278497036659e7bae57f542776a17c7f8b28600",
    );

    #[test]
    fn a_real_secp192r1_meter_record_verifies() {
        let record = ocmf::parse(EBZ_LD3_RECORD).unwrap();
        assert_eq!(record.signature.algorithm, "ECDSA-secp192r1-SHA256");
        assert_eq!(
            record.payload.meter_serial.as_deref(),
            Some("1EBZ0300034628")
        );

        let key = PublicKey::from_hex(KeyType::Secp192r1, EBZ_LD3_KEY).unwrap();
        verify(&record, &key).expect("a real German meter's record must check out");
    }

    #[test]
    fn the_fixed_width_integers_real_meters_emit_are_accepted() {
        // The eBZ signature is a SEQUENCE of two 24-byte INTEGERs: `r` begins
        // `e1` with no sign padding, which strict DER reads as negative, and
        // `s` carries a `0x00` it does not need. Both are re-encoded minimally.
        let raw = hex::decode(concat!(
            "30340218e10a077929f593717affdff69a5df0d2862989a6638f873d",
            "0218007d21b1c0255c5b24a3c5a01d600839ebae2bb67bcb1159",
        ))
        .unwrap();
        let canonical = hex::encode(canonical_der(&raw).unwrap());
        assert_eq!(
            canonical,
            concat!(
                "3034021900e10a077929f593717affdff69a5df0d2862989a6638f873d",
                "02177d21b1c0255c5b24a3c5a01d600839ebae2bb67bcb1159",
            ),
            "r gains the sign octet DER demands, s loses the one it does not need"
        );

        // A signature that was canonical already comes back unchanged.
        let already = hex::decode("3006020101020102").unwrap();
        assert_eq!(canonical_der(&already).unwrap(), already);
    }

    #[test]
    fn a_signature_that_is_not_two_integers_is_refused() {
        // Leniency about integer padding is not leniency about structure.
        for junk in [
            "",                       // empty
            "3000",                   // an empty SEQUENCE
            "300602010102010203",     // trailing bytes
            "30060201010401ff",       // an OCTET STRING where an INTEGER belongs
            "3009020101020102020103", // three integers
            "02020101",               // a bare INTEGER, no SEQUENCE
        ] {
            assert!(
                canonical_der(&hex::decode(junk).unwrap_or_default()).is_err(),
                "{junk:?} must be refused"
            );
        }
    }

    #[test]
    fn a_changed_digit_in_the_real_record_is_caught() {
        // 2851753 Wh becomes 9851753 Wh: seven megawatt-hours conjured out of a
        // five-minute session, and the signature says so.
        let key = PublicKey::from_hex(KeyType::Secp192r1, EBZ_LD3_KEY).unwrap();
        let tampered = ocmf::parse(&EBZ_LD3_RECORD.replace("2851753", "9851753")).unwrap();
        assert!(matches!(
            verify(&tampered, &key),
            Err(VerifyError::SignatureMismatch)
        ));
    }

    /// A DZG DVH4013 behind a Nano gateway, from the same S.A.F.E. reference
    /// data set (`src/test/resources/xml/OCMF_Test_Data_00.xml`,
    /// © S.A.F.E. e.V., Apache-2.0).
    ///
    /// A third vendor, a third curve actually exercised, and two things no
    /// fixture this workspace writes would ever have: an `RV` quoted as a
    /// **string** and padded with spaces, and a signature whose `s` lies in the
    /// **high half of the curve order**.
    const DZG_RECORD: &str = concat!(
        r#"OCMF|{"FV" : "1.0","GI" : "Nano CH-10311C","GS" : "060643","GV" : "v017","PG" : "T198","MV" : "DZG","MM" : "DVH4013","MS" : "1DZG0033016824","IS" : true,"IL" : "VERIFIED","IF" : ["RFID_NONE","OCPP_NONE","ISO15118_NONE","PLMN_NONE"],"IT" : "EMAID","ID" : "04ab076a345b85","CT" : "CBIDC","CI" : "CI","#,
        r#""RD" : [{"TM" : "2021-10-26T10:20:52,000+0200 I","TX" : "B","RV" : "       9.038","RI" : "01-00:01.08.00.FF","RU" : "kWh","RT" : "AC","EF" : "","ST" : "G"}]}"#,
        r#"|{"SA" : "ECDSA-secp256k1-SHA256","SD" : "3046022100A4C188533ECA1793336520F7F99E010E62DEC32ABD344A562B00D396F65DFFE9022100CB0FB3782E406525641D689F4326D2118365A722EE75AAAB976C14B090BE49DA"}"#,
    );

    /// That record's published key: a DER `SubjectPublicKeyInfo` on secp256k1,
    /// distributed base64 rather than hex.
    const DZG_KEY: &str = concat!(
        "MFYwEAYHKoZIzj0CAQYFK4EEAAoDQgAEqHEykfqZhspgok6zCQh/329B38xine8ujzT8",
        "p5Nh7lek47cYeZj507aN6E4/QirF1b7Q57ln4VGfK6h0d0GOQA==",
    );

    #[test]
    fn a_high_s_signature_from_a_real_meter_verifies() {
        // For every ECDSA signature `(r, s)`, the pair `(r, n − s)` verifies the
        // same message under the same key — both are correct, and plain ECDSA
        // says nothing about which a signer emits. Bitcoin does, because
        // malleability matters to a consensus rule, and `k256` enforces the low
        // half inside `verify`.
        //
        // This meter has never heard of Bitcoin. `openssl dgst -verify` accepts
        // its signature and so does the reference verifier; this build called it
        // `SignatureMismatch`, which is the diagnostic for *tampering* — so
        // every session from such a meter was unbillable, and the reason on the
        // operator's queue pointed at fraud.
        let record = ocmf::parse(DZG_RECORD).unwrap();
        assert_eq!(record.signature.algorithm, "ECDSA-secp256k1-SHA256");

        let key = PublicKey::from_base64(KeyType::Secp256k1, DZG_KEY).unwrap();
        verify(&record, &key).expect("a real meter's high-s signature must check out");
    }

    #[test]
    fn a_quoted_reading_value_keeps_its_scale() {
        // `[OCMF Tab. 7]` types `RV` as a Number and this meter writes a string,
        // padded to a fixed width. Refusing it rejects hardware the reference
        // verifier accepts; parsing it through a float would throw away the
        // resolution claim the field exists to carry.
        let record = ocmf::parse(DZG_RECORD).unwrap();
        assert_eq!(
            record.payload.readings[0].value.unwrap().to_string(),
            "9.038",
            "three decimals, from a value written \"       9.038\""
        );
    }

    /// A `TwinCharger` Pro, from the same corpus
    /// (`src/test/resources/xml/chargepoint_3.xml`, © S.A.F.E. e.V.,
    /// Apache-2.0) — a fourth vendor, and four more things a self-written
    /// fixture does not do: `FV` and `CT` are JSON **numbers** where
    /// `[OCMF Tab. 1]` and `[OCMF Tab. 6]` say String, the signature is
    /// base64 rather than hex, the whole transaction is one data set, and the
    /// identification section sits *after* the readings.
    const TWINCHARGER_RECORD: &str = concat!(
        r#"OCMF|{"FV": 1.0, "GI": "TwinCharger Pro 2.1.0", "GS": "TC_00010", "GV": "0.5.15-1", "PG": "T2", "MV": "DZG", "MM": "DVH4013", "MS": "33019230", "CT": 0, "CI": "DE*EBW*EP006120*1", "UCPN": "101", "#,
        r#""RD": [{"TM": "2021-04-09T09:09:51,000+0200 I", "TX": "B", "RV": "0.0", "RI": "1-0:1.8.0", "RU": "kWh", "RT": "AC", "EF": "", "ST": "G"}, {"TM": "2021-04-09T09:21:30,000+0200 I", "TX": "E", "RV": "1.304", "RI": "1-0:1.8.0", "RU": "kWh", "RT": "AC", "EF": "", "ST": "G"}], "#,
        r#""IS": true, "IL": "HEARSAY", "IT": "ISO14443", "ID": "ad228beb"}"#,
        r#"|{"SD":"MEUCIQCW6ui2zIeCPLfElYuJoT0HJBmx7JTauGrWEAb+5l3LmAIgN4Q9vTm86z0rRJFF3p+gHnXp7YmRbJiuUrp61a+vHB4=","SA":"ECDSA-secp256r1-SHA256","SE":"base64"}"#,
    );

    /// Its published key, distributed as hex under `encoding="plain"`.
    const TWINCHARGER_KEY: &str = concat!(
        "3059301306072a8648ce3d020106082a8648ce3d030107034200042c2646046965",
        "328e8db4700e78c19816f91a9a389f61aa9c5b3b87dae6cba7a277afafb16390003",
        "1a1b507291f1c3d8d951bfd48b1edab4b406551a9c26ad12a",
    );

    #[test]
    fn a_fourth_vendors_record_verifies_and_bills() {
        let record = ocmf::parse(TWINCHARGER_RECORD).unwrap();
        let key = PublicKey::from_hex(KeyType::Secp256r1, TWINCHARGER_KEY).unwrap();
        verify(&record, &key).expect("a real TwinCharger record must check out");

        // The typed view survives the vendor's liberties with the schema.
        assert_eq!(record.payload.format_version.as_deref(), Some("1.0"));
        assert_eq!(record.payload.charge_point_id_type.as_deref(), Some("0"));
        assert_eq!(record.payload.readings.len(), 2);

        // …and the chain reads it as one whole transaction: 1.304 kWh on an
        // ordinary IEC register that states its own direction.
        let report = crate::chain::validate(&[record]);
        assert_eq!(report.billable_energy.unwrap().to_string(), "1.304 kWh");
        assert_eq!(report.direction, Some(emob_core::Direction::Import));

        // The only thing standing against it is the clock: `TM` ends in `I`, so
        // the duration is informative and a per-minute fee may not touch it —
        // the same split this crate exists to keep, met in the field rather
        // than in a fixture.
        assert!(
            report
                .findings
                .iter()
                .all(|f| matches!(f, crate::chain::ChainFinding::ClockNotBillable { .. })),
            "{:?}",
            report.findings
        );
        assert!(!report.is_billable_for_time());
    }

    #[test]
    fn normalising_s_does_not_make_a_forgery_verify() {
        // The property that makes the normalisation safe rather than lenient:
        // it accepts exactly what plain ECDSA accepts. A tampered payload still
        // fails, in whichever half of the order its `s` happens to sit.
        let key = PublicKey::from_base64(KeyType::Secp256k1, DZG_KEY).unwrap();
        let tampered = ocmf::parse(&DZG_RECORD.replace("9.038", "9.938")).unwrap();
        assert!(matches!(
            verify(&tampered, &key),
            Err(VerifyError::SignatureMismatch)
        ));

        // …and so does a signature whose `s` was replaced with an unrelated
        // value in the low half, which normalisation leaves untouched.
        let forged = ocmf::parse(&DZG_RECORD.replace(
            "CB0FB3782E406525641D689F4326D2118365A722EE75AAAB976C14B090BE49DA",
            "0B0FB3782E406525641D689F4326D2118365A722EE75AAAB976C14B090BE49DA",
        ))
        .unwrap();
        assert!(matches!(
            verify(&forged, &key),
            Err(VerifyError::SignatureMismatch)
        ));
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
        for spelling in ["ECDSA-secp192r1", "secp192r1", "prime192v1", "P-192"] {
            assert_eq!(KeyType::parse(spelling).unwrap(), KeyType::Secp192r1);
        }
    }

    #[test]
    fn every_key_type_pairs_with_a_supported_algorithm() {
        // The property that keeps the registry and the verifier in step: a key
        // this crate can hold is a key it can actually check a signature with.
        for key_type in [
            KeyType::Secp256r1,
            KeyType::Secp384r1,
            KeyType::Secp256k1,
            KeyType::Secp192r1,
        ] {
            assert!(
                key_type.signature_algorithm().is_supported(),
                "{key_type:?} is registrable but unverifiable"
            );
        }
    }
}
