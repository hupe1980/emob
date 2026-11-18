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
//! takes Basic passwords in clear on a public address is handing them out. The
//! rustls server config is built here rather than by the kit: the kit's
//! mutual-TLS constructor demands a certificate from every peer, and a profile-2
//! station has none, so client authentication is *offered* and [`Gate`] decides
//! per station whether it was required.
//!
//! # How much a station may say
//!
//! The kit bounds sessions (`max_connections`, answered 503), handshakes (count
//! and timeout) and dead peers (keepalive). It does not bound a live peer that
//! talks too much or says nothing at all; [`Guarded`] adds a per-connection
//! message budget (an excess message is answered `SecurityError` and not
//! processed) and [`Edge::sweep`] closes sessions silent past
//! `idle_timeout_secs`.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use emob_service::estate::{Csms, EstateError, SecurityProfile, Station};
use ocpp_kit::RawValue;
use ocpp_kit::Version;
use ocpp_kit::engine::IncomingRequest;
use ocpp_kit::rpc::CallError;
use ocpp_kit::transport::{
    Auth, AuthOutcome, BoxFuture, Credentials, CsmsHandle, Ctx, Handler, ServerTls, SessionContext,
    SessionEvent,
};
use ocpp_kit::types::Identity;
use tokio::time::Instant;

use crate::Provisioning;

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
    credentials: BTreeMap<String, Credential>,
    /// The limits.
    pub csms: Csms,
}

impl Security {
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

        let mut credentials = BTreeMap::new();
        let mut wants_certificates = false;
        for (index, station) in stations.iter().enumerate() {
            let at = |field: &str| format!("stations[{index}].{field}");
            let credential = match station.security_profile {
                SecurityProfile::Basic | SecurityProfile::TlsBasic => {
                    let hash = station.password_hash.clone().ok_or_else(|| {
                        invalid(
                            at("password_hash"),
                            format!(
                                "security profile {} authenticates with a password and none is \
                                 provisioned (`csmsd hash-password` prints one)",
                                station.security_profile.number()
                            ),
                        )
                    })?;
                    let parsed = argon2::PasswordHash::new(&hash).map_err(|error| {
                        invalid(at("password_hash"), format!("not a PHC string: {error}"))
                    })?;
                    if parsed.algorithm != argon2::Algorithm::Argon2id.ident() {
                        return Err(invalid(
                            at("password_hash"),
                            format!("{} is not Argon2id", parsed.algorithm),
                        ));
                    }
                    Credential::Password {
                        encrypted: station.security_profile == SecurityProfile::TlsBasic,
                        hash,
                    }
                }
                SecurityProfile::TlsClientCertificate => {
                    if station.password_hash.is_some() {
                        return Err(invalid(
                            at("password_hash"),
                            "security profile 3 authenticates with a certificate; a password \
                             beside it is a second credential nobody checks",
                        ));
                    }
                    wants_certificates = true;
                    Credential::Certificate
                }
            };
            credentials.insert(station.identity.clone(), credential);
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
            credentials,
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

fn server_tls(cert: &Path, key: &Path, client_ca: Option<&Path>) -> Result<ServerTls, EstateError> {
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| pem_error(cert, e))?
        .collect::<Result<_, _>>()
        .map_err(|e| pem_error(cert, e))?;
    if chain.is_empty() {
        return Err(pem_error(cert, "no certificate in it"));
    }
    let key_der = PrivateKeyDer::from_pem_file(key).map_err(|e| pem_error(key, e))?;
    // The provider the kit uses, chosen explicitly so a process-wide default
    // cannot change the cipher suites underneath.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| invalid("csms", e))?;
    let (builder, mutual) = match client_ca {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            for root in CertificateDer::pem_file_iter(ca).map_err(|e| pem_error(ca, e))? {
                roots
                    .add(root.map_err(|e| pem_error(ca, e))?)
                    .map_err(|e| pem_error(ca, e))?;
            }
            // Offered, not demanded: a profile-2 station on the same listener
            // has no certificate, and `Gate` requires one per station.
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                provider,
            )
            .allow_unauthenticated()
            .build()
            .map_err(|e| pem_error(ca, e))?;
            (builder.with_client_cert_verifier(verifier), true)
        }
        None => (builder.with_no_client_auth(), false),
    };
    let config = builder
        .with_single_cert(chain, key_der)
        .map_err(|e| pem_error(cert, e))?;
    Ok(ServerTls::from_config(Arc::new(config), mutual))
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

/// One connection's message budget.
#[derive(Debug)]
struct Budget {
    tokens: bucket::Tokens,
    last_seen: Instant,
}

/// Token-bucket arithmetic in integer milli-tokens — the workspace has no
/// floats (constitution II), and a budget is no reason to start.
mod bucket {
    use tokio::time::Instant;

    #[derive(Debug)]
    pub struct Tokens {
        milli: u64,
        at: Instant,
    }

    impl Tokens {
        pub fn full(per_minute: u32, now: Instant) -> Self {
            Self {
                milli: u64::from(per_minute) * 1000,
                at: now,
            }
        }

        /// Refill for the time since the last take, then take one if there is one.
        pub fn take(&mut self, per_minute: u32, now: Instant) -> bool {
            let capacity = u64::from(per_minute) * 1000;
            let elapsed_ms =
                u64::try_from(now.duration_since(self.at).as_millis()).unwrap_or(u64::MAX);
            // per_minute tokens per 60 000 ms = per_minute milli-tokens per 60 ms.
            let refill = elapsed_ms.saturating_mul(u64::from(per_minute)) / 60;
            self.milli = self.milli.saturating_add(refill).min(capacity);
            self.at = now;
            if self.milli >= 1000 {
                self.milli -= 1000;
                true
            } else {
                false
            }
        }
    }
}

/// The live half of the edge: budgets, idleness and the counters.
#[derive(Debug)]
pub struct Edge {
    per_minute: u32,
    idle: Duration,
    budgets: Mutex<HashMap<Identity, Budget>>,
    /// What was refused.
    pub counters: Counters,
}

impl Edge {
    /// An edge with these limits.
    #[must_use]
    pub fn new(csms: &Csms) -> Self {
        Self {
            per_minute: csms.messages_per_minute.max(1),
            idle: Duration::from_secs(csms.idle_timeout_secs.max(1)),
            budgets: Mutex::new(HashMap::new()),
            counters: Counters::default(),
        }
    }

    fn budgets(&self) -> std::sync::MutexGuard<'_, HashMap<Identity, Budget>> {
        self.budgets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A message arrived: whether it is within the connection's budget.
    fn admit(&self, identity: &Identity) -> bool {
        let now = Instant::now();
        let mut budgets = self.budgets();
        let budget = budgets.entry(identity.clone()).or_insert_with(|| Budget {
            tokens: bucket::Tokens::full(self.per_minute, now),
            last_seen: now,
        });
        budget.last_seen = now;
        budget.tokens.take(self.per_minute, now)
    }

    /// A session opened: its budget starts full and its silence starts now.
    fn opened(&self, identity: &Identity) {
        let now = Instant::now();
        self.budgets().insert(
            identity.clone(),
            Budget {
                tokens: bucket::Tokens::full(self.per_minute, now),
                last_seen: now,
            },
        );
    }

    /// Close every session silent for longer than the idle timeout.
    pub async fn sweep(&self, csms: &CsmsHandle) {
        let now = Instant::now();
        for identity in csms.sessions().await {
            let silent = self
                .budgets()
                .get(&identity)
                .is_none_or(|budget| now.duration_since(budget.last_seen) > self.idle);
            if !silent {
                continue;
            }
            if let Some(session) = csms.session(&identity).await {
                tracing::warn!(station = %identity, idle = ?self.idle, "closing a silent session");
                self.counters.idle_closed.fetch_add(1, Ordering::Relaxed);
                self.budgets().remove(&identity);
                session.shutdown(Duration::from_secs(1)).await;
            }
        }
    }

    /// How often [`Self::sweep`] runs: often enough that a session is closed
    /// within a quarter of the timeout of going silent.
    #[must_use]
    pub fn sweep_every(&self) -> Duration {
        (self.idle / 4).clamp(Duration::from_millis(50), Duration::from_secs(30))
    }

    /// Follow the kit's session events: count its refusals, log the opens.
    pub async fn watch(self: Arc<Self>, csms: CsmsHandle) {
        let mut events = csms.events();
        loop {
            match events.recv().await {
                Ok(SessionEvent::Opened {
                    identity, version, ..
                }) => {
                    self.opened(&identity);
                    tracing::info!(station = %identity, ocpp = %version, "station connected");
                }
                Ok(SessionEvent::Refused {
                    identity,
                    status,
                    remote,
                }) => {
                    let counter = match status {
                        401 => &self.counters.refused_credentials,
                        404 => &self.counters.refused_unknown,
                        503 => &self.counters.refused_capacity,
                        _ => continue,
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                    let station = identity
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    tracing::warn!(%station, %remote, status, "a station's upgrade was refused");
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// The handler behind the budget: an excess message is refused before it
/// reaches `csmsd`, so it is neither journalled nor answered from state.
pub struct Guarded<H> {
    inner: H,
    edge: Arc<Edge>,
}

impl<H> Guarded<H> {
    /// `inner`, behind `edge`'s budget.
    pub const fn new(inner: H, edge: Arc<Edge>) -> Self {
        Self { inner, edge }
    }
}

impl<H: Handler> Handler for Guarded<H> {
    fn on_request(
        &self,
        ctx: Ctx,
        request: IncomingRequest,
    ) -> BoxFuture<'_, Result<Box<RawValue>, CallError>> {
        if self.edge.admit(ctx.identity()) {
            return self.inner.on_request(ctx, request);
        }
        self.edge
            .counters
            .rate_limited
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(station = %ctx.identity(), action = %request.action, "a station is over its message budget");
        Box::pin(async {
            Err(CallError::security(
                "this connection is over its message budget; slow down",
            ))
        })
    }
}

/// Decides whether a station may connect: its provisioning, its version and
/// the credential its profile requires.
pub struct Gate {
    provisioning: Provisioning,
    security: Arc<Security>,
}

impl Gate {
    /// A gate over the fleet.
    #[must_use]
    pub fn new(provisioning: Provisioning, security: Arc<Security>) -> Self {
        Self {
            provisioning,
            security,
        }
    }

    async fn decide(&self, auth: Auth) -> AuthOutcome {
        // An identity nobody provisioned is answered 404 rather than 401, so
        // an operator can tell a typo from a bad password
        // `[OCPP 2.0.1 Part 4 §3.1.1]` — and before any hash is computed.
        let Some(point) = self.provisioning.get(&auth.identity).cloned() else {
            return AuthOutcome::Unknown;
        };
        let refuse = |why: &str| {
            tracing::warn!(station = %auth.identity, remote = %auth.remote, profile = %auth.profile, "{why}");
            AuthOutcome::Reject
        };
        if !auth.offered.contains(&point.ocpp) {
            return refuse("the station does not offer the OCPP version it was provisioned for");
        }
        let Some(credential) = self.security.credentials.get(auth.identity.as_str()) else {
            return refuse("the station has no credential provisioned");
        };
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
                    AuthOutcome::Accept(SessionContext::new(point))
                } else {
                    refuse("the station's password does not match its provisioned hash")
                }
            }
            (Credential::Password { .. }, _) => refuse("the station sent no Basic password"),
            (Credential::Certificate, Credentials::ClientCertificate { der }) => {
                match common_name(&der) {
                    Some(name) if name == auth.identity.as_str() => {
                        AuthOutcome::Accept(SessionContext::new(point))
                    }
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

    #[test]
    fn sr_043_the_budget_refills_and_refuses_past_it() {
        let start = Instant::now();
        let mut tokens = bucket::Tokens::full(2, start);
        assert!(tokens.take(2, start));
        assert!(tokens.take(2, start));
        assert!(!tokens.take(2, start), "the third in the same instant");
        assert!(
            tokens.take(2, start + Duration::from_secs(30)),
            "one more after half a minute"
        );
    }
}
