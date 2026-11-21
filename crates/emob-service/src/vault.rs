//! Data at rest: one key per daemon, a keyed digest and a sealed box.
//!
//! # Why a journal needs this
//!
//! A journal is append-only and hash-chained, so it is the one file a daemon
//! can never rewrite — and the file most likely to leave the building, in a
//! backup or a support bundle. A credential written into it is a credential in
//! every copy for ever; a card UID written into it can never be erased
//! (GDPR Article 17). So a daemon writes neither: what it needs only to
//! *recognise* (a token a partner presents) is journalled as a [`Digest`], and
//! what it needs to *use* again (the token it presents to a partner, a UID a
//! CDR must carry) as a [`Sealed`] box.
//!
//! # One key, two purposes, and a name
//!
//! Each daemon reads one 32-byte [`DataKey`] from `<DAEMON>_DATA_KEY`
//! (standard base64; `just dev-keys` prints a set). Two subkeys are derived
//! from it — one for HMAC-SHA256, one for AES-256-GCM — so the same bytes are
//! never a MAC key and a cipher key at once. Every digest and every box carries
//! the [`KeyId`] it was made under, so a record under another key is told apart
//! from a forgery: for `empd` that is crypto-erasure (the key was destroyed on
//! purpose), for `roamd` and `csmsd` a refusal to start.
//!
//! Each call names a **purpose** (`"roamd/token-b"`, `"empd/uid"`): it is mixed
//! into the digest and is the AEAD's associated data, so a box sealed for one
//! field cannot be replayed into another.

use core::fmt;

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use base64::Engine as _;
use hmac::{Hmac, KeyInit as _, Mac};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::estate::Env;

type HmacSha256 = Hmac<Sha256>;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Why a key could not be read, or a value could not be opened.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The variable is not set.
    #[error(
        "{variable} is not set: this daemon keeps credentials and card UIDs out of its journal \
         under a data key, and does not run without one (32 random bytes, base64 — `just \
         dev-keys` prints a set)"
    )]
    Unset {
        /// Which variable.
        variable: String,
    },
    /// The variable holds something that is not a 32-byte key.
    #[error("{variable} is not a data key: {reason} (32 random bytes, base64)")]
    Invalid {
        /// Which variable.
        variable: String,
        /// Why.
        reason: String,
    },
    /// The value was made under a key this daemon does not hold.
    #[error("the record was sealed under data key {found}, and this daemon holds {held}")]
    UnknownKey {
        /// The key id the record names.
        found: KeyId,
        /// The key id this daemon holds.
        held: KeyId,
    },
    /// The box does not open: altered, truncated, or sealed for another purpose.
    #[error("a sealed {purpose} does not open: it was altered or sealed for something else")]
    Corrupt {
        /// What it claimed to be.
        purpose: String,
    },
}

/// A data key's name: the first 8 hex digits of SHA-256 over the key.
///
/// Public by design — it names a key without revealing anything about it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyId(String);

impl KeyId {
    /// The id, as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for KeyId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        if value.len() == 8
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(value))
        } else {
            Err(format!(
                "{value:?} is not a key id (8 lowercase hex digits)"
            ))
        }
    }
}

impl From<KeyId> for String {
    fn from(id: KeyId) -> Self {
        id.0
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({})", self.0)
    }
}

/// A keyed digest: HMAC-SHA256 under the digest subkey, with the key's id.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "RawDigest")]
pub struct Digest {
    key: KeyId,
    mac: String,
}

#[derive(serde::Deserialize)]
struct RawDigest {
    key: KeyId,
    mac: String,
}

impl TryFrom<RawDigest> for Digest {
    type Error = String;
    fn try_from(raw: RawDigest) -> Result<Self, String> {
        if raw.mac.len() == 64
            && raw
                .mac
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self {
                key: raw.key,
                mac: raw.mac,
            })
        } else {
            Err("a digest is 64 lowercase hex digits".to_owned())
        }
    }
}

impl Digest {
    /// The key it was made under.
    #[must_use]
    pub const fn key(&self) -> &KeyId {
        &self.key
    }

    /// The MAC, as 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.mac
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({}:{}…)", self.key, &self.mac[..8])
    }
}

/// A value sealed with AES-256-GCM under the seal subkey, with the key's id.
///
/// `data` is base64 of `nonce ‖ ciphertext ‖ tag`; it is checked when opened.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sealed {
    key: KeyId,
    data: String,
}

impl Sealed {
    /// The key it was sealed under.
    #[must_use]
    pub const fn key(&self) -> &KeyId {
        &self.key
    }
}

impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sealed({}, …)", self.key)
    }
}

/// One daemon's data key.
pub struct DataKey {
    id: KeyId,
    digest: [u8; 32],
    seal: LessSafeKey,
}

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DataKey({})", self.id)
    }
}

fn subkey(master: &[u8], label: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(master)
        .unwrap_or_else(|_| unreachable!("HMAC accepts a key of any length"));
    mac.update(label.as_bytes());
    mac.finalize().into_bytes().into()
}

impl DataKey {
    /// The key in `variable`, looked up in `env`.
    ///
    /// # Errors
    ///
    /// [`VaultError::Unset`] or [`VaultError::Invalid`], naming the variable.
    pub fn from_env(variable: &str, env: &Env) -> Result<Self, VaultError> {
        let text = env(variable)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| VaultError::Unset {
                variable: variable.to_owned(),
            })?;
        Self::from_base64(&text).map_err(|reason| VaultError::Invalid {
            variable: variable.to_owned(),
            reason,
        })
    }

    /// A key from its base64 text.
    ///
    /// # Errors
    ///
    /// Why it is not 32 bytes of base64.
    pub fn from_base64(text: &str) -> Result<Self, String> {
        let bytes = B64
            .decode(text.trim())
            .map_err(|error| format!("not base64 ({error})"))?;
        let master: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("{} bytes, not 32", bytes.len()))?;
        Ok(Self::from_bytes(&master))
    }

    /// A key from its 32 bytes.
    #[must_use]
    pub fn from_bytes(master: &[u8; 32]) -> Self {
        let id = KeyId(hex::encode(&Sha256::digest(master)[..4]));
        let seal = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &subkey(master, "emob/seal/v1"))
                .unwrap_or_else(|_| unreachable!("a 32-byte key is an AES-256 key")),
        );
        Self {
            id,
            digest: subkey(master, "emob/digest/v1"),
            seal,
        }
    }

    /// A fresh random key, as base64 — what `just dev-keys` and tests use.
    #[must_use]
    pub fn generate() -> String {
        let mut bytes = [0_u8; 32];
        aws_lc_rs::rand::fill(&mut bytes)
            .unwrap_or_else(|_| unreachable!("the system RNG does not fail"));
        B64.encode(bytes)
    }

    /// Its id.
    #[must_use]
    pub const fn id(&self) -> &KeyId {
        &self.id
    }

    fn mac(&self, purpose: &str, value: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(&self.digest)
            .unwrap_or_else(|_| unreachable!("HMAC accepts a key of any length"));
        mac.update(purpose.as_bytes());
        mac.update(&[0]);
        mac.update(value);
        mac.finalize().into_bytes().into()
    }

    /// The keyed digest of `value` for `purpose`.
    #[must_use]
    pub fn digest(&self, purpose: &str, value: &[u8]) -> Digest {
        Digest {
            key: self.id.clone(),
            mac: hex::encode(self.mac(purpose, value)),
        }
    }

    /// Whether `digest` is this key's digest of `value` for `purpose`,
    /// compared in constant time. A digest under another key never matches.
    #[must_use]
    pub fn matches(&self, digest: &Digest, purpose: &str, value: &[u8]) -> bool {
        digest.key == self.id
            && bool::from(
                hex::encode(self.mac(purpose, value))
                    .as_bytes()
                    .ct_eq(digest.mac.as_bytes()),
            )
    }

    /// Seal `plaintext` for `purpose`.
    #[must_use]
    pub fn seal(&self, purpose: &str, plaintext: &[u8]) -> Sealed {
        let mut nonce = [0_u8; NONCE_LEN];
        aws_lc_rs::rand::fill(&mut nonce)
            .unwrap_or_else(|_| unreachable!("the system RNG does not fail"));
        let mut in_out = plaintext.to_vec();
        self.seal
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(purpose.as_bytes()),
                &mut in_out,
            )
            .unwrap_or_else(|_| unreachable!("AES-GCM seals any length a journal holds"));
        let mut data = nonce.to_vec();
        data.extend_from_slice(&in_out);
        Sealed {
            key: self.id.clone(),
            data: B64.encode(data),
        }
    }

    /// Seal a string.
    #[must_use]
    pub fn seal_str(&self, purpose: &str, plaintext: &str) -> Sealed {
        self.seal(purpose, plaintext.as_bytes())
    }

    /// Open a box sealed for `purpose`.
    ///
    /// # Errors
    ///
    /// [`VaultError::UnknownKey`] for a box under another key, and
    /// [`VaultError::Corrupt`] for one that does not authenticate.
    pub fn open(&self, purpose: &str, sealed: &Sealed) -> Result<Vec<u8>, VaultError> {
        if sealed.key != self.id {
            return Err(VaultError::UnknownKey {
                found: sealed.key.clone(),
                held: self.id.clone(),
            });
        }
        let corrupt = || VaultError::Corrupt {
            purpose: purpose.to_owned(),
        };
        let data = B64.decode(&sealed.data).map_err(|_| corrupt())?;
        if data.len() < NONCE_LEN {
            return Err(corrupt());
        }
        let (nonce, body) = data.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| corrupt())?;
        let mut body = body.to_vec();
        let plain = self
            .seal
            .open_in_place(nonce, Aad::from(purpose.as_bytes()), &mut body)
            .map_err(|_| corrupt())?;
        Ok(plain.to_vec())
    }

    /// Open a box holding a string.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], and [`VaultError::Corrupt`] for bytes that are not UTF-8.
    pub fn open_str(&self, purpose: &str, sealed: &Sealed) -> Result<String, VaultError> {
        String::from_utf8(self.open(purpose, sealed)?).map_err(|_| VaultError::Corrupt {
            purpose: purpose.to_owned(),
        })
    }
}

/// A journal whose record bodies are sealed under a [`DataKey`].
///
/// For a daemon whose facts carry personal data in many places — `csmsd`'s
/// authorisation answers, CDRs and delivery bodies all hold the card's UID —
/// sealing each field is a list somebody forgets to extend. So the whole body
/// is sealed (the AEAD's associated data is `purpose` and the record's kind),
/// and the chain is kept over the **sealed** bytes: it verifies without the
/// key, and an edited record still breaks it. The kind stays readable, so the
/// file still says what happened, just not to whom.
pub struct Sealing<J> {
    inner: J,
    key: std::sync::Arc<DataKey>,
    purpose: &'static str,
}

impl<J> Sealing<J> {
    /// `inner`, with every body sealed under `key` for `purpose`.
    #[must_use]
    pub const fn new(inner: J, key: std::sync::Arc<DataKey>, purpose: &'static str) -> Self {
        Self {
            inner,
            key,
            purpose,
        }
    }

    /// The journal underneath, as it is on disk.
    #[must_use]
    pub const fn inner(&self) -> &J {
        &self.inner
    }

    fn aad(&self, kind: &str) -> String {
        format!("{}/{kind}", self.purpose)
    }
}

impl<J: crate::journal::Journal> crate::journal::Journal for Sealing<J> {
    fn append(
        &self,
        kind: &str,
        actor: &crate::journal::Actor,
        body: &str,
    ) -> Result<crate::journal::Record, crate::journal::JournalError> {
        let sealed = self.key.seal_str(&self.aad(kind), body);
        let text = serde_json::to_string(&sealed).map_err(|source| {
            crate::journal::JournalError::Encoding {
                doing: "sealed",
                source,
            }
        })?;
        let mut record = self.inner.append(kind, actor, &text)?;
        body.clone_into(&mut record.body);
        Ok(record)
    }

    fn records(&self) -> Result<Vec<crate::journal::Record>, crate::journal::JournalError> {
        let mut records = self.inner.records()?;
        for record in &mut records {
            let refused = |reason: String| crate::journal::JournalError::Sealed {
                seq: record.seq,
                reason,
            };
            let sealed: Sealed = serde_json::from_str(&record.body)
                .map_err(|error| refused(format!("not a sealed record ({error})")))?;
            let body = self
                .key
                .open_str(&self.aad(&record.kind), &sealed)
                .map_err(|error| refused(error.to_string()))?;
            record.body = body;
        }
        Ok(records)
    }

    fn head(&self) -> Result<crate::journal::Head, crate::journal::JournalError> {
        self.inner.head()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> DataKey {
        DataKey::from_bytes(&[7; 32])
    }

    #[test]
    fn sr_044_a_sealed_value_opens_only_under_its_key_and_purpose() {
        let key = key();
        let sealed = key.seal_str("roamd/token-b", "token-b-from-the-partner");
        let json = serde_json::to_string(&sealed).unwrap();
        assert!(!json.contains("token-b-from-the-partner"), "{json}");
        let back: Sealed = serde_json::from_str(&json).unwrap();
        assert_eq!(
            key.open_str("roamd/token-b", &back).unwrap(),
            "token-b-from-the-partner"
        );
        assert!(matches!(
            key.open("empd/uid", &back),
            Err(VaultError::Corrupt { .. })
        ));
        let other = DataKey::from_bytes(&[8; 32]);
        assert!(matches!(
            other.open("roamd/token-b", &back),
            Err(VaultError::UnknownKey { .. })
        ));
        // Two seals of one value differ: the nonce is fresh.
        assert_ne!(key.seal_str("p", "v"), key.seal_str("p", "v"));
    }

    #[test]
    fn sr_044_a_digest_matches_its_value_and_nothing_under_another_key() {
        let key = key();
        let digest = key.digest("roamd/token-c", b"token-c");
        assert!(key.matches(&digest, "roamd/token-c", b"token-c"));
        assert!(!key.matches(&digest, "roamd/token-c", b"token-d"));
        assert!(!key.matches(&digest, "roamd/token-a", b"token-c"));
        assert!(!DataKey::from_bytes(&[8; 32]).matches(&digest, "roamd/token-c", b"token-c"));
        assert_eq!(key.id().as_str().len(), 8);
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), digest);
        assert!(serde_json::from_str::<Digest>(r#"{"key":"00000000","mac":"token-c"}"#).is_err());
    }

    #[test]
    fn sr_048_a_sealed_journal_keeps_its_chain_and_never_its_plaintext() {
        use crate::journal::{Journal as _, Memory};
        let key = std::sync::Arc::new(key());
        let journal = Sealing::new(Memory::new(), std::sync::Arc::clone(&key), "csmsd/journal");
        let station = crate::journal::Actor::station("CP-1").unwrap();
        journal
            .append("authorized", &station, r#"{"uid":"04A1B2C3D4E5F6"}"#)
            .unwrap();
        let on_disk = journal.inner().records().unwrap();
        assert!(!on_disk[0].body.contains("04A1B2C3D4E5F6"), "{on_disk:?}");
        assert_eq!(on_disk[0].kind, "authorized", "the kind stays readable");
        assert!(
            journal.records().unwrap()[0]
                .body
                .contains("04A1B2C3D4E5F6")
        );

        let other = Sealing::new(
            Memory::new(),
            std::sync::Arc::new(DataKey::from_bytes(&[9; 32])),
            "csmsd/journal",
        );
        other
            .inner()
            .append("authorized", &station, &on_disk[0].body)
            .unwrap();
        let error = other.records().unwrap_err();
        assert!(error.to_string().contains("does not open"), "{error}");
    }

    #[test]
    fn sr_044_a_daemon_without_a_32_byte_key_refuses_naming_the_variable() {
        let unset: Env = std::sync::Arc::new(|_| None);
        let error = DataKey::from_env("ROAMD_DATA_KEY", &unset).unwrap_err();
        assert!(error.to_string().contains("ROAMD_DATA_KEY"), "{error}");
        let short: Env = std::sync::Arc::new(|_| Some("c2hvcnQ=".to_owned()));
        let error = DataKey::from_env("ROAMD_DATA_KEY", &short).unwrap_err();
        assert!(error.to_string().contains("5 bytes, not 32"), "{error}");
        let good = DataKey::generate();
        let env: Env = std::sync::Arc::new(move |_| Some(good.clone()));
        assert!(DataKey::from_env("ROAMD_DATA_KEY", &env).is_ok());
    }
}
