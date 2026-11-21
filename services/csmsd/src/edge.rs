//! The station edge: who may connect, over what, and how much they may say.
//!
//! # A provisioned identity is not a credential
//!
//! An OCPP identity is printed on the station's housing. Until spec 005 that was
//! all `csmsd` asked for: anyone who read it could connect as that station. Now
//! every `[[stations]]` entry names the security profile it connects with
//! `[OCPP 2.0.1 Part 2 §A00]`, and [`Gate`] checks the matching credential:
//!
//! | Profile | Transport | Credential checked |
//! |---|---|---|
//! | 1 | plain WebSocket (or TLS) | Basic password against the estate's Argon2id hash |
//! | 2 | TLS (or a declared terminating proxy) | the same |
//! | 3 | TLS with a client certificate | the chain to `[csms] client_ca` (rustls) and its CN = the identity |
//!
//! The estate holds the password's **hash**: a copy of the estate or of the
//! environment is not a station's password. The kit already refuses a Basic
//! user name that is not the URL identity (A00.FR.204/207) before [`Gate`] is
//! asked; an identity nobody provisioned is answered 404, a wrong credential
//! 401 — both counted on [`Edge`] and logged with the identity.
//!
//! # TLS, or a reason there is none
//!
//! [`Security::from_estate`] refuses a non-loopback bind without
//! `[csms] tls_cert` unless `tls_terminated_by_proxy` is declared — a CSMS that
//! takes Basic passwords in clear on a public address is handing them out. With
//! a client CA, a certificate is *offered* — verified when presented, absent
//! for a profile-2 station — and [`Gate`] decides per station whether it was
//! required.
//!
//! # How much a station may say
//!
//! The kit bounds all of it: sessions (`max_connections`, answered 503),
//! handshakes (count and timeout), dead peers (keepalive), a live peer that
//! talks too much (`messages_per_minute`, answered `SecurityError` before the
//! handler) and one that says nothing at all (`idle_timeout_secs`, measured on
//! OCPP messages, never pings). [`Edge`] counts each refusal from the kit's
//! session events.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use emob_service::estate::{Csms, EstateError, SecurityProfile, Station};
use ocpp_kit::Version;
use ocpp_kit::engine::RateLimit;
use ocpp_kit::transport::{
    Admission, Auth, AuthOutcome, BoxFuture, Credentials, CsmsHandle, DisconnectReason, ServerTls,
    SessionEvent,
};

use crate::ChargePoint;

fn invalid(key: impl Into<String>, reason: impl ToString) -> EstateError {
    EstateError::Invalid {
        key: key.into(),
        reason: reason.to_string(),
    }
}

/// What a station must present.
#[derive(Debug, Clone)]
enum Credential {
    /// Profile 1 or 2: a Basic password matching this Argon2id hash.
    Password {
        /// Whether the transport must be encrypted (profile 2).
        encrypted: bool,
        /// The PHC string.
        hash: String,
    },
    /// Profile 3: a client certificate whose CN is the identity.
    Certificate,
}

/// The edge as the estate configures it.
#[derive(Debug, Clone)]
pub struct Security {
    tls: Option<ServerTls>,
    proxy: bool,
    /// Whether a profile-3 station can be admitted here at all.
    certificates: bool,
    /// The limits.
    pub csms: Csms,
}

impl Security {
    /// Whether a profile-3 station can be admitted at all: a client CA is
    /// named and TLS ends here.
    #[must_use]
    pub const fn admits_certificates(&self) -> bool {
        self.certificates
    }

    /// Read `[csms]` and every station's credential, refusing by key.
    ///
    /// `bind` is the OCPP address: a non-loopback one needs TLS or a declared
    /// proxy.
    ///
    /// # Errors
    ///
    /// [`EstateError::Invalid`] naming the key, or naming the PEM file that
    /// could not be read.
    pub fn from_estate(
        csms: &Csms,
        stations: &[Station],
        bind: SocketAddr,
    ) -> Result<Self, EstateError> {
        let proxy = csms.tls_terminated_by_proxy;
        let tls_paths = match (&csms.tls_cert, &csms.tls_key) {
            (Some(cert), Some(key)) => Some((csms.path(cert), csms.path(key))),
            (None, None) => None,
            (Some(_), None) => {
                return Err(invalid(
                    "csms.tls_key",
                    "tls_cert is set and its key is not",
                ));
            }
            (None, Some(_)) => {
                return Err(invalid(
                    "csms.tls_cert",
                    "tls_key is set and its certificate is not",
                ));
            }
        };
        if tls_paths.is_some() && proxy {
            return Err(invalid(
                "csms.tls_terminated_by_proxy",
                "TLS is configured here and also declared terminated by a proxy; name one",
            ));
        }
        if !bind.ip().is_loopback() && tls_paths.is_none() && !proxy {
            return Err(invalid(
                "csms.tls_cert",
                format!(
                    "CSMSD_BIND {bind} is not a loopback address, so stations would send their \
                     passwords in clear: set [csms] tls_cert and tls_key, or declare \
                     tls_terminated_by_proxy = true when a proxy in front of csmsd terminates TLS"
                ),
            ));
        }

        let mut wants_certificates = false;
        for (index, station) in stations.iter().enumerate() {
            let at = |field: &str| format!("stations[{index}].{field}");
            let credential =
                credential_of(station).map_err(|(field, reason)| invalid(at(field), reason))?;
            if matches!(credential, Credential::Certificate) {
                wants_certificates = true;
            }
        }
        if wants_certificates {
            if proxy {
                return Err(invalid(
                    "csms.tls_terminated_by_proxy",
                    "a profile-3 station's certificate ends at the proxy, so csmsd cannot bind it \
                     to the identity: terminate TLS in csmsd for profile 3",
                ));
            }
            if csms.client_ca.is_none() {
                return Err(invalid(
                    "csms.client_ca",
                    "a station is provisioned for security profile 3 and no CA is named for its \
                     certificate to chain to",
                ));
            }
        }

        let tls = match tls_paths {
            Some((cert, key)) => Some(server_tls(
                &cert,
                &key,
                csms.client_ca.as_deref().map(|ca| csms.path(ca)).as_deref(),
            )?),
            None => None,
        };
        Ok(Self {
            tls,
            proxy,
            certificates: csms.client_ca.is_some() && !proxy,
            csms: csms.clone(),
        })
    }

    /// The TLS configuration the listener serves, if any.
    #[must_use]
    pub fn tls(&self) -> Option<ServerTls> {
        self.tls.clone()
    }
}

fn pem_error(path: &Path, error: impl std::fmt::Display) -> EstateError {
    invalid(
        "csms",
        format!(
            "{} could not be read ({error}); for a local run, `just dev-ca` mints a throwaway CA \
             and server certificate into target/dev-tls/",
            path.display()
        ),
    )
}

/// The listener's TLS: profile 2 alone, or profiles 2 and 3 side by side when
/// a client CA is configured — a certificate is then verified when presented
/// and may be absent, and `Gate` decides per station which one it requires.
fn server_tls(cert: &Path, key: &Path, client_ca: Option<&Path>) -> Result<ServerTls, EstateError> {
    match client_ca {
        Some(ca) => ServerTls::with_optional_client_auth(cert, key, ca),
        None => ServerTls::new(cert, key),
    }
    .map_err(|e| pem_error(cert, e))
}

/// What the edge refused, counted.
#[derive(Debug, Default)]
pub struct Counters {
    /// Upgrades answered 401: a wrong or missing credential.
    pub refused_credentials: AtomicU64,
    /// Upgrades answered 404: an identity nobody provisioned.
    pub refused_unknown: AtomicU64,
    /// Upgrades answered 503: `max_connections` reached.
    pub refused_capacity: AtomicU64,
    /// Messages refused for exceeding `messages_per_minute`.
    pub rate_limited: AtomicU64,
    /// Sessions closed for being silent past `idle_timeout_secs`.
    pub idle_closed: AtomicU64,
}

impl Counters {
    /// A counter's value.
    #[must_use]
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// The edge's limits, and what it refused.
#[derive(Debug)]
pub struct Edge {
    rate: RateLimit,
    idle: Duration,
    /// What was refused.
    pub counters: Counters,
}

impl Edge {
    /// An edge with the estate's limits.
    ///
    /// # Errors
    ///
    /// When `messages_per_minute` is zero.
    pub fn new(csms: &Csms) -> Result<Self, EstateError> {
        Ok(Self {
            rate: RateLimit::per_minute(csms.messages_per_minute)
                .map_err(|_| invalid("csms", "messages_per_minute must be at least 1"))?,
            idle: Duration::from_secs(csms.idle_timeout_secs.max(1)),
            counters: Counters::default(),
        })
    }

    /// The kit enforces these: a station over `rate` is answered
    /// `SecurityError`, one silent past `idle` is closed.
    pub fn limit(
        &self,
        builder: ocpp_kit::transport::CsmsBuilder,
    ) -> ocpp_kit::transport::CsmsBuilder {
        builder.message_rate(self.rate).idle_timeout(self.idle)
    }

    /// Follow the kit's session events: count its refusals, log the opens.
    pub async fn watch(self: Arc<Self>, csms: CsmsHandle) {
        let mut events = csms.events();
        loop {
            match events.recv().await {
                Ok(SessionEvent::Opened {
                    identity, version, ..
                }) => {
                    tracing::info!(station = %identity, ocpp = %version, "station connected");
                }
                Ok(SessionEvent::RateLimited { identity }) => {
                    self.counters.rate_limited.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(station = %identity, "a station is over its message budget");
                }
                Ok(SessionEvent::Closed {
                    identity,
                    reason: DisconnectReason::Idle(idle),
                }) => {
                    self.counters.idle_closed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(station = %identity, ?idle, "closed a silent session");
                }
                Ok(SessionEvent::Refused {
                    identity,
                    reason,
                    remote,
                }) => {
                    let station = identity
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    tracing::warn!(%station, %remote, %reason, "a station's connection was refused");
                    let counter = match reason.status() {
                        Some(401) => &self.counters.refused_credentials,
                        Some(404) => &self.counters.refused_unknown,
                        Some(503) => &self.counters.refused_capacity,
                        _ => continue,
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// The credential a station's record requires, from its profile and its hash.
///
/// # Errors
///
/// The field and why, for a record whose credential does not fit its profile.
fn credential_of(station: &Station) -> Result<Credential, (&'static str, String)> {
    match station.security_profile {
        SecurityProfile::Basic | SecurityProfile::TlsBasic => {
            let hash = station.password_hash.clone().ok_or_else(|| {
                (
                    "password_hash",
                    format!(
                        "security profile {} authenticates with a password and none is \
                         provisioned (`csmsd hash-password` prints one)",
                        station.security_profile.number()
                    ),
                )
            })?;
            let parsed = argon2::PasswordHash::new(&hash)
                .map_err(|error| ("password_hash", format!("not a PHC string: {error}")))?;
            if parsed.algorithm != argon2::Algorithm::Argon2id.ident() {
                return Err((
                    "password_hash",
                    format!("{} is not Argon2id", parsed.algorithm),
                ));
            }
            Ok(Credential::Password {
                encrypted: station.security_profile == SecurityProfile::TlsBasic,
                hash,
            })
        }
        SecurityProfile::TlsClientCertificate => {
            if station.password_hash.is_some() {
                return Err((
                    "password_hash",
                    "security profile 3 authenticates with a certificate; a password beside it \
                     is a second credential nobody checks"
                        .to_owned(),
                ));
            }
            Ok(Credential::Certificate)
        }
    }
}

/// Where admission finds a station (spec 021): the registry, read on every
/// connection, so a registered station connects and a retired one is refused
/// without a restart (SR-146).
pub trait Registry: Send + Sync + 'static {
    /// The station registered under `identity`, and the point it is.
    fn admit(&self, identity: &str) -> Option<(ChargePoint, Station)>;
}

/// Decides whether a station may connect: its provisioning, its version and
/// the credential its profile requires.
pub struct Gate {
    registry: Arc<dyn Registry>,
    security: Arc<Security>,
}

impl Gate {
    /// A gate over the fleet.
    #[must_use]
    pub fn new(registry: Arc<dyn Registry>, security: Arc<Security>) -> Self {
        Self { registry, security }
    }

    async fn decide(&self, auth: Auth) -> AuthOutcome {
        // An identity nobody provisioned is answered 404 rather than 401, so
        // an operator can tell a typo from a bad password
        // `[OCPP 2.0.1 Part 4 §3.1.1]` — and before any hash is computed.
        let Some((point, station)) = self.registry.admit(auth.identity.as_str()) else {
            return AuthOutcome::Unknown;
        };
        let refuse = |why: &str| {
            tracing::warn!(station = %auth.identity, remote = %auth.remote, profile = %auth.profile, "{why}");
            AuthOutcome::Reject
        };
        // The station speaks the version it was provisioned for, whatever it
        // would rather: the admission pins it, and one that does not offer
        // it is refused `[OCPP 2.0.1 Part 4 §3.2]`.
        let version = point.ocpp;
        let admit =
            move |point| AuthOutcome::from(Admission::new().context(point).version(version));
        let Ok(credential) = credential_of(&station) else {
            return refuse("the station has no credential provisioned");
        };
        let credential = &credential;
        let encrypted = auth.profile.is_transport_encrypted() || self.security.proxy;
        match (credential, auth.credentials) {
            (
                Credential::Password {
                    encrypted: true, ..
                },
                _,
            ) if !encrypted => refuse("a profile-2 station connected without TLS"),
            (Credential::Password { hash, .. }, Credentials::Basic { password, .. }) => {
                let hash = hash.clone();
                let verified =
                    tokio::task::spawn_blocking(move || verify_password(&hash, &password))
                        .await
                        .unwrap_or(false);
                if verified {
                    admit(point)
                } else {
                    refuse("the station's password does not match its provisioned hash")
                }
            }
            (Credential::Password { .. }, _) => refuse("the station sent no Basic password"),
            (Credential::Certificate, Credentials::ClientCertificate { der }) => {
                match common_name(&der) {
                    Some(name) if name == auth.identity.as_str() => admit(point),
                    _ => refuse("the station's certificate names another identity"),
                }
            }
            (Credential::Certificate, _) => {
                refuse("a profile-3 station presented no client certificate")
            }
        }
    }
}

impl ocpp_kit::transport::Authenticator for Gate {
    fn authenticate(&self, auth: Auth) -> BoxFuture<'_, AuthOutcome> {
        Box::pin(self.decide(auth))
    }
}

/// Whether `password` matches the PHC `hash`.
#[must_use]
pub fn verify_password(hash: &str, password: &[u8]) -> bool {
    use argon2::PasswordVerifier as _;
    argon2::PasswordHash::new(hash).is_ok_and(|parsed| {
        argon2::Argon2::default()
            .verify_password(password, &parsed)
            .is_ok()
    })
}

/// The Argon2id hash of the password a station of `version` is configured
/// with: hex for 1.6's `AuthorizationKey`, UTF-8 of at least 16 characters for
/// 2.x's `BasicAuthPassword` — the octets the station will send.
///
/// # Errors
///
/// What the kit says about a password the version does not allow.
pub fn hash_password(version: Version, configured: &str) -> Result<String, String> {
    use argon2::PasswordHasher as _;
    let password = ocpp_kit::transport::BasicAuthPassword::for_version(version, configured)
        .map_err(|error| error.to_string())?;
    let mut salt = [0_u8; 16];
    aws_lc_rs::rand::fill(&mut salt).map_err(|_| "the system RNG failed".to_owned())?;
    let salt = argon2::password_hash::SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| error.to_string())
}

/// The subject Common Name of a DER certificate, if it has one.
///
/// Enough DER to walk `Certificate → tbsCertificate → subject` and find the
/// last `2.5.4.3` attribute. The certificate has already been verified by
/// rustls against `client_ca`; this only reads who it names.
#[must_use]
pub fn common_name(der: &[u8]) -> Option<String> {
    /// One TLV: (tag, contents, rest).
    fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, rest) = input.split_first()?;
        let (&first, mut rest) = rest.split_first()?;
        let len = if first < 0x80 {
            usize::from(first)
        } else {
            let count = usize::from(first & 0x7f);
            if count == 0 || count > 4 || rest.len() < count {
                return None;
            }
            let (bytes, after) = rest.split_at(count);
            rest = after;
            bytes
                .iter()
                .fold(0_usize, |acc, b| (acc << 8) | usize::from(*b))
        };
        (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
    }
    const SEQUENCE: u8 = 0x30;
    const SET: u8 = 0x31;
    const CN: &[u8] = &[0x55, 0x04, 0x03];

    let (SEQUENCE, certificate, _) = tlv(der)? else {
        return None;
    };
    let (SEQUENCE, tbs, _) = tlv(certificate)? else {
        return None;
    };
    let mut fields = tbs;
    // version [0] EXPLICIT is optional.
    if fields.first() == Some(&0xa0) {
        fields = tlv(fields)?.2;
    }
    // serialNumber, signature, issuer, validity — then subject.
    for _ in 0..4 {
        fields = tlv(fields)?.2;
    }
    let (SEQUENCE, mut subject, _) = tlv(fields)? else {
        return None;
    };
    let mut found = None;
    while !subject.is_empty() {
        let (SET, mut set, rest) = tlv(subject)? else {
            return None;
        };
        subject = rest;
        while !set.is_empty() {
            let (SEQUENCE, attribute, rest) = tlv(set)? else {
                return None;
            };
            set = rest;
            let (0x06, oid, value) = tlv(attribute)? else {
                return None;
            };
            if oid == CN {
                let (_, text, _) = tlv(value)?;
                found = Some(String::from_utf8(text.to_vec()).ok()?);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const CP_1: &str = include_str!("../tests/fixtures/tls/cp-1.pem");

    fn der(pem: &str) -> Vec<u8> {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;
        CertificateDer::from_pem_slice(pem.as_bytes())
            .unwrap()
            .to_vec()
    }

    #[test]
    fn sr_041_the_common_name_is_read_from_the_certificate() {
        assert_eq!(common_name(&der(CP_1)).as_deref(), Some("CP-1"));
        assert_eq!(common_name(b"not a certificate"), None);
    }

    #[test]
    fn sr_040_a_hash_verifies_its_password_and_no_other() {
        let hash = hash_password(Version::V2_0_1, "a-password-of-16-chars").unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify_password(&hash, b"a-password-of-16-chars"));
        assert!(!verify_password(&hash, b"a-password-of-16-charz"));
        assert!(
            hash_password(Version::V2_0_1, "short").is_err(),
            "A00.FR.205"
        );
    }
}
