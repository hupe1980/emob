//! The signed journal head: what closes the end of the chain.
//!
//! # Why a journal signs where it ends
//!
//! [`crate::journal`] proves nothing was rewritten and nothing removed from
//! the middle, and cannot prove nothing was removed from the end: a journal
//! with its last record cut off is the file a process killed mid-append
//! leaves. So after **every** append, [`Signing`] signs the head — position
//! and digest — with the daemon's Ed25519 key, keeps it beside the journal in
//! `<journal>.head`, and serves it at `GET /v1/<name>/head` for a
//! [`crate::witness`]. At start it refuses a journal shorter than the head it
//! signed, one whose record at that position is not the one it signed, and one
//! that runs past it by more than the single record a crash between the append
//! and the signature can leave.
//!
//! # The format is a C2SP signed note
//!
//! A [checkpoint]-shaped note — origin, size, base64 hash — with one signature
//! line, readable with `curl` and checkable by any signed-note verifier:
//!
//! ```text
//! emob/csmsd
//! 57
//! q1Iot…=
//!
//! — emob/csmsd GrH0…==
//! ```
//!
//! The verifier key is the note format's own: `<name>+<key hash>+<base64(0x01
//! ‖ public key)>`. Ed25519 is `aws-lc-rs`'s, which rustls already links — not
//! a second `RustCrypto` generation (D304).
//!
//! [checkpoint]: https://c2sp.org/tlog-checkpoint

use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use crate::estate::Env;
use crate::journal::{Actor, Head, Journal, JournalError, Record};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// The signature algorithm byte the note format assigns Ed25519.
const ED25519_ALG: u8 = 0x01;

/// The em dash that opens a signature line.
const DASH: &str = "\u{2014} ";

/// Why a key or a note was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NoteError {
    /// The signing-key variable is not set.
    #[error(
        "{variable} is not set: this daemon signs where its journal ends, so a truncated one is \
         caught, and does not run without a key (32 random bytes, base64 — `just dev-keys` \
         prints a set)"
    )]
    Unset {
        /// Which variable.
        variable: String,
    },
    /// The variable is not a 32-byte seed.
    #[error("{variable} is not a signing key: {reason} (32 random bytes, base64)")]
    Invalid {
        /// Which variable.
        variable: String,
        /// Why.
        reason: String,
    },
    /// A verifier key does not parse.
    #[error("{0:?} is not a verifier key (`<name>+<8 hex>+<base64>`)")]
    VerifierKey(String),
    /// A note does not parse.
    #[error("not a signed head: {0}")]
    Malformed(&'static str),
    /// No signature line by this key, or the signature does not verify.
    #[error("the head is not signed by {0}")]
    NotSigned(String),
}

/// The variable a daemon's signing key is read from: `<DAEMON>_SIGNING_KEY`.
#[must_use]
pub fn variable(daemon: &str) -> String {
    format!("{}_SIGNING_KEY", daemon.to_ascii_uppercase())
}

/// A daemon's head-signing key.
pub struct SigningKey {
    name: String,
    pair: Ed25519KeyPair,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningKey({})", self.name)
    }
}

impl SigningKey {
    /// The key for `daemon`, from the variable [`variable`] names, in `env`.
    ///
    /// # Errors
    ///
    /// As [`Self::from_env`].
    pub fn for_daemon(daemon: &str, env: &Env) -> Result<Self, NoteError> {
        Self::from_env(&variable(daemon), daemon, env)
    }

    /// The key in `variable`, for the daemon `daemon` (origin `emob/<daemon>`).
    ///
    /// # Errors
    ///
    /// [`NoteError::Unset`] or [`NoteError::Invalid`], naming the variable.
    pub fn from_env(variable: &str, daemon: &str, env: &Env) -> Result<Self, NoteError> {
        let text = env(variable)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| NoteError::Unset {
                variable: variable.to_owned(),
            })?;
        let invalid = |reason: String| NoteError::Invalid {
            variable: variable.to_owned(),
            reason,
        };
        let bytes = B64
            .decode(text.trim())
            .map_err(|error| invalid(format!("not base64 ({error})")))?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| invalid(format!("{} bytes, not 32", bytes.len())))?;
        Ok(Self::from_seed(&seed, daemon))
    }

    /// A key from its 32-byte seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; 32], daemon: &str) -> Self {
        Self {
            name: format!("emob/{daemon}"),
            pair: Ed25519KeyPair::from_seed_unchecked(seed)
                .unwrap_or_else(|_| unreachable!("any 32 bytes are an Ed25519 seed")),
        }
    }

    /// A fresh random seed, as base64 — what `just dev-keys` and tests use.
    #[must_use]
    pub fn generate() -> String {
        crate::vault::DataKey::generate()
    }

    /// The note name and origin: `emob/<daemon>`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The public half, as the note format writes it.
    #[must_use]
    pub fn verifier(&self) -> VerifierKey {
        let public: [u8; 32] = self
            .pair
            .public_key()
            .as_ref()
            .try_into()
            .unwrap_or_else(|_| unreachable!("an Ed25519 public key is 32 bytes"));
        VerifierKey::new(&self.name, public)
    }

    /// Sign a head as a note.
    #[must_use]
    pub fn sign(&self, head: &Head) -> String {
        let body = body(&self.name, head);
        let signature = self.pair.sign(body.as_bytes());
        let mut blob = self.verifier().id.to_vec();
        blob.extend_from_slice(signature.as_ref());
        format!("{body}\n{DASH}{} {}\n", self.name, B64.encode(blob))
    }
}

fn body(origin: &str, head: &Head) -> String {
    let hash = hex::decode(&head.hash).unwrap_or_default();
    format!("{origin}\n{}\n{}\n", head.position, B64.encode(hash))
}

/// A verifier key: `<name>+<hex key hash>+<base64(0x01 ‖ public key)>`.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifierKey {
    name: String,
    id: [u8; 4],
    public: [u8; 32],
}

impl VerifierKey {
    fn new(name: &str, public: [u8; 32]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update(b"\n");
        hasher.update([ED25519_ALG]);
        hasher.update(public);
        let digest = hasher.finalize();
        Self {
            name: name.to_owned(),
            id: [digest[0], digest[1], digest[2], digest[3]],
            public,
        }
    }

    /// The name it signs under.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Verify a note and read the head it signs.
    ///
    /// # Errors
    ///
    /// [`NoteError::Malformed`] for text that is not a head note, and
    /// [`NoteError::NotSigned`] when no signature line by this key verifies.
    pub fn open(&self, note: &str) -> Result<Head, NoteError> {
        let (text, signatures) = note
            .split_once("\n\n")
            .ok_or(NoteError::Malformed("no blank line before the signatures"))?;
        let text = format!("{text}\n");
        let signed = signatures.lines().any(|line| {
            let Some(rest) = line.strip_prefix(DASH) else {
                return false;
            };
            let Some((name, blob)) = rest.split_once(' ') else {
                return false;
            };
            let Ok(blob) = B64.decode(blob) else {
                return false;
            };
            name == self.name
                && blob.len() == 4 + 64
                && blob[..4] == self.id
                && UnparsedPublicKey::new(&ED25519, self.public)
                    .verify(text.as_bytes(), &blob[4..])
                    .is_ok()
        });
        if !signed {
            return Err(NoteError::NotSigned(self.to_string()));
        }
        let mut lines = text.lines();
        let origin = lines.next().ok_or(NoteError::Malformed("no origin"))?;
        if origin != self.name {
            return Err(NoteError::Malformed("the origin is not the key's name"));
        }
        let position = lines
            .next()
            .and_then(|line| line.parse::<u64>().ok())
            .ok_or(NoteError::Malformed("no position"))?;
        let hash = lines
            .next()
            .and_then(|line| B64.decode(line).ok())
            .filter(|bytes| bytes.len() == 32)
            .ok_or(NoteError::Malformed("no 32-byte hash"))?;
        Ok(Head {
            position,
            hash: hex::encode(hash),
        })
    }
}

impl fmt::Display for VerifierKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut key = vec![ED25519_ALG];
        key.extend_from_slice(&self.public);
        write!(
            f,
            "{}+{}+{}",
            self.name,
            hex::encode(self.id),
            B64.encode(key)
        )
    }
}

impl fmt::Debug for VerifierKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VerifierKey({self})")
    }
}

impl core::str::FromStr for VerifierKey {
    type Err = NoteError;
    fn from_str(text: &str) -> Result<Self, NoteError> {
        let refused = || NoteError::VerifierKey(text.chars().take(120).collect());
        let mut parts = text.trim().splitn(3, '+');
        let (Some(name), Some(id), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(refused());
        };
        let key = B64.decode(key).map_err(|_| refused())?;
        let (Some(&ED25519_ALG), Ok(public)) = (key.first(), <[u8; 32]>::try_from(&key[1..]))
        else {
            return Err(refused());
        };
        let parsed = Self::new(name, public);
        if hex::encode(parsed.id) != id || name.is_empty() || name.contains(char::is_whitespace) {
            return Err(refused());
        }
        Ok(parsed)
    }
}

/// The last note a daemon signed, shared with the route that serves it.
#[derive(Clone)]
pub struct Latest {
    note: Arc<RwLock<String>>,
    verifier: VerifierKey,
}

impl fmt::Debug for Latest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Latest({})", self.verifier)
    }
}

impl Latest {
    /// The note.
    #[must_use]
    pub fn note(&self) -> String {
        self.note
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The key it verifies under.
    #[must_use]
    pub const fn verifier(&self) -> &VerifierKey {
        &self.verifier
    }
}

/// Where a journal's signed head is kept: beside it, as `<journal>.head`.
#[must_use]
pub fn beside(journal: &Path) -> PathBuf {
    let mut name = journal.file_name().unwrap_or_default().to_os_string();
    name.push(".head");
    journal.with_file_name(name)
}

/// A journal that signs where it ends after every append, and will not open
/// from a history shorter than one it signed.
pub struct Signing<J> {
    inner: J,
    key: SigningKey,
    path: PathBuf,
    latest: Latest,
}

impl<J> fmt::Debug for Signing<J> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signing")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl<J: Journal> Signing<J> {
    /// Open `inner` against the head signed at `path` (see [`beside`]).
    ///
    /// # Errors
    ///
    /// [`JournalError::Truncated`], [`JournalError::Diverged`],
    /// [`JournalError::Unsigned`] and [`JournalError::HeadUnreadable`] — each
    /// a reason not to start — and whatever reading `inner` could not do.
    pub fn open(inner: J, key: SigningKey, path: PathBuf) -> Result<Self, JournalError> {
        let records = inner.records()?;
        let count = u64::try_from(records.len()).unwrap_or(u64::MAX);
        let signed =
            match std::fs::read_to_string(&path) {
                Ok(note) => Some(key.verifier().open(&note).map_err(|error| {
                    JournalError::HeadUnreadable {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(JournalError::Io {
                        path,
                        doing: "read",
                        source,
                    });
                }
            };
        let signed = signed.unwrap_or_else(Head::genesis);
        if count < signed.position {
            return Err(JournalError::Truncated {
                journal: count,
                signed: signed.position,
                missing: signed.position - count,
            });
        }
        if signed.position > 0 {
            let at = usize::try_from(signed.position - 1).unwrap_or(usize::MAX);
            if records
                .get(at)
                .is_none_or(|record| record.hash != signed.hash)
            {
                return Err(JournalError::Diverged {
                    position: signed.position,
                });
            }
        }
        // One record past the head is the crash between an append and its
        // signature; more is somebody else's writing, or a removed head.
        if count > signed.position.saturating_add(1) {
            return Err(JournalError::Unsigned {
                journal: count,
                signed: signed.position,
            });
        }
        let head = records.last().map_or_else(Head::genesis, |record| Head {
            position: record.seq,
            hash: record.hash.clone(),
        });
        let latest = Latest {
            note: Arc::new(RwLock::new(String::new())),
            verifier: key.verifier(),
        };
        let signing = Self {
            inner,
            key,
            path,
            latest,
        };
        signing.sign(&head)?;
        Ok(signing)
    }

    /// The last signed note, for [`router`].
    #[must_use]
    pub fn latest(&self) -> Latest {
        self.latest.clone()
    }

    /// The journal underneath.
    #[must_use]
    pub const fn inner(&self) -> &J {
        &self.inner
    }

    fn sign(&self, head: &Head) -> Result<(), JournalError> {
        let note = self.key.sign(head);
        let io = |doing, source| JournalError::Io {
            path: self.path.clone(),
            doing,
            source,
        };
        // Written aside and renamed, so a crash leaves the old head or the
        // new one, never half of either.
        let partial = self.path.with_extension("head.partial");
        std::fs::write(&partial, &note).map_err(|source| io("written", source))?;
        std::fs::rename(&partial, &self.path).map_err(|source| io("replaced", source))?;
        *self
            .latest
            .note
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = note;
        Ok(())
    }
}

impl<J: Journal> Journal for Signing<J> {
    fn append(&self, kind: &str, actor: &Actor, body: &str) -> Result<Record, JournalError> {
        let record = self.inner.append(kind, actor, body)?;
        self.sign(&Head {
            position: record.seq,
            hash: record.hash.clone(),
        })?;
        Ok(record)
    }

    fn records(&self) -> Result<Vec<Record>, JournalError> {
        self.inner.records()
    }

    fn head(&self) -> Result<Head, JournalError> {
        self.inner.head()
    }
}

/// The response header carrying the verifier key.
pub const VERIFIER_HEADER: &str = "emob-verifier-key";

/// `GET /v1/<daemon>/head`: the last signed note, `text/plain`, with its
/// verifier key in [`VERIFIER_HEADER`]. Mount it inside
/// [`crate::serve::operator_only`] like every read model (SR-103).
pub fn router(daemon: &str, latest: Latest) -> axum::Router {
    axum::Router::new().route(
        &format!("/v1/{daemon}/head"),
        axum::routing::get(move || {
            let latest = latest.clone();
            async move {
                (
                    [
                        (
                            axum::http::header::CONTENT_TYPE,
                            "text/plain; charset=utf-8".to_owned(),
                        ),
                        (
                            axum::http::HeaderName::from_static(VERIFIER_HEADER),
                            latest.verifier().to_string(),
                        ),
                    ],
                    latest.note(),
                )
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{File, Memory};

    fn key() -> SigningKey {
        SigningKey::from_seed(&[3; 32], "testd")
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emob-head-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("testd.ndjson")
    }

    /// The signed-note example `golang.org/x/mod/sumdb/note` documents: a
    /// note and keys somebody else wrote, so the format is measured against
    /// its own reference rather than against this module's signer.
    #[test]
    fn sr_097_the_note_format_is_the_reference_ones() {
        let seed = B64
            .decode("AYEKFALVFGyNhPJEMzD1QIDr+Y7hfZx09iUvxdXHKDFz")
            .unwrap();
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed[1..]).unwrap();
        let public: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
        let verifier = VerifierKey::new("PeterNeumann", public);
        assert_eq!(
            verifier.to_string(),
            "PeterNeumann+c74f20a3+ARpc2QcUPDhMQegwxbzhKqiBfsVkmqq/LDE4izWy10TW"
        );
        let text = "If you think cryptography is the answer to your problem,\n\
                    then you don't know what your problem is.\n";
        let blob = {
            let mut blob = verifier.id.to_vec();
            blob.extend_from_slice(pair.sign(text.as_bytes()).as_ref());
            B64.encode(blob)
        };
        assert_eq!(
            blob,
            "x08go/ZJkuBS9UG/SffcvIAQxVBtiFupLLr8pAcElZInNIuGUgYN1FFYC2pZSNXgKvqfqdngotpRZb6KE6RyyBwJnAM="
        );
        let parsed: VerifierKey = verifier.to_string().parse().unwrap();
        assert_eq!(parsed, verifier);
    }

    #[test]
    fn sr_097_a_head_note_round_trips_and_only_its_key_opens_it() {
        let head = Head {
            position: 57,
            hash: "ab".repeat(32),
        };
        let note = key().sign(&head);
        assert!(note.starts_with("emob/testd\n57\n"), "{note}");
        assert!(note.contains("\n\n\u{2014} emob/testd "), "{note}");
        assert_eq!(key().verifier().open(&note).unwrap(), head);

        let other = SigningKey::from_seed(&[4; 32], "testd").verifier();
        assert!(matches!(other.open(&note), Err(NoteError::NotSigned(_))));
        let tampered = note.replacen("\n57\n", "\n58\n", 1);
        assert!(key().verifier().open(&tampered).is_err());
    }

    #[test]
    fn sr_096_a_journal_shorter_than_its_signed_head_does_not_open() {
        let path = scratch("short");
        {
            let journal = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap();
            for _ in 0..3 {
                journal.append("added", &Actor::Timer, "{}").unwrap();
            }
            assert!(journal.latest().note().contains("\n3\n"));
        }
        // The last record is cut off.
        let text = std::fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = text.lines().take(2).collect();
        std::fs::write(&path, format!("{}\n", kept.join("\n"))).unwrap();

        let refused = Signing::open(File::open(&path).unwrap(), key(), beside(&path))
            .expect_err("a cut journal is refused");
        assert!(
            matches!(
                refused,
                JournalError::Truncated {
                    journal: 2,
                    signed: 3,
                    missing: 1
                }
            ),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(
            message.contains("2 records") && message.contains("at 3"),
            "{message}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sr_096_a_removed_head_or_an_unsigned_tail_does_not_open() {
        let path = scratch("unsigned");
        {
            let journal = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap();
            journal.append("added", &Actor::Timer, "{}").unwrap();
            journal.append("added", &Actor::Timer, "{}").unwrap();
        }
        std::fs::remove_file(beside(&path)).unwrap();
        let refused = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap_err();
        assert!(
            matches!(
                refused,
                JournalError::Unsigned {
                    journal: 2,
                    signed: 0
                }
            ),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sr_096_a_rewritten_history_or_a_foreign_head_does_not_open() {
        let path = scratch("diverged");
        {
            let journal = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap();
            journal.append("added", &Actor::Timer, "{\"n\":1}").unwrap();
        }
        // Somebody rebuilds the journal from scratch with another fact.
        std::fs::remove_file(&path).unwrap();
        File::open(&path)
            .unwrap()
            .append("added", &Actor::Timer, "{\"n\":2}")
            .unwrap();
        let refused = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap_err();
        assert!(
            matches!(refused, JournalError::Diverged { position: 1 }),
            "{refused:?}"
        );

        let foreign = SigningKey::from_seed(&[9; 32], "testd");
        let refused =
            Signing::open(File::open(&path).unwrap(), foreign, beside(&path)).unwrap_err();
        assert!(
            matches!(refused, JournalError::HeadUnreadable { .. }),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_crash_between_the_append_and_the_signature_still_opens() {
        let path = scratch("crash");
        {
            let journal = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap();
            journal.append("added", &Actor::Timer, "{}").unwrap();
        }
        // The record reached the disk and the process died before signing.
        File::open(&path)
            .unwrap()
            .append("added", &Actor::Timer, "{}")
            .unwrap();
        let journal = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap();
        assert!(journal.latest().note().contains("\n2\n"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sr_105_a_daemon_without_a_32_byte_signing_key_refuses_naming_the_variable() {
        let unset: Env = Arc::new(|_| None);
        let error = SigningKey::from_env("CSMSD_SIGNING_KEY", "csmsd", &unset).unwrap_err();
        assert!(error.to_string().contains("CSMSD_SIGNING_KEY"), "{error}");
        let short: Env = Arc::new(|_| Some("c2hvcnQ=".to_owned()));
        let error = SigningKey::from_env("CSMSD_SIGNING_KEY", "csmsd", &short).unwrap_err();
        assert!(error.to_string().contains("5 bytes, not 32"), "{error}");
        assert!(!format!("{:?}", key()).contains("seed"));
        let _ = Memory::new();
    }

    #[tokio::test]
    async fn sr_103_the_head_is_served_with_its_verifier_key() {
        use tower_service::Service as _;
        let path = scratch("route");
        let journal = Signing::open(File::open(&path).unwrap(), key(), beside(&path)).unwrap();
        journal.append("added", &Actor::Timer, "{}").unwrap();
        let mut routes = router("testd", journal.latest());
        let response = routes
            .call(
                axum::http::Request::builder()
                    .uri("/v1/testd/head")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let verifier: VerifierKey = response.headers()[VERIFIER_HEADER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let head = verifier
            .open(core::str::from_utf8(&bytes).unwrap())
            .unwrap();
        assert_eq!(head.position, 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
