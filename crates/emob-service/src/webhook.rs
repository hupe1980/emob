//! Signing what leaves, and checking what arrives.
//!
//! emob signs `CloudEvents` with the official [Standard Webhooks] library. The
//! stable event id is the receiver's idempotency key. Each retry signs the
//! original encoded event at the time of the new attempt. During rotation
//! a sender can sign with both keys and a receiver can accept either.
//!
//! [Standard Webhooks]: https://www.standardwebhooks.com
//!
//! [`verify`] checks the signature. On an incoming HTTP request use [`receive`],
//! which also requires an id and checks freshness against the supplied clock.
//! Configured keys use `whsec_<base64>`. For raw key bytes use [`Secret::raw`].

use standardwebhooks::{Webhook, WebhookError};
use subtle::ConstantTimeEq as _;

#[cfg(feature = "webhook-client")]
pub mod transport;

pub use standardwebhooks::{
    HEADER_WEBHOOK_ID as ID_HEADER, HEADER_WEBHOOK_SIGNATURE as SIGNATURE_HEADER,
    HEADER_WEBHOOK_TIMESTAMP as TIMESTAMP_HEADER,
};

/// A validated symmetric key. Debug output omits key material.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Webhook);

impl Secret {
    /// Read the unambiguous Standard Webhooks secret spelling.
    ///
    /// # Errors
    ///
    /// Rejects missing prefixes, malformed Base64 and empty keys.
    pub fn standard(configured: &str) -> Result<Self, SecretError> {
        if !configured.starts_with("whsec_") {
            return Err(SecretError::NotPrefixed);
        }
        Webhook::new(configured).map(Self).map_err(|error| {
            if matches!(error, WebhookError::EmptySecret) {
                SecretError::Empty
            } else {
                SecretError::NotBase64
            }
        })
    }

    /// Use a peer's raw key bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::Empty`] for a zero-length key.
    pub fn raw(bytes: impl AsRef<[u8]>) -> Result<Self, SecretError> {
        Webhook::from_bytes(bytes.as_ref().to_vec())
            .map(Self)
            .map_err(|_| SecretError::Empty)
    }
}

/// Why the supplied webhook signing key was refused.
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
    /// The decoded key contains no bytes.
    #[error("the webhook signing key is empty")]
    Empty,
}

impl core::fmt::Debug for Secret {
    /// Never the value.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
}

/// Sign the exact UTF-8 body using the Standard Webhooks v1 wire format.
///
/// # Errors
///
/// Returns [`WebhookError::InvalidPayload`] for a body that is not UTF-8.
pub fn sign(delivery: &Delivery<'_>, secret: &Secret) -> Result<String, WebhookError> {
    secret
        .0
        .sign(delivery.id, delivery.timestamp, delivery.payload)
}

/// Sign the same delivery under each active key during rotation.
///
/// # Errors
///
/// Returns [`WebhookError::InvalidPayload`] for a body that is not UTF-8.
pub fn sign_with<'a>(
    delivery: &Delivery<'_>,
    secrets: impl IntoIterator<Item = &'a Secret>,
) -> Result<String, WebhookError> {
    let signatures = secrets
        .into_iter()
        .map(|key| sign(delivery, key))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(signatures.join(" "))
}

/// Compare every presented signature with every configured key, including during
/// rotation. The caller checks freshness with its own clock through [`receive`].
#[must_use]
pub fn verify(delivery: &Delivery<'_>, secrets: &[Secret], header: &str) -> bool {
    secrets
        .iter()
        .filter_map(|key| sign(delivery, key).ok())
        .fold(false, |found, expected| {
            header
                .split_ascii_whitespace()
                .fold(found, |matched, supplied| {
                    matched | bool::from(expected.as_bytes().ct_eq(supplied.as_bytes()))
                })
        })
}

/// Why a receiver refused a delivery before reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refusal {
    /// A Standard Webhooks header is missing or not text.
    #[error("the delivery carries no {0} header")]
    Unsigned(&'static str),
    /// A signed field has more than one HTTP header value.
    #[error("the delivery repeats the {0} header")]
    Repeated(&'static str),
    /// The timestamp is not a number.
    #[error("the webhook-timestamp is not Unix seconds")]
    BadTimestamp,
    /// The timestamp is further from the receiver's clock than it tolerates.
    #[error(
        "the delivery was signed {skew} s away from this receiver's clock, beyond the {tolerance} s \
         it accepts — a replay, or a sender whose clock is wrong"
    )]
    Stale {
        /// How far, in seconds.
        skew: u64,
        /// The tolerance, in seconds.
        tolerance: i64,
    },
    /// No signature in the header verifies under any configured secret.
    #[error("no signature on the delivery verifies under this receiver's secret")]
    Forged,
}

/// Check a delivery as it arrives: the three headers, the freshness, and the
/// signature — and return its `webhook-id`.
///
/// The receiver half of [`sign`], with the tolerance [`verify`] leaves to its
/// caller made an argument: a receiver that verified the signature and not the
/// timestamp would accept a captured delivery for ever.
///
/// # Errors
///
/// [`Refusal`], naming which check failed.
pub fn receive(
    headers: &axum::http::HeaderMap,
    body: &[u8],
    secrets: &[Secret],
    now: time::OffsetDateTime,
    tolerance: time::Duration,
) -> Result<String, Refusal> {
    let header = |name: &'static str| {
        let mut values = headers.get_all(name).iter();
        let value = values
            .next()
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.trim().is_empty())
            .ok_or(Refusal::Unsigned(name))?;
        if values.next().is_some() {
            return Err(Refusal::Repeated(name));
        }
        Ok(value)
    };
    let id = header(ID_HEADER)?;
    let timestamp: i64 = header(TIMESTAMP_HEADER)?
        .parse()
        .map_err(|_| Refusal::BadTimestamp)?;
    let signature = header(SIGNATURE_HEADER)?;

    let skew = now.unix_timestamp().abs_diff(timestamp);
    let limit = tolerance.whole_seconds();
    if !u64::try_from(limit).is_ok_and(|limit| skew <= limit) {
        return Err(Refusal::Stale {
            skew,
            tolerance: limit,
        });
    }
    let delivery = Delivery {
        id,
        timestamp,
        payload: body,
    };
    if !verify(&delivery, secrets, signature) {
        return Err(Refusal::Forged);
    }
    Ok(id.to_owned())
}

/// Whether a delivery target may be posted to.
///
/// `https://`, or `http://` on a loopback host only — a signed body over plain
/// HTTP to another host is readable on the way, and its replay window is the
/// tolerance.
#[must_use]
pub fn secure_target(url: &str) -> bool {
    let Ok(target) = url::Url::parse(url) else {
        return false;
    };
    if !target.username().is_empty() || target.password().is_some() || target.fragment().is_some() {
        return false;
    }
    match (target.scheme(), target.host()) {
        ("https", Some(_)) | ("http", Some(url::Host::Domain("localhost"))) => true,
        ("http", Some(url::Host::Ipv4(ip))) => ip.is_loopback(),
        ("http", Some(url::Host::Ipv6(ip))) => ip.is_loopback(),
        _ => false,
    }
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
    fn upstream_signature_vector_and_key_validation() {
        // Standard Webhooks Rust SDK, test_sign.
        let key = Secret::standard("whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD").unwrap();
        let original = Delivery {
            id: "msg_27UH4WbU6Z5A5EzD8u03UvzRbpk",
            timestamp: 1_649_367_553,
            payload: br#"{"email":"test@example.com","username":"test_user"}"#,
        };
        let expected = "v1,tZ1I4/hDygAJgO5TYxiSd6Sd0kDW6hPenDe+bTa3Kkw=";
        assert_eq!(sign(&original, &key).unwrap(), expected);
        assert!(verify(&original, &[key], expected));
        assert_eq!(Secret::standard("whsec_"), Err(SecretError::Empty));
        assert_eq!(Secret::raw([]), Err(SecretError::Empty));
    }

    #[test]
    fn each_signed_field_has_one_http_value() {
        let key = Secret::raw(b"test key").unwrap();
        let mut received = headers("example", at(), "v1,signature");
        received.append(ID_HEADER, "second".parse().unwrap());
        assert_eq!(
            receive(&received, b"{}", &[key], at(), time::Duration::minutes(5)),
            Err(Refusal::Repeated(ID_HEADER))
        );
    }

    #[test]
    fn targets_require_a_host_and_explicit_destination() {
        assert!(!secure_target("https://"));
        assert!(!secure_target("https://user:password@example.org/events"));
        assert!(!secure_target("https://receiver.example/events#ignored"));
        assert!(secure_target("http://[::1]:8080/events"));
    }

    #[test]
    fn timestamp_skew_is_defined_across_the_integer_range() {
        let key = Secret::raw(b"test key").unwrap();
        for value in [i64::MIN, i64::MAX] {
            let mut received = headers("example", at(), "unused");
            received.insert(TIMESTAMP_HEADER, value.to_string().parse().unwrap());
            assert!(matches!(
                receive(
                    &received,
                    b"{}",
                    std::slice::from_ref(&key),
                    at(),
                    time::Duration::minutes(5)
                ),
                Err(Refusal::Stale { .. })
            ));
        }
    }

    // platform-interfaces/SR-032: the signature binds the id, exact body and attempt timestamp.
    #[test]
    fn a_signature_authenticates_its_own_delivery_and_nothing_else() {
        let secret = Secret::standard("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        let body = br#"{"type":"de.emob.cdr.issued"}"#;
        let header = sign(&delivery(body), &secret).unwrap();

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
        let header = sign(&delivery(body), &Secret::standard("whsec_AAAA").unwrap()).unwrap();
        assert!(!verify(&delivery(body), &[], &header));
    }

    #[test]
    fn a_rotation_needs_no_flag_day() {
        let outgoing = Secret::standard("whsec_b2xkc2VjcmV0").unwrap();
        let incoming = Secret::standard("whsec_bmV3c2VjcmV0").unwrap();
        let body = b"{}";
        let header = sign_with(&delivery(body), [&outgoing, &incoming]).unwrap();

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
        let mine = sign(&delivery(body), &secret).unwrap();
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
        let bare = Secret::raw(b"secret").unwrap();
        let header = sign(&delivery(body), &prefixed).unwrap();
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
        // A fixed attempt can be independently reconstructed from its inputs.
        let secret = Secret::standard("whsec_AAAA").unwrap();
        let body = b"{}";
        assert_eq!(
            sign(&delivery(body), &secret).unwrap(),
            sign(&delivery(body), &secret).unwrap()
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
        let as_text = sign(&delivery(body), &Secret::raw("bXlzZWNyZXQ=").unwrap()).unwrap();
        let as_base64 = sign(
            &delivery(body),
            &Secret::standard("whsec_bXlzZWNyZXQ=").unwrap(),
        )
        .unwrap();
        assert_ne!(as_text, as_base64);

        // …and each verifies only under its own.
        assert!(verify(
            &delivery(body),
            &[Secret::raw("bXlzZWNyZXQ=").unwrap()],
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

    fn headers(id: &str, at: time::OffsetDateTime, signature: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(ID_HEADER, id.parse().unwrap());
        headers.insert(
            TIMESTAMP_HEADER,
            at.unix_timestamp().to_string().parse().unwrap(),
        );
        headers.insert(SIGNATURE_HEADER, signature.parse().unwrap());
        headers
    }

    #[test]
    fn sr_029_a_receiver_takes_its_own_senders_delivery_and_refuses_a_forgery() {
        let secret = Secret::standard("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        let body = br#"{"kind":"de.emob.cdr.issued"}"#;
        let signed = sign(&Delivery::new("DE*ABC/CP-1-1", at(), body), &secret).unwrap();
        let tolerance = time::Duration::minutes(5);

        assert_eq!(
            receive(
                &headers("DE*ABC/CP-1-1", at(), &signed),
                body,
                std::slice::from_ref(&secret),
                at() + time::Duration::seconds(3),
                tolerance,
            ),
            Ok("DE*ABC/CP-1-1".to_owned())
        );

        let other = Secret::raw("somebody else's").unwrap();
        let forged = sign(&Delivery::new("DE*ABC/CP-1-1", at(), body), &other).unwrap();
        assert_eq!(
            receive(
                &headers("DE*ABC/CP-1-1", at(), &forged),
                body,
                std::slice::from_ref(&secret),
                at(),
                tolerance,
            ),
            Err(Refusal::Forged)
        );
        assert_eq!(
            receive(
                &axum::http::HeaderMap::new(),
                body,
                std::slice::from_ref(&secret),
                at(),
                tolerance
            ),
            Err(Refusal::Unsigned(ID_HEADER))
        );
    }

    #[test]
    fn sr_030_a_signature_older_than_the_tolerance_is_a_replay() {
        let secret = Secret::raw("shared").unwrap();
        let body = b"{}";
        let signed = sign(&Delivery::new("id-1", at(), body), &secret).unwrap();
        let refusal = receive(
            &headers("id-1", at(), &signed),
            body,
            std::slice::from_ref(&secret),
            at() + time::Duration::minutes(10),
            time::Duration::minutes(5),
        );
        assert_eq!(
            refusal,
            Err(Refusal::Stale {
                skew: 600,
                tolerance: 300
            })
        );
    }
}
