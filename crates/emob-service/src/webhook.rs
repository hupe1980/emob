//! Signing what leaves, and checking what arrives.
//!
//! # One signer, because two is how a receiver stops trusting either
//!
//! A platform notifies outward in several places — a CDR was refused, a duty
//! falls due, a station stopped answering — and each of them is a `POST` to a
//! URL somebody configured. Signed differently in each place, a receiver has to
//! implement three verifiers and will get one wrong.
//!
//! So there is one, and it is [Standard Webhooks]: HMAC-SHA256 over
//! `{id}.{timestamp}.{payload}`, base64, in a `webhook-signature` header that
//! can carry **several** signatures at once so a secret can be rotated without
//! a flag day.
//!
//! [Standard Webhooks]: https://www.standardwebhooks.com
//!
//! # The timestamp is an argument, and the tolerance is the caller's
//!
//! Signing reads no clock: the instant is passed in, so a delivery replayed
//! from an outbox produces the same bytes it did the first time — which is the
//! property that lets a receiver de-duplicate on the signature rather than on a
//! guess. [`verify`] takes the instant it is checking against for the same
//! reason, and this crate has no opinion about how old is too old: a five-minute
//! window is right for an interactive callback and wrong for an overnight batch,
//! and a library that picked one would be picking it for both.
//!
//! # The secret's encoding is stated, never inferred
//!
//! Standard Webhooks writes a secret as `whsec_<base64>`, and some deployments
//! configure a passphrase instead. Guessing between them is ambiguous for
//! exactly the secrets that look like base64: `"mysecret"` is eight ASCII bytes
//! *and* a valid base64 string, `"hunter2"` is not, and a sender and receiver
//! that read the same string differently disagree on every delivery.
//!
//! So [`Secret::standard`] and [`Secret::raw`] are two constructors and the
//! first is fallible. The same string means two different keys, and which one a
//! deployment wants is not something a library can infer (D188).
//!
//! # Comparison is constant time, and a receiver that holds no secret rejects
//!
//! A signature compared with `==` leaks where two differ. And a verifier
//! configured with no secrets rejects everything rather than accepting
//! everyone — the deployment where somebody forgot the secret is exactly the one
//! nobody would notice.

use core::fmt;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq as _;

type HmacSha256 = Hmac<Sha256>;

/// The header a signature travels in.
pub const SIGNATURE_HEADER: &str = "webhook-signature";
/// The header the delivery's id travels in.
pub const ID_HEADER: &str = "webhook-id";
/// The header the signed timestamp travels in.
pub const TIMESTAMP_HEADER: &str = "webhook-timestamp";
/// The version prefix Standard Webhooks puts on a symmetric signature.
const VERSION: &str = "v1";

/// A shared secret, which neither prints itself nor compares in variable time.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// A secret in the Standard Webhooks spelling — `whsec_<base64>`.
    ///
    /// # Errors
    ///
    /// [`SecretError`] when the prefix is missing or the rest is not base64.
    ///
    /// # Why this refuses rather than guesses
    ///
    /// Stripping the prefix, trying base64 and falling back to the raw bytes is
    /// **ambiguous for exactly the secrets that look like base64**: `"mysecret"`
    /// is eight ASCII bytes and also a valid base64 string, so it would silently
    /// become six arbitrary ones while `"hunter2"` stayed as it is. The only
    /// symptom is `SignatureMismatch` on every delivery, which points at the
    /// payload rather than at the key.
    pub fn standard(configured: &str) -> Result<Self, SecretError> {
        let body = configured
            .strip_prefix("whsec_")
            .ok_or(SecretError::NotPrefixed)?;
        base64::engine::general_purpose::STANDARD
            .decode(body)
            .map(Self)
            .map_err(|_| SecretError::NotBase64)
    }

    /// A secret that is the bytes it is written as.
    ///
    /// For a peer whose configuration is a passphrase rather than the
    /// specification's encoding. Named so the choice is visible in a diff: the
    /// same string means two different keys under the two constructors, and
    /// which one a deployment wants is not something a library can infer.
    #[must_use]
    pub fn raw(bytes: impl AsRef<[u8]>) -> Self {
        Self(bytes.as_ref().to_vec())
    }
}

/// A configured secret that is not the Standard Webhooks spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// The value does not start with `whsec_`.
    #[error(
        "a Standard Webhooks secret is written `whsec_<base64>`; use `Secret::raw` for a \
         passphrase, because the same string is a different key under the two readings"
    )]
    NotPrefixed,
    /// The part after the prefix is not base64.
    #[error("the part after `whsec_` is not base64")]
    NotBase64,
}

impl fmt::Debug for Secret {
    /// Never the value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// What a delivery is signed over.
///
/// The three fields together, because the signature covers all three: a
/// receiver that checked the payload and not the id would accept the same body
/// under a second delivery id, and one that checked neither timestamp would
/// accept a replay forever.
#[derive(Debug, Clone, Copy)]
pub struct Delivery<'a> {
    /// The delivery's unique id — `webhook-id`.
    pub id: &'a str,
    /// When it was signed, as Unix seconds — `webhook-timestamp`.
    pub timestamp: i64,
    /// The body, exactly as it will be sent.
    pub payload: &'a [u8],
}

impl<'a> Delivery<'a> {
    /// A delivery at an instant.
    ///
    /// The instant is an argument. See the module documentation for why nothing
    /// here reads a clock.
    #[must_use]
    pub const fn new(id: &'a str, at: time::OffsetDateTime, payload: &'a [u8]) -> Self {
        Self {
            id,
            timestamp: at.unix_timestamp(),
            payload,
        }
    }

    /// The bytes the MAC is taken over — `{id}.{timestamp}.{payload}`.
    fn signed_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.payload.len() + self.id.len() + 24);
        bytes.extend_from_slice(self.id.as_bytes());
        bytes.push(b'.');
        bytes.extend_from_slice(self.timestamp.to_string().as_bytes());
        bytes.push(b'.');
        bytes.extend_from_slice(self.payload);
        bytes
    }
}

/// Sign a delivery.
///
/// Returns the header value — `v1,<base64>` — which a caller sets as
/// [`SIGNATURE_HEADER`] beside [`ID_HEADER`] and [`TIMESTAMP_HEADER`].
#[must_use]
pub fn sign(delivery: &Delivery<'_>, secret: &Secret) -> String {
    format!("{VERSION},{}", mac(delivery, secret))
}

/// Sign a delivery under several secrets at once.
///
/// What makes a rotation possible without a flag day: the header carries both
/// the outgoing and the incoming signature, space-separated, and a receiver that
/// has either one accepts.
#[must_use]
pub fn sign_with<'a, I>(delivery: &Delivery<'_>, secrets: I) -> String
where
    I: IntoIterator<Item = &'a Secret>,
{
    secrets
        .into_iter()
        .map(|secret| format!("{VERSION},{}", mac(delivery, secret)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a `webhook-signature` header authenticates a delivery.
///
/// Every space-separated element is tried, because a sender mid-rotation sends
/// two. An element whose version this build does not know is skipped rather
/// than refused — a future asymmetric `v1a` alongside a `v1` must not make the
/// `v1` unreadable.
///
/// **The freshness of the timestamp is not checked here.** See the module
/// documentation: the tolerance belongs to the caller, and
/// [`Delivery::timestamp`] is what it compares.
#[must_use]
pub fn verify(delivery: &Delivery<'_>, secrets: &[Secret], header: &str) -> bool {
    // A verifier configured with nothing rejects everything.
    if secrets.is_empty() {
        return false;
    }
    let expected: Vec<String> = secrets.iter().map(|s| mac(delivery, s)).collect();

    // Every candidate is compared against every expected signature, and the
    // loop does not stop early: an early return on the first match is fine, and
    // an early return on the first *mismatch* would leak which secret was
    // tried. Folding with `|` keeps the work constant in the number of secrets.
    header
        .split(' ')
        .filter_map(|element| element.strip_prefix(&format!("{VERSION},")))
        .fold(false, |found, candidate| {
            expected.iter().fold(found, |found, expected| {
                found | bool::from(expected.as_bytes().ct_eq(candidate.as_bytes()))
            })
        })
}

/// The base64 MAC of a delivery under one secret.
fn mac(delivery: &Delivery<'_>, secret: &Secret) -> String {
    // `new_from_slice` is infallible for HMAC — the construction accepts a key
    // of any length — so this is a signature, not a fallible step. The branch
    // that used to stand here fell back to an **empty** key, which is a
    // different signature reached silently: unreachable, and the wrong thing to
    // be unreachable to.
    let mut hmac = HmacSha256::new_from_slice(&secret.0).expect("HMAC takes a key of any length");
    hmac.update(&delivery.signed_bytes());
    base64::engine::general_purpose::STANDARD.encode(hmac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn at() -> time::OffsetDateTime {
        datetime!(2026-07-01 09:00:00 UTC)
    }

    fn delivery(payload: &[u8]) -> Delivery<'_> {
        Delivery::new("msg_2Xy", at(), payload)
    }

    #[test]
    fn a_signature_authenticates_its_own_delivery_and_nothing_else() {
        let secret = Secret::standard("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        let body = br#"{"type":"de.emob.cdr.issued"}"#;
        let header = sign(&delivery(body), &secret);

        assert!(verify(
            &delivery(body),
            std::slice::from_ref(&secret),
            &header
        ));

        // A different body, a different id and a different instant each break
        // it, because the MAC covers all three.
        assert!(!verify(
            &delivery(br#"{"type":"de.emob.cdr.rejected"}"#),
            std::slice::from_ref(&secret),
            &header
        ));
        assert!(!verify(
            &Delivery::new("msg_OTHER", at(), body),
            std::slice::from_ref(&secret),
            &header
        ));
        assert!(!verify(
            &Delivery::new("msg_2Xy", at() + time::Duration::seconds(1), body),
            &[secret],
            &header
        ));
    }

    #[test]
    fn a_receiver_that_holds_no_secret_rejects_rather_than_accepts() {
        // The deployment where somebody forgot the secret is exactly the one
        // nobody would notice.
        let body = b"{}";
        let header = sign(&delivery(body), &Secret::standard("whsec_AAAA").unwrap());
        assert!(!verify(&delivery(body), &[], &header));
    }

    #[test]
    fn a_rotation_needs_no_flag_day() {
        let outgoing = Secret::standard("whsec_b2xkc2VjcmV0").unwrap();
        let incoming = Secret::standard("whsec_bmV3c2VjcmV0").unwrap();
        let body = b"{}";
        let header = sign_with(&delivery(body), [&outgoing, &incoming]);

        // A receiver that has only the old one accepts, and so does one that
        // has only the new one — which is what makes the two ends independent.
        assert!(verify(
            &delivery(body),
            std::slice::from_ref(&outgoing),
            &header
        ));
        assert!(verify(
            &delivery(body),
            std::slice::from_ref(&incoming),
            &header
        ));
        assert!(verify(&delivery(body), &[outgoing, incoming], &header));
        assert!(!verify(
            &delivery(body),
            &[Secret::standard("whsec_c29tZXRoaW5n").unwrap()],
            &header
        ));
    }

    #[test]
    fn a_version_this_build_does_not_know_does_not_hide_one_it_does() {
        let secret = Secret::standard("whsec_AAAA").unwrap();
        let body = b"{}";
        let mine = sign(&delivery(body), &secret);
        let mixed = format!("v1a,c29tZXRoaW5nZWxzZQ== {mine}");
        assert!(verify(&delivery(body), &[secret], &mixed));
    }

    #[test]
    fn both_spellings_of_a_secret_are_the_same_secret() {
        // `whsec_<base64>` is what a Standard Webhooks console prints; the raw
        // bytes are what a hand-configured deployment writes. A verifier that
        // accepted only one would reject half its senders.
        let body = b"{}";
        let prefixed = Secret::standard("whsec_c2VjcmV0").unwrap();
        let bare = Secret::raw(b"secret");
        let header = sign(&delivery(body), &prefixed);
        assert!(verify(&delivery(body), &[bare], &header));
    }

    #[test]
    fn a_secret_never_prints_itself() {
        assert_eq!(
            format!("{:?}", Secret::standard("whsec_AAAA").unwrap()),
            "Secret(…)"
        );
    }

    #[test]
    fn signing_reads_no_clock_so_a_replayed_delivery_is_the_same_bytes() {
        // The property an outbox needs: a retry of a delivery is byte-identical
        // to the first attempt, so a receiver can de-duplicate on the signature.
        let secret = Secret::standard("whsec_AAAA").unwrap();
        let body = b"{}";
        assert_eq!(
            sign(&delivery(body), &secret),
            sign(&delivery(body), &secret)
        );
    }

    #[test]
    fn the_two_spellings_are_two_constructors_because_one_string_is_two_keys() {
        // `"mysecret"` is eight ASCII bytes *and* a valid base64 string, so a
        // constructor that guessed would silently key on six arbitrary bytes —
        // while `"hunter2"` is not base64 and would stay as it is. An operator
        // cannot predict which they get, and the only symptom is that every
        // delivery fails to verify.
        assert_eq!(Secret::standard("mysecret"), Err(SecretError::NotPrefixed));

        // The same string under the two readings is two different keys, and
        // that is exactly why neither is a default.
        let body = br#"{"type":"de.emob.cdr.issued"}"#;
        let as_text = sign(&delivery(body), &Secret::raw("bXlzZWNyZXQ="));
        let as_base64 = sign(
            &delivery(body),
            &Secret::standard("whsec_bXlzZWNyZXQ=").unwrap(),
        );
        assert_ne!(as_text, as_base64);

        // …and each verifies only under its own.
        assert!(verify(
            &delivery(body),
            &[Secret::raw("bXlzZWNyZXQ=")],
            &as_text
        ));
        assert!(!verify(
            &delivery(body),
            &[Secret::standard("whsec_bXlzZWNyZXQ=").unwrap()],
            &as_text
        ));
    }

    #[test]
    fn a_malformed_standard_secret_is_refused_rather_than_keyed_on_its_prefix() {
        // The old fallback kept the `whsec_` prefix in the key material on the
        // failure path while stripping it on the success path, so one typo in
        // the base64 changed the key rather than reporting a bad secret.
        assert_eq!(
            Secret::standard("whsec_not base64!"),
            Err(SecretError::NotBase64)
        );
    }
}
