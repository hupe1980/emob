//! The estate: one file every daemon reads its own sections of.
//!
//! # One file, many readers, each reading only what it uses
//!
//! An estate is the parties an operator acts as, the stations it runs, the
//! meter keys that make their energy billable, the tariffs that price it, the
//! partners it settles with, and who may look. Before this module each daemon
//! hard-coded its share — `CP-1`, `DE*ABC`, a 0.49 €/kWh tariff — and an
//! estate was eight copies of a guess.
//!
//! So a daemon declares a struct of the sections it reads, made of the section
//! types below, and [`load`] deserialises the whole text into it. A section the
//! daemon does not name is not read, so a malformed tariff does not stop `empd`;
//! a section it does name is read whole, with `deny_unknown_fields`, so a typo
//! in a key is a refusal rather than a default — refuse rather than guess.
//!
//! # A refusal names the key
//!
//! The error is `toml`'s own, which carries the line, the column and the key —
//! and the identifiers inside a section deserialise through their validating
//! constructors (`PartyId`, `EvseId`), so `evse = "nonsense"` is refused at its
//! line. Checks that need a domain crate (a meter key's curve, a tariff's
//! shape) are the daemon's, and name `section[index].field` themselves.
//!
//! # A secret is never a value here
//!
//! Every field that holds a credential ends in `_env` and holds the **name** of
//! an environment variable ([`EnvRef`]). The file is the reviewed artefact and
//! goes into an image; a secret written into it is in every copy of both.

use core::fmt;
use std::path::{Path, PathBuf};

use emob_core::{ContractId, EvseId, PartyId};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::config::Secret;

/// The environment variable naming the estate file.
pub const PATH_VARIABLE: &str = "EMOB_ESTATE";

/// Where the estate is: `EMOB_ESTATE`, or `estate.toml` in the working
/// directory.
#[must_use]
pub fn path() -> PathBuf {
    std::env::var_os(PATH_VARIABLE).map_or_else(|| PathBuf::from("estate.toml"), PathBuf::from)
}

/// Why an estate could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EstateError {
    /// The file could not be read.
    #[error(
        "the estate at {path} could not be read ({source}); set {PATH_VARIABLE} to the estate file"
    )]
    Unreadable {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A section this daemon reads does not parse.
    #[error("the estate at {path} is not one this daemon can run on: {detail}")]
    Malformed {
        /// Which file.
        path: String,
        /// What the parser said — line, column and key.
        detail: String,
    },
    /// A value parsed and is not acceptable.
    #[error("the estate's {key} is refused: {reason}")]
    Invalid {
        /// The key, as `section[index].field`.
        key: String,
        /// Why.
        reason: String,
    },
    /// A secret the estate names is not set in the environment.
    #[error(
        "the estate's {key} names the environment variable {variable}, which is not set: a \
         secret is never written into the estate, so the deployment has to supply it"
    )]
    Unset {
        /// The variable.
        variable: String,
        /// The key that named it.
        key: String,
    },
}

/// Read the sections `T` declares from the estate at `path`.
///
/// # Errors
///
/// [`EstateError::Unreadable`] and [`EstateError::Malformed`].
pub fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, EstateError> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| EstateError::Unreadable {
        path: display.clone(),
        source,
    })?;
    parse(&text, &display)
}

/// The same, from text — what the tests and an embedding caller use.
///
/// # Errors
///
/// [`EstateError::Malformed`].
pub fn from_str<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, EstateError> {
    parse(text, "<in memory>")
}

fn parse<T: serde::de::DeserializeOwned>(text: &str, path: &str) -> Result<T, EstateError> {
    toml::from_str(text).map_err(|error: toml::de::Error| EstateError::Malformed {
        path: path.to_owned(),
        detail: error.to_string().trim().to_owned(),
    })
}

/// Where secrets are looked up: the process environment in a daemon, a map in
/// a test that starts several daemons in one process.
pub type Env = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The process environment.
#[must_use]
pub fn process_env() -> Env {
    std::sync::Arc::new(|name| std::env::var(name).ok())
}

/// The name of an environment variable that holds a secret.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct EnvRef(String);

impl EnvRef {
    /// A reference to `variable`.
    #[must_use]
    pub fn new(variable: impl Into<String>) -> Self {
        Self(variable.into())
    }

    /// The variable's name.
    #[must_use]
    pub fn variable(&self) -> &str {
        &self.0
    }

    /// Read the secret from the process environment.
    ///
    /// `key` is the estate key that named the variable, for the refusal.
    ///
    /// # Errors
    ///
    /// [`EstateError::Unset`] when the variable is absent or empty — an empty
    /// secret is a verifier that accepts a forger who also sends nothing.
    pub fn resolve(&self, key: &str) -> Result<Secret, EstateError> {
        self.resolve_from(key, |name| std::env::var(name).ok())
    }

    /// The same, in an [`Env`].
    ///
    /// # Errors
    ///
    /// [`EstateError::Unset`].
    pub fn resolve_in(&self, key: &str, env: &Env) -> Result<Secret, EstateError> {
        self.resolve_from(key, |name| env(name))
    }

    /// The same, against a lookup the caller supplies.
    ///
    /// # Errors
    ///
    /// [`EstateError::Unset`].
    pub fn resolve_from(
        &self,
        key: &str,
        lookup: impl FnOnce(&str) -> Option<String>,
    ) -> Result<Secret, EstateError> {
        match lookup(&self.0) {
            Some(value) if !value.is_empty() => Ok(Secret::new(value)),
            _ => Err(EstateError::Unset {
                variable: self.0.clone(),
                key: key.to_owned(),
            }),
        }
    }
}

impl fmt::Debug for EnvRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}", self.0)
    }
}

/// `[parties]` — whom this estate acts as.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parties {
    /// The charge point operator: the party the stations, CDRs and invoices are
    /// issued under.
    pub operator: PartyId,
    /// The e-mobility provider `empd` runs: the party its tokens are issued by.
    pub provider: PartyId,
}

/// The OCPP version a station speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Ocpp {
    /// OCPP 1.6J.
    #[serde(rename = "1.6")]
    V1_6,
    /// OCPP 2.0.1.
    #[serde(rename = "2.0.1")]
    V2_0_1,
    /// OCPP 2.1.
    #[serde(rename = "2.1")]
    V2_1,
}

/// Where a station stands, as a roaming partner and the national access point
/// are told it. Coordinates are exact decimals: they travel as decimal text in
/// OCPI and DATEX II and never become a float here.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// The location's id, unique per operator.
    pub id: String,
    /// What a driver sees.
    pub name: String,
    /// Street.
    pub street: String,
    /// House number.
    pub house_number: String,
    /// Postal code.
    pub postal_code: String,
    /// City.
    pub city: String,
    /// ISO 3166-1 alpha-2.
    pub country: String,
    /// WGS 84 latitude.
    pub latitude: Decimal,
    /// WGS 84 longitude.
    pub longitude: Decimal,
    /// The IANA zone the site's local time is read in.
    pub time_zone: String,
}

/// `[[stations]]` — one charging station and the point it is.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Station {
    /// The OCPP identity it connects under.
    pub identity: String,
    /// The security profile it must connect with
    /// `[OCPP 2.0.1 Part 2 §A00]`: 1 (Basic over plain WebSocket),
    /// 2 (Basic over TLS) or 3 (TLS client certificate). There is no profile 0.
    pub security_profile: SecurityProfile,
    /// Profiles 1 and 2: its Basic password as an Argon2id PHC string
    /// (`csmsd hash-password` prints one). A hash is not a credential, so it is
    /// written here rather than named as an environment variable.
    pub password_hash: Option<String>,
    /// The version it speaks.
    pub ocpp: Ocpp,
    /// The EVSE its sessions are attributed to.
    pub evse: EvseId,
    /// What it can deliver, in kW — the `[AFIR Art. 5(4)]` threshold reads it.
    pub rated_power_kw: Decimal,
    /// The id of the `[[tariffs]]` entry that rates its sessions.
    pub tariff: String,
    /// The connector it offers.
    pub connector: emob_core::ConnectorType,
    /// Where it stands.
    pub location: Location,
    /// How many minutes (1–15) the station's display shows the receipt link
    /// after a session — `csmsd` sends it as `SetDisplayMessage`
    /// `[OCPP 2.0.1 Part 2 §O]` (spec 011). Absent is off: a screen is public,
    /// and the next person at it can read the link.
    #[serde(default)]
    pub receipt_display_minutes: Option<u8>,
}

/// The OCPP security profile a station is provisioned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u8")]
pub enum SecurityProfile {
    /// 1 — HTTP Basic over a plain WebSocket: a trusted network only.
    Basic,
    /// 2 — HTTP Basic over TLS.
    TlsBasic,
    /// 3 — a TLS client certificate.
    TlsClientCertificate,
}

impl TryFrom<u8> for SecurityProfile {
    type Error = String;
    fn try_from(number: u8) -> Result<Self, String> {
        match number {
            1 => Ok(Self::Basic),
            2 => Ok(Self::TlsBasic),
            3 => Ok(Self::TlsClientCertificate),
            other => Err(format!(
                "security_profile {other} is not 1, 2 or 3: a station that authenticates with \
                 nothing is not one this CSMS admits"
            )),
        }
    }
}

impl SecurityProfile {
    /// The profile number.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Basic => 1,
            Self::TlsBasic => 2,
            Self::TlsClientCertificate => 3,
        }
    }
}

/// `[csms]` — how `csmsd`'s OCPP listener is exposed.
///
/// Every path is relative to the estate file (see [`Csms::path`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Csms {
    /// The server certificate chain, PEM.
    pub tls_cert: Option<PathBuf>,
    /// Its private key, PEM.
    pub tls_key: Option<PathBuf>,
    /// The CA a profile-3 station's certificate must chain to, PEM.
    pub client_ca: Option<PathBuf>,
    /// TLS is terminated in front of `csmsd` by a proxy the operator runs.
    #[serde(default)]
    pub tls_terminated_by_proxy: bool,
    /// The most simultaneous station sessions.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// The most messages one connection may send per minute.
    #[serde(default = "default_messages_per_minute")]
    pub messages_per_minute: u32,
    /// How long a session may stay silent before it is closed.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// The directory the estate file is in, which relative paths are read
    /// against. Set by the daemon that loads the file; empty (the working
    /// directory) for an estate parsed from text.
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Default for Csms {
    fn default() -> Self {
        Self {
            tls_cert: None,
            tls_key: None,
            client_ca: None,
            tls_terminated_by_proxy: false,
            max_connections: default_max_connections(),
            messages_per_minute: default_messages_per_minute(),
            idle_timeout_secs: default_idle_timeout_secs(),
            dir: PathBuf::new(),
        }
    }
}

impl Csms {
    /// A configured path, read against the estate's directory.
    #[must_use]
    pub fn path(&self, configured: &Path) -> PathBuf {
        if configured.is_absolute() {
            configured.to_owned()
        } else {
            self.dir.join(configured)
        }
    }
}

const fn default_max_connections() -> usize {
    1024
}

const fn default_messages_per_minute() -> u32 {
    120
}

// Three missed heartbeats at the 300 s interval `csmsd` answers a boot with.
const fn default_idle_timeout_secs() -> u64 {
    900
}

/// `[[meter_keys]]` — a meter's public key, out of band, with where it came
/// from `[OCMF §Relation of Serial Numbers]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterKey {
    /// The meter serial (`MS`).
    pub meter: String,
    /// The gateway serial (`GS`), where the key is bound to the pair.
    pub gateway: Option<String>,
    /// The curve, as `[OCMF Tab. 22]` names it (`secp256r1`, …).
    pub curve: String,
    /// The public key, hex — SEC1 or `SubjectPublicKeyInfo`.
    pub key: String,
    /// From when the key is valid (RFC 3339); unbounded when absent.
    pub valid_from: Option<String>,
    /// Until when (RFC 3339, exclusive); unbounded when absent.
    pub valid_until: Option<String>,
    /// Where the key came from — a type approval, a cabinet reading.
    pub provenance: String,
}

/// `[[tariffs]]` — one flat ad-hoc energy tariff.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TariffSpec {
    /// Its id.
    pub id: String,
    /// ISO 4217.
    pub currency: emob_core::Currency,
    /// The IANA zone its times are read in (D175).
    pub time_zone: String,
    /// Gross price per kWh, VAT included — the figure a driver is shown
    /// `[PAngV §3(1)]`; [`TariffSpec::vat`] is the rate inside it.
    pub energy: Decimal,
    /// The VAT rate inside [`TariffSpec::energy`], in percent.
    pub vat: Decimal,
}

impl TariffSpec {
    /// The tariff that rates — the one object `csmsd` prices with and
    /// `tarifd` publishes, so the price shown is the price billed.
    ///
    /// # Errors
    ///
    /// [`EstateError::Invalid`] naming `tariffs[index].<field>`.
    pub fn to_tariff(&self, index: usize) -> Result<emob_tariff::Tariff, EstateError> {
        let at = |field: &str| format!("tariffs[{index}].{field}");
        let invalid = |field: &str, reason: String| EstateError::Invalid {
            key: at(field),
            reason,
        };
        let id = self
            .id
            .parse()
            .map_err(|e: emob_core::IdError| invalid("id", e.to_string()))?;
        let zone = emob_core::TimeZone::new(&self.time_zone)
            .map_err(|e| invalid("time_zone", e.to_string()))?;
        Ok(emob_tariff::Tariff::simple(
            id,
            self.currency,
            emob_tariff::TariffKind::AdHoc,
            zone,
            vec![
                emob_tariff::PriceComponent::new(emob_tariff::Dimension::Energy, self.energy)
                    .with_vat(self.vat),
            ],
        ))
    }
}

/// The party details an invoice needs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Billing {
    /// Legal name.
    pub name: String,
    /// Street and number.
    pub street: Option<String>,
    /// Postal code.
    pub post_code: Option<String>,
    /// City.
    pub city: String,
    /// ISO 3166-1 alpha-2.
    pub country: String,
    /// VAT identifier.
    pub vat_id: String,
    /// The e-mail address an e-invoice is addressed to (BT-34 / BT-49,
    /// scheme `EM`), which `XRechnung` requires of both parties.
    #[serde(default)]
    pub electronic_address: Option<String>,
}

/// The EN 16931 specification a partner's invoices are written in (BT-24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvoiceSpecification {
    /// EN 16931 itself — what `[UStG §14(1)]` names for a business recipient.
    #[default]
    En16931,
    /// `XRechnung` 3.0 — a German public buyer, or a partner that asks for it.
    Xrechnung,
}

/// A SEPA direct-debit mandate a partner signed (BT-89, BT-91).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MandateSeed {
    /// The mandate reference.
    pub reference: String,
    /// The account debited.
    pub iban: String,
}

/// The seller's contact on an invoice (BG-6), which `XRechnung` requires.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceContact {
    /// A name or department.
    pub name: String,
    /// A telephone number.
    pub phone: String,
    /// An e-mail address.
    pub email: String,
}

/// The syntax a delivered invoice is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvoiceSyntax {
    /// OASIS UBL 2.1.
    #[default]
    Ubl,
    /// UN/CEFACT CII D16B.
    Cii,
}

/// `[billing.delivery]` — the operator system (ERP, Peppol provider) every
/// issued invoice is handed to, signed (spec 010).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceDelivery {
    /// Where invoices are posted: `https://`, or `http://` on loopback only.
    pub url: String,
    /// The variable holding the Standard Webhooks secret (`whsec_…`).
    pub secret_env: EnvRef,
    /// The syntax posted.
    #[serde(default)]
    pub syntax: InvoiceSyntax,
}

/// `[[partners]]` — a provider this operator settles with.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Partner {
    /// Its party id; a contract whose issuer this is routes here.
    pub party: PartyId,
    /// The variable holding the Token A handed to it out of band
    /// `[OCPI 2.3.0 §credentials]`.
    pub token_a_env: EnvRef,
    /// Whether it requires the signed data on every CDR.
    #[serde(default)]
    pub signed_data: bool,
    /// The window it settles within, in days.
    pub settles_within_days: Option<i64>,
    /// Whom `billd` invoices.
    pub billing: Billing,
    /// The specification its invoices are written in.
    #[serde(default)]
    pub specification: InvoiceSpecification,
    /// BT-10 — its reference for our invoices (a Leitweg-ID for a public buyer).
    #[serde(default)]
    pub buyer_reference: Option<String>,
    /// Its direct-debit mandate; without one, it pays by transfer.
    #[serde(default)]
    pub mandate: Option<MandateSeed>,
}

/// `[billing]` — the operator's own invoicing identity.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingSection {
    /// The invoice number series prefix.
    pub series: String,
    /// The seller on every invoice.
    pub seller: Billing,
    /// The standard VAT rate, in percent, where the energy is supplied.
    pub vat_rate: Decimal,
    /// The operator's account, paid into by transfer (BT-84).
    #[serde(default)]
    pub iban: Option<String>,
    /// The operator's SEPA creditor identifier (BT-90), for a mandate.
    #[serde(default)]
    pub creditor_id: Option<String>,
    /// The seller's contact.
    #[serde(default)]
    pub contact: Option<InvoiceContact>,
    /// The origin `billd`'s invoice pages are reached at: `<public_url>/i/<token>`.
    #[serde(default)]
    pub public_url: Option<String>,
    /// The operator system invoices are handed to.
    #[serde(default)]
    pub delivery: Option<InvoiceDelivery>,
}

/// One token on a seeded contract.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenSeed {
    /// The uid the card or app presents.
    pub uid: String,
    /// `RFID`, `APP_USER`, `AD_HOC_USER`, `OTHER`.
    #[serde(rename = "type")]
    pub token_type: String,
    /// `ALWAYS`, `ALLOWED`, `ALLOWED_OFFLINE`, `NEVER`.
    pub whitelist: String,
    /// Whether it is blocked.
    #[serde(default)]
    pub blocked: bool,
}

/// `[[contracts]]` — a driver contract `empd` seeds once.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractSeed {
    /// The contract id (eMAID).
    pub id: ContractId,
    /// The day it starts, `YYYY-MM-DD`.
    pub from: String,
    /// Its tokens.
    #[serde(default)]
    pub tokens: Vec<TokenSeed>,
}

/// `[[operators]]` — who may read the read models.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operator {
    /// Who this is: the name every journal record and log line this operator
    /// causes carries (`operator:<name>`), 1 to 64 of `[A-Za-z0-9*._:=+|@ -]`.
    pub name: String,
    /// The variable holding the bearer token.
    pub token_env: EnvRef,
}

/// `[services]` — where the daemons reach each other, and how.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Services {
    /// `billd`'s base URL.
    pub billd: String,
    /// `roamd`'s base URL.
    pub roamd: String,
    /// `empd`'s base URL.
    pub empd: String,
    /// The variable holding the Standard Webhooks secret (`whsec_…`).
    pub webhook_secret_env: EnvRef,
    /// The variable holding the token `csmsd` presents to `empd`.
    pub service_token_env: EnvRef,
    /// How long `csmsd` waits for `empd` before answering `Invalid`.
    #[serde(default = "default_authorize_timeout_ms")]
    pub authorize_timeout_ms: u64,
    /// How often an outbox or a roaming queue is retried.
    #[serde(default = "default_retry_secs")]
    pub retry_secs: u64,
}

const fn default_authorize_timeout_ms() -> u64 {
    2000
}

const fn default_retry_secs() -> u64 {
    5
}

/// `[desk]` — what the operator page reads, and with which token.
///
/// `[witness]` — what the witness trusts (spec 008).
///
/// `keys` names a daemon's head-verifier key (`emob/<daemon>+<8 hex>+<base64>`,
/// what `GET /v1/<daemon>/head` sends in `Emob-Verifier-Key`). A daemon not
/// named is pinned the first time the witness sees it — trust on first use,
/// which Heute says.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Witness {
    /// Daemon name → verifier key.
    #[serde(default)]
    pub keys: std::collections::BTreeMap<String, String>,
}

/// `billd` and `roamd` are reached at their `[services]` addresses; `csmsd`'s
/// shell is named here because no other daemon calls it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Desk {
    /// `csmsd`'s shell base URL (its read model, not its OCPP socket).
    pub csmsd: String,
    /// The variable holding the operator token `deskd` reads the read models
    /// with — one of the `[[operators]]` tokens.
    pub token_env: EnvRef,
    /// The IANA zone the page tells the time and "today" in.
    pub time_zone: String,
}

/// `[roaming]` — `roamd`'s face to its partners.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roaming {
    /// Where partners reach this node's OCPI root (ending in `/ocpi`).
    pub public_url: String,
    /// Hosts the credentials reach-back may contact over `http` or at a
    /// private, loopback or link-local address. Empty in production.
    #[serde(default)]
    pub reach_back_allow: Vec<String>,
}

/// Resolve the webhook secret the estate names, in the Standard Webhooks
/// spelling (`whsec_<base64>`).
///
/// # Errors
///
/// [`EstateError::Unset`], or [`EstateError::Invalid`] for a value that is not
/// `whsec_<base64>`.
pub fn webhook_secret(
    services: &Services,
    env: &Env,
) -> Result<crate::webhook::Secret, EstateError> {
    let key = "services.webhook_secret_env";
    let secret = services.webhook_secret_env.resolve_in(key, env)?;
    crate::webhook::Secret::standard(secret.expose()).map_err(|error| EstateError::Invalid {
        key: key.to_owned(),
        reason: format!("{} holds {error}", services.webhook_secret_env.variable()),
    })
}

/// Resolve every operator's token.
///
/// # Errors
///
/// [`EstateError::Unset`] for the first operator whose variable is missing,
/// and [`EstateError::Invalid`] for a name that cannot be an
/// [`crate::Actor`] — it is what every record the operator causes will say.
pub fn operator_tokens(
    operators: &[Operator],
    env: &Env,
) -> Result<crate::authority::Operators, EstateError> {
    let mut tokens = Vec::with_capacity(operators.len());
    for (index, operator) in operators.iter().enumerate() {
        let actor =
            crate::Actor::operator(&operator.name).map_err(|error| EstateError::Invalid {
                key: format!("operators[{index}].name"),
                reason: error.to_string(),
            })?;
        let secret = operator
            .token_env
            .resolve_in(&format!("operators[{index}].token_env"), env)?;
        tokens.push((actor, crate::authority::Token::new(secret.expose())));
    }
    Ok(crate::authority::Operators::new(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESTATE: &str = r#"
[parties]
operator = "DE*ABC"
provider = "DE*MSP"

[[stations]]
identity = "CP-1"
ocpp = "1.6"
security_profile = 1
password_hash = "$argon2id$v=19$m=19456,t=2,p=1$q1iothavEHNDTeGVTzaU5A$FPoYufVFH6pj8V/ENPoPB3jm497gMXbSv9Y82oKyfYA"
evse = "DE*ABC*E00001"
rated_power_kw = "150"
tariff = "ad-hoc"
connector = "iec62196_t2_combo"
location = { id = "LOC-1", name = "Depot", street = "Hauptstr.", house_number = "1", city = "Berlin", postal_code = "10115", country = "DE", latitude = "52.520008", longitude = "13.404954", time_zone = "Europe/Berlin" }

[[tariffs]]
id = "ad-hoc"
currency = "EUR"
time_zone = "Europe/Berlin"
energy = "0.49"
vat = "19"

[services]
billd = "http://127.0.0.1:9584"
roamd = "http://127.0.0.1:9583"
empd = "http://127.0.0.1:9585"
webhook_secret_env = "EMOB_WEBHOOK_SECRET"
service_token_env = "EMOB_SERVICE_TOKEN"
"#;

    #[derive(Debug, Deserialize)]
    struct Csms {
        parties: Parties,
        stations: Vec<Station>,
        tariffs: Vec<TariffSpec>,
        services: Services,
    }

    #[derive(Debug, Deserialize)]
    struct OnlyParties {
        parties: Parties,
    }

    #[test]
    fn a_daemon_reads_the_sections_it_names() {
        let estate: Csms = from_str(ESTATE).unwrap();
        assert_eq!(estate.parties.operator.to_string(), "DE*ABC");
        assert_eq!(estate.stations[0].ocpp, Ocpp::V1_6);
        assert_eq!(estate.stations[0].rated_power_kw, Decimal::from(150));
        assert_eq!(estate.tariffs[0].energy.to_string(), "0.49");
        assert_eq!(estate.services.authorize_timeout_ms, 2000, "the default");
        assert_eq!(estate.services.retry_secs, 5);
    }

    #[test]
    fn a_section_a_daemon_does_not_read_cannot_stop_it() {
        // A broken tariff is csmsd's problem, not empd's.
        let broken = ESTATE.replace(r#"energy = "0.49""#, r#"energy = "cheap""#);
        assert!(from_str::<Csms>(&broken).is_err());
        let estate: OnlyParties = from_str(&broken).expect("parties alone still parse");
        assert_eq!(estate.parties.provider.to_string(), "DE*MSP");
    }

    #[test]
    fn sr_036_a_bad_identifier_is_refused_naming_its_key() {
        let broken = ESTATE.replace(r#"evse = "DE*ABC*E00001""#, r#"evse = "not an evse""#);
        let error = from_str::<Csms>(&broken).expect_err("an EVSE id that is not one");
        let text = error.to_string();
        assert!(text.contains("evse"), "{text}");
        assert!(text.contains("line"), "the line is named too: {text}");
    }

    #[test]
    fn sr_036_an_unknown_key_is_a_refusal_rather_than_a_default() {
        let broken = ESTATE.replace(
            r#"tariff = "ad-hoc""#,
            "tarif = \"ad-hoc\"\ntariff = \"ad-hoc\"",
        );
        let text = from_str::<Csms>(&broken).expect_err("a typo").to_string();
        assert!(text.contains("tarif"), "{text}");
    }

    #[test]
    fn sr_035_a_secret_is_a_variable_name_and_an_unset_one_refuses() {
        let estate: Csms = from_str(ESTATE).unwrap();
        let reference = &estate.services.webhook_secret_env;
        assert_eq!(reference.variable(), "EMOB_WEBHOOK_SECRET");

        let error = reference
            .resolve_from("services.webhook_secret_env", |_| None)
            .expect_err("unset");
        let text = error.to_string();
        assert!(text.contains("EMOB_WEBHOOK_SECRET"), "{text}");
        assert!(text.contains("services.webhook_secret_env"), "{text}");

        // An empty variable is not a secret either.
        assert!(
            reference
                .resolve_from("k", |_| Some(String::new()))
                .is_err()
        );

        let secret = reference
            .resolve_from("k", |_| Some("whsec_c2VjcmV0".to_owned()))
            .unwrap();
        assert_eq!(format!("{secret:?}"), "Secret(…)", "and it never prints");
    }

    #[test]
    fn the_demo_estate_parses_for_every_reader() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code, reason = "parsed to prove it parses")]
        struct Everything {
            parties: Parties,
            stations: Vec<Station>,
            meter_keys: Vec<MeterKey>,
            tariffs: Vec<TariffSpec>,
            partners: Vec<Partner>,
            billing: BillingSection,
            contracts: Vec<ContractSeed>,
            operators: Vec<Operator>,
            services: Services,
            roaming: Roaming,
            csms: super::Csms,
        }
        for text in [
            include_str!("../../../deploy/estate.example.toml"),
            include_str!("../../../deploy/estate.compose.toml"),
        ] {
            let estate: Everything =
                from_str(text).expect("the demo estate is one every daemon runs on");
            assert!(!estate.meter_keys.is_empty());
            assert!(!estate.operators.is_empty());
            // SC-002: the demo runs with profile 2 and TLS on.
            assert_eq!(
                estate.stations[0].security_profile,
                SecurityProfile::TlsBasic
            );
            assert!(estate.csms.tls_cert.is_some() && !estate.csms.tls_terminated_by_proxy);
        }
    }

    #[test]
    fn sr_042_a_station_with_no_profile_or_profile_0_is_refused() {
        let missing = ESTATE.replace("security_profile = 1\n", "");
        let text = from_str::<Csms>(&missing)
            .expect_err("no profile")
            .to_string();
        assert!(text.contains("security_profile"), "{text}");
        let zero = ESTATE.replace("security_profile = 1", "security_profile = 0");
        let text = from_str::<Csms>(&zero).expect_err("profile 0").to_string();
        assert!(text.contains("not 1, 2 or 3"), "{text}");
    }
}
