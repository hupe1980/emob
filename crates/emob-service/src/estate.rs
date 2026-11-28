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
    #[error("the estate at {path} is not one this daemon can run on:\n{detail}")]
    Malformed {
        /// Which file.
        path: String,
        /// The refusal as `rustc` would print it: what, where, and what
        /// would be valid ([`report::render`]).
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

/// The same, from text read from `path` — so a refusal names the file.
///
/// # Errors
///
/// [`EstateError::Malformed`].
pub fn from_str_at<T: serde::de::DeserializeOwned>(
    text: &str,
    path: &str,
) -> Result<T, EstateError> {
    parse(text, path)
}

fn parse<T: serde::de::DeserializeOwned>(text: &str, path: &str) -> Result<T, EstateError> {
    toml::from_str(text).map_err(|error: toml::de::Error| EstateError::Malformed {
        path: path.to_owned(),
        detail: report::render(path, text, error.span(), error.message()),
    })
}

pub mod report;

/// The whole estate: every section any daemon reads (spec 012).
///
/// The daemons keep their own narrower views — a malformed tariff does not
/// stop `empd` — and this one is what `emob check` and the published JSON
/// Schema describe. A section no daemon reads is refused here, so
/// `[[subscriber]]` for `[[subscribers]]` is a refusal and not a silence.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Estate {
    /// Whom the platform acts as.
    pub parties: Parties,
    /// The fleet.
    #[serde(default)]
    pub stations: Vec<Station>,
    /// The keys that make energy billable.
    #[serde(default)]
    pub meter_keys: Vec<MeterKey>,
    /// The tariffs stations name.
    #[serde(default)]
    pub tariffs: Vec<TariffSpec>,
    /// Explicitly approved public provider plans (AFIR Art. 5(5)).
    #[serde(default)]
    pub public_plans: Vec<PublicPlan>,
    /// The roaming partners.
    #[serde(default)]
    pub partners: Vec<Partner>,
    /// How invoices are issued (`billd`).
    #[serde(default)]
    pub billing: Option<BillingSection>,
    /// Driver contracts `empd` seeds.
    #[serde(default)]
    pub contracts: Vec<ContractSeed>,
    /// Customer sign-in subjects. Grants are held in the provider journal.
    #[serde(default)]
    pub portal_users: Vec<PortalUser>,
    /// Private bill-to accounts available for native enrolment.
    #[serde(default)]
    pub retail_accounts: Vec<RetailAccount>,
    /// Unassigned provider RFID cards available for native enrolment.
    #[serde(default)]
    pub card_stock: Vec<CardStock>,
    /// Who may read and act.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The OCPP listener.
    #[serde(default)]
    pub csms: Option<Csms>,
    /// The operator page.
    #[serde(default)]
    pub desk: Option<Desk>,
    /// Public origin of the customer portal.
    #[serde(default)]
    pub portal: Option<Portal>,
    /// Where receipt links point.
    #[serde(default)]
    pub receipts: Option<ReceiptSection>,
    /// Where the daemons are.
    #[serde(default)]
    pub services: Option<Services>,
    /// The roaming node.
    #[serde(default)]
    pub roaming: Option<Roaming>,
    /// The keys journal heads are signed under.
    #[serde(default)]
    pub witness: Witness,
    /// Systems outside the estate that receive events.
    #[serde(default)]
    pub subscribers: Vec<Subscriber>,
    /// Registered Mobilithek offers for the static DATEX II table and dynamic updates.
    #[serde(default)]
    pub mobilithek: Option<Mobilithek>,
}

/// The origin on which customers use their portal.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Portal {
    /// External HTTPS origin, or an HTTP loopback origin for a local deployment.
    pub public_url: String,
}

/// `[mobilithek]` configures the separate static and dynamic offers that
/// `poid` delivers to the national access point. See `[AFIR Art. 20(2)]`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Mobilithek {
    /// Maximum age of a connector report before the feed marks availability unknown.
    #[serde(default = "default_status_max_age_secs")]
    pub status_max_age_secs: u32,
    /// Push URL of the separately registered dynamic offer, linked to the static offer.
    pub dynamic_url: String,
    /// The publication's push address the Mobilithek issued — `https://`,
    /// or `http://` on loopback for a mock.
    pub url: String,
    /// The client certificate (PEM) the Mobilithek authenticates the
    /// operator by; required for `https://`.
    #[serde(default)]
    pub client_cert: Option<String>,
    /// Its private key (PEM).
    #[serde(default)]
    pub client_key: Option<String>,
}

const fn default_status_max_age_secs() -> u32 {
    86_400
}

/// `[receipts]` — read by `csmsd`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReceiptSection {
    /// The origin a driver reaches the shell at; links are
    /// `<public_url>/r/<token>`.
    pub public_url: String,
}

/// The JSON Schema of [`Estate`], as `schemas/estate.schema.json` holds it.
#[cfg(feature = "schema")]
#[must_use]
pub fn json_schema() -> serde_json::Value {
    let mut schema =
        serde_json::to_value(schemars::schema_for!(Estate)).unwrap_or(serde_json::Value::Null);
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "$id".to_owned(),
            "https://hupe1980.github.io/emob/schemas/estate.schema.json".into(),
        );
        object.insert("title".to_owned(), "emob estate".into());
    }
    schema
}

#[cfg(feature = "schema")]
mod schema {
    //! Schemas for the section fields whose type is not this crate's.

    use schemars::{Schema, SchemaGenerator, json_schema};

    /// `[[stations]] connector`, in the spelling the estate accepts.
    pub fn connector(_: &mut SchemaGenerator) -> Schema {
        use emob_core::ConnectorType as C;
        let names: Vec<serde_json::Value> = [
            C::Iec62196T2,
            C::Iec62196T2Combo,
            C::Iec62196T1,
            C::Iec62196T1Combo,
            C::Chademo,
            C::Iec62196T3A,
            C::DomesticF,
            C::Cee5,
        ]
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
        json_schema!({ "type": "string", "enum": names })
    }

    /// `[[stations]] security_profile`: OCPP security profile 1, 2 or 3.
    pub fn security_profile(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "integer", "minimum": 1, "maximum": 3 })
    }
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Parties {
    /// The charge point operator: the party the stations, CDRs and invoices are
    /// issued under.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^[A-Z]{2}\*?[A-Z0-9]{3}$"))
    )]
    pub operator: PartyId,
    /// The e-mobility provider `empd` runs: the party its tokens are issued by.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^[A-Z]{2}\*?[A-Z0-9]{3}$"))
    )]
    pub provider: PartyId,
}

/// The OCPP version a station speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^-?\d+(\.\d+)?$"))
    )]
    pub latitude: Decimal,
    /// WGS 84 longitude.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^-?\d+(\.\d+)?$"))
    )]
    pub longitude: Decimal,
    /// The IANA zone the site's local time is read in.
    pub time_zone: String,
}

/// `[[stations]]` — one charging station and the point it is.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Station {
    /// Whether this point may appear in public price and infrastructure publications.
    #[serde(default)]
    pub publicly_accessible: bool,
    /// Operator declaration that the electricity supplied is 100% renewable.
    pub fully_renewable: Option<bool>,
    /// The OCPP identity it connects under.
    pub identity: String,
    /// The security profile it must connect with
    /// `[OCPP 2.0.1 Part 2 §A00]`: 1 (Basic over plain WebSocket),
    /// 2 (Basic over TLS) or 3 (TLS client certificate). There is no profile 0.
    #[cfg_attr(feature = "schema", schemars(schema_with = "schema::security_profile"))]
    pub security_profile: SecurityProfile,
    /// Profiles 1 and 2: its Basic password as an Argon2id PHC string
    /// (`csmsd hash-password` prints one). A hash is not a credential, so it is
    /// written here rather than named as an environment variable.
    pub password_hash: Option<String>,
    /// The version it speaks.
    pub ocpp: Ocpp,
    /// The EVSE its sessions are attributed to.
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(pattern = r"^[A-Z]{2}\*?[A-Z0-9]{3}\*?E[A-Z0-9*]{1,30}$")
        )
    )]
    pub evse: EvseId,
    /// What it can deliver, in kW — the `[AFIR Art. 5(4)]` threshold reads it.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^-?\d+(\.\d+)?$"))
    )]
    pub rated_power_kw: Decimal,
    /// The id of the `[[tariffs]]` entry that rates its sessions.
    pub tariff: String,
    /// The connector it offers.
    #[cfg_attr(feature = "schema", schemars(schema_with = "schema::connector"))]
    pub connector: emob_core::ConnectorType,
    /// Where it stands.
    pub location: Location,
    /// How many minutes (1–15) the station's display shows the receipt link
    /// after a session — `csmsd` sends it as `SetDisplayMessage`
    /// `[OCPP 2.0.1 Part 2 §O]` (spec 011). Absent is off: a screen is public,
    /// and the next person at it can read the link.
    #[serde(default)]
    pub receipt_display_minutes: Option<u8>,
    /// The day it went into service, `YYYY-MM-DD` — the asset card's age.
    #[serde(default)]
    pub commissioned: Option<String>,
    /// Whether its display shows the price and the Preisstand — and on OCPP
    /// 2.1 the price page's code — sent after every boot, one message per
    /// tariff version over that version's window (spec 015). OCPP 1.6 has no
    /// display message.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub show_price: bool,
}

/// The OCPP security profile a station is provisioned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(try_from = "u8", into = "u8")]
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

impl From<SecurityProfile> for u8 {
    fn from(profile: SecurityProfile) -> Self {
        profile.number()
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TariffSpec {
    /// Its id.
    pub id: String,
    /// ISO 4217.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^[A-Z]{3}$"))
    )]
    pub currency: emob_core::Currency,
    /// The IANA zone its times are read in (D175).
    pub time_zone: String,
    /// Gross price per kWh, VAT included — the figure a driver is shown
    /// `[PAngV §3(1)]`; [`TariffSpec::vat`] is the rate inside it.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^-?\d+(\.\d+)?$"))
    )]
    pub energy: Decimal,
    /// The VAT rate inside every price of this version, in percent.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^-?\d+(\.\d+)?$"))
    )]
    pub vat: Decimal,
    /// Gross price per minute of charging, when the tariff has one.
    #[serde(default)]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", regex(pattern = r"^\d+(\.\d+)?$"))
    )]
    pub per_minute: Option<Decimal>,
    /// Gross fee per minute connected while the vehicle is not requesting energy.
    #[serde(default)]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", regex(pattern = r"^\d+(\.\d+)?$"))
    )]
    pub occupancy_per_minute: Option<Decimal>,
    /// Gross price per session, when the tariff has one.
    #[serde(default)]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", regex(pattern = r"^\d+(\.\d+)?$"))
    )]
    pub session_fee: Option<Decimal>,
    /// From when this version is in force (RFC 3339, on a quarter hour) —
    /// spec 015. A tariff id may appear once per version; the version without
    /// one has always been in force, and each version ends where the next
    /// begins.
    #[serde(default)]
    pub valid_from: Option<String>,
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
        let mut components = vec![
            emob_tariff::PriceComponent::new(emob_tariff::Dimension::Energy, self.energy)
                .with_vat(self.vat),
        ];
        if let Some(per_minute) = self.per_minute {
            // Stored per hour, as OCPI carries it; shown per minute again by
            // the display, exactly.
            components.push(
                emob_tariff::PriceComponent::new(
                    emob_tariff::Dimension::Time,
                    per_minute * Decimal::from(60),
                )
                .with_vat(self.vat),
            );
        }
        if let Some(fee) = self.occupancy_per_minute {
            let component = emob_tariff::PriceComponent::occupancy_per_minute(fee)
                .ok_or_else(|| invalid("occupancy_per_minute", "supply a nonnegative rate that converts exactly to the hourly billing unit".into()))?;
            components.push(component.with_vat(self.vat));
        }
        if let Some(fee) = self.session_fee {
            components.push(
                emob_tariff::PriceComponent::new(emob_tariff::Dimension::Flat, fee)
                    .with_vat(self.vat),
            );
        }
        Ok(emob_tariff::Tariff::simple(
            id,
            self.currency,
            emob_tariff::TariffKind::AdHoc,
            zone,
            components,
        ))
    }

    fn valid_from(&self, index: usize) -> Result<Option<time::OffsetDateTime>, EstateError> {
        self.valid_from
            .as_deref()
            .map(|from| {
                time::OffsetDateTime::parse(from, &time::format_description::well_known::Rfc3339)
                    .map_err(|e| EstateError::Invalid {
                        key: format!("tariffs[{index}].valid_from"),
                        reason: format!(
                            "{from} is not RFC 3339 like 2027-01-01T00:00:00+01:00 ({e})"
                        ),
                    })
            })
            .transpose()
    }
}

/// Every tariff's history by id — the one function `csmsd` prices with and
/// `tarifd` publishes from, so the version a driver is shown is the version a
/// session is rated with (spec 015).
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `tariffs[index]` for a version that does
/// not build, two versions of one id without a `valid_from` between them, or a
/// history `emob-tariff` refuses (a change off the quarter-hour grid).
pub fn histories(
    specs: &[TariffSpec],
) -> Result<std::collections::BTreeMap<String, emob_tariff::TariffHistory>, EstateError> {
    use std::collections::BTreeMap;
    let mut by_id: BTreeMap<&str, Vec<(usize, Option<time::OffsetDateTime>)>> = BTreeMap::new();
    for (index, spec) in specs.iter().enumerate() {
        by_id
            .entry(spec.id.as_str())
            .or_default()
            .push((index, spec.valid_from(index)?));
    }
    let mut histories = BTreeMap::new();
    for (id, mut versions) in by_id {
        versions.sort_by_key(|(_, from)| *from);
        if let [(_, None), (second, None), ..] = versions[..] {
            return Err(EstateError::Invalid {
                key: format!("tariffs[{second}].valid_from"),
                reason: format!(
                    "{id} has two versions in force from the start; give the later one a valid_from"
                ),
            });
        }
        let mut built = Vec::with_capacity(versions.len());
        for (position, (index, from)) in versions.iter().enumerate() {
            let until = versions.get(position + 1).and_then(|(_, next)| *next);
            built.push(specs[*index].to_tariff(*index)?.valid_between(*from, until));
        }
        let first = versions[0].0;
        let history = emob_tariff::TariffHistory::new(built).map_err(|e| EstateError::Invalid {
            key: format!("tariffs[{first}]"),
            reason: e.to_string(),
        })?;
        histories.insert(id.to_owned(), history);
    }
    Ok(histories)
}

/// The party details an invoice needs.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MandateSeed {
    /// The mandate reference.
    pub reference: String,
    /// The account debited.
    pub iban: String,
}

/// The seller's contact on an invoice (BG-6), which `XRechnung` requires.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

impl InvoiceDelivery {
    /// Resolve the ERP credential, keeping it separate from subscriber and internal keys.
    ///
    /// # Errors
    /// An unset, malformed or shared key cannot identify the operator system.
    pub fn signing_key(
        &self,
        env: &Env,
        subscribers: &[Subscription],
        internal: Option<&crate::webhook::Secret>,
    ) -> Result<crate::webhook::Secret, EstateError> {
        let field = "billing.delivery.secret_env";
        let configured = self.secret_env.resolve_in(field, env)?;
        let invalid = |reason: String| EstateError::Invalid {
            key: field.into(),
            reason,
        };
        let key = crate::webhook::Secret::standard(configured.expose())
            .map_err(|e| invalid(e.to_string()))?;
        if internal == Some(&key) || subscribers.iter().any(|s| s.secret == key) {
            return Err(invalid(
                "the invoice receiver needs its own signing key".into(),
            ));
        }
        Ok(key)
    }
}

/// `[[subscribers]]` — a system outside the estate that receives events.
///
/// Each event a daemon emits whose type one of `types` admits
/// ([`crate::events::matches`]) is posted to `url` as a structured-mode
/// [`crate::cloudevent::CloudEvent`], signed with this subscriber's own secret,
/// and retried until it answers `2xx`. A subscriber added later receives what
/// is emitted from then on.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Subscriber {
    /// Its name; the journal records an answer as `subscriber:<name>`.
    pub name: String,
    /// Where events are posted: `https://`, or `http://` on loopback only.
    pub url: String,
    /// The variable holding its Standard Webhooks secret (`whsec_…`).
    pub secret_env: EnvRef,
    /// The types it takes: concrete types or `*`-terminated prefixes.
    pub types: Vec<String>,
}

/// A subscriber, checked and with its own secret.
#[derive(Debug, Clone)]
pub struct Subscription {
    /// Its name.
    pub name: String,
    /// Where events go.
    pub url: String,
    /// What they are signed with.
    pub secret: crate::webhook::Secret,
    /// What it takes.
    pub types: Vec<String>,
}

impl Subscription {
    /// Whether it takes this type.
    #[must_use]
    pub fn takes(&self, event_type: &str) -> bool {
        self.types
            .iter()
            .any(|pattern| crate::events::matches(pattern, event_type))
    }
}

/// Check every subscriber and resolve its secret.
///
/// # Errors
///
/// [`EstateError::Invalid`] for a name outside the actor alphabet, a name
/// used twice, a target that is not `https` (or `http` on loopback), no
/// types, or a pattern that admits no type in the catalogue — a typo would
/// otherwise subscribe to nothing and say nothing; [`EstateError::Unset`] for
/// a missing secret.
pub fn subscriptions(
    subscribers: &[Subscriber],
    env: &Env,
) -> Result<Vec<Subscription>, EstateError> {
    let mut out: Vec<Subscription> = Vec::with_capacity(subscribers.len());
    for (index, subscriber) in subscribers.iter().enumerate() {
        let invalid = |field: &str, reason: String| EstateError::Invalid {
            key: format!("subscribers[{index}].{field}"),
            reason,
        };
        crate::Actor::subscriber(&subscriber.name)
            .map_err(|error| invalid("name", error.to_string()))?;
        if out.iter().any(|seen| seen.name == subscriber.name) {
            return Err(invalid(
                "name",
                format!("{} is named twice", subscriber.name),
            ));
        }
        if !crate::webhook::secure_target(&subscriber.url) {
            return Err(invalid(
                "url",
                format!(
                    "{} must be https, or http on a loopback host",
                    subscriber.url
                ),
            ));
        }
        if subscriber.types.is_empty() {
            return Err(invalid("types", "names no type".to_owned()));
        }
        for pattern in &subscriber.types {
            if !crate::events::ALL
                .iter()
                .any(|event_type| crate::events::matches(pattern, event_type))
            {
                return Err(invalid(
                    "types",
                    format!(
                        "{pattern} admits no type emob emits ({})",
                        crate::events::ALL.join(", ")
                    ),
                ));
            }
        }
        let key = format!("subscribers[{index}].secret_env");
        let secret = subscriber.secret_env.resolve_in(&key, env)?;
        let secret = crate::webhook::Secret::standard(secret.expose()).map_err(|error| {
            EstateError::Invalid {
                key: key.clone(),
                reason: format!("{} holds {error}", subscriber.secret_env.variable()),
            }
        })?;
        if out.iter().any(|prior| prior.secret == secret) {
            return Err(invalid(
                "secret_env",
                "each subscriber needs its own signing key".to_owned(),
            ));
        }
        out.push(Subscription {
            name: subscriber.name.clone(),
            url: subscriber.url.clone(),
            secret,
            types: subscriber.types.clone(),
        });
    }
    Ok(out)
}

/// Require a separate key for internal deliveries and every external principal.
///
/// # Errors
/// Returns the offending subscriber's name if its key also signs internal events.
pub fn separate_subscriber_keys(
    list: &[Subscription],
    internal: &crate::webhook::Secret,
) -> Result<(), EstateError> {
    if let Some(entry) = list.iter().find(|s| s.secret == *internal) {
        return Err(EstateError::Invalid {
            key: format!("subscribers.{}.secret_env", entry.name),
            reason: "a subscriber must have a different key from the estate delivery key"
                .to_owned(),
        });
    }
    Ok(())
}

/// `[[partners]]` — a provider this operator settles with.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Partner {
    /// Its party id; a contract whose issuer this is routes here.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^[A-Z]{2}\*?[A-Z0-9]{3}$"))
    )]
    pub party: PartyId,
    /// CPO parties this platform may represent when sending or reading charge records.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
    pub cdr_owners: Vec<PartyId>,
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BillingSection {
    /// The invoice number series prefix.
    pub series: String,
    /// The CPO seller on partner invoices.
    pub seller: Billing,
    /// The mobility provider issuing native retail invoices.
    pub retail_seller: Option<Billing>,
    /// The standard VAT rate, in percent, where the energy is supplied.
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", regex(pattern = r"^-?\d+(\.\d+)?$"))
    )]
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

/// The printable number a driver reads from a card, trimmed for exact operator lookup.
#[must_use]
pub fn printed_card_number(number: &str) -> Option<&str> {
    let value = number.trim();
    (!value.is_empty() && number.chars().count() <= 64 && !number.chars().any(char::is_control))
        .then_some(value)
}

/// One token on a seeded contract.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TokenSeed {
    /// Optional printed number for an operator's exact card lookup (1–64 characters).
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 64)))]
    pub visual_number: Option<String>,
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

/// An unissued physical card. Staff enter the inventory code printed on its sleeve.
#[derive(Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CardStock {
    /// Case-sensitive inventory code, unique within this estate.
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[A-Z0-9][A-Z0-9-]{0,31}$"))
    )]
    pub code: String,
    /// Canonical uppercase hexadecimal RFID UID (4, 7 or 10 bytes).
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^([A-F0-9]{8}|[A-F0-9]{14}|[A-F0-9]{20})$"))
    )]
    pub uid: String,
}

impl core::fmt::Debug for CardStock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CardStock").finish_non_exhaustive()
    }
}

/// Check inventory shape and uniqueness before issuing against it.
///
/// # Errors
/// Names the conflicting inventory field without printing card identifiers.
pub fn validate_card_stock(
    stock: &[CardStock],
    contracts: &[ContractSeed],
) -> Result<(), EstateError> {
    use std::collections::BTreeSet;
    let mut codes = BTreeSet::new();
    let mut uids = BTreeSet::new();
    for token in contracts.iter().flat_map(|seed| &seed.tokens) {
        uids.insert(token.uid.to_ascii_uppercase());
        if let Some(number) = &token.visual_number {
            codes.insert(number.trim().to_ascii_uppercase());
        }
    }
    for (i, card) in stock.iter().enumerate() {
        let bad = |field, reason: &str| EstateError::Invalid {
            key: format!("card_stock[{i}].{field}"),
            reason: reason.to_owned(),
        };
        let valid = |c: u8| c.is_ascii_uppercase() || c.is_ascii_digit();
        if card.code.len() > 32
            || !card.code.bytes().next().is_some_and(valid)
            || !card.code.bytes().all(|c| valid(c) || c == b'-')
            || !codes.insert(card.code.clone())
        {
            return Err(bad(
                "code",
                "expected a distinct inventory code of 1–32 uppercase letters, digits or hyphens",
            ));
        }
        if !matches!(card.uid.len(), 8 | 14 | 20)
            || !card
                .uid
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
            || !uids.insert(card.uid.clone())
        {
            return Err(bad(
                "uid",
                "expected a distinct unissued uppercase hexadecimal RFID UID of 4, 7 or 10 bytes",
            ));
        }
    }
    Ok(())
}

/// `[[contracts]]` — a driver contract `empd` seeds once.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ContractSeed {
    /// The contract id (eMAID).
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub id: ContractId,
    /// The day it starts, `YYYY-MM-DD`.
    pub from: String,
    /// Its tokens.
    #[serde(default)]
    pub tokens: Vec<TokenSeed>,
}

/// `[[operators]]` — a named credential for estate reads and optional writes.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Operator {
    /// Who this is: the name every journal record and log line this operator
    /// causes carries (`operator:<name>`), 1 to 64 of `[A-Za-z0-9*._:=+|@ -]`
    /// with no surrounding spaces.
    pub name: String,
    /// The variable holding the bearer token.
    pub token_env: EnvRef,
    /// Permit named operator actions. Omit for read-only access.
    #[serde(default)]
    pub write: bool,
    /// Permit finance reads, statement imports and reviewed allocations.
    #[serde(default)]
    pub finance: bool,
    /// Permit a named reviewer to erase customer portal access bindings.
    #[serde(default)]
    pub privacy: bool,
}

/// `[services]` — where the daemons reach each other, and how.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Services {
    /// `billd`'s base URL.
    pub billd: String,
    /// Optional receivables owner used by the finance desk.
    pub accountingd: Option<String>,
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Witness {
    /// Daemon name → verifier key.
    #[serde(default)]
    pub keys: std::collections::BTreeMap<String, String>,
}

/// `billd` and `roamd` are reached at their `[services]` addresses; `csmsd`'s
/// shell is named here because no other daemon calls it.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Desk {
    /// Optional POI publication service for the operator delivery view.
    pub poid: Option<String>,
    /// `csmsd`'s shell base URL (its read model, not its OCPP socket).
    pub csmsd: String,
    /// The variable holding the operator token `deskd` reads the read models
    /// with — a read-only `[[operators]]` credential.
    pub token_env: EnvRef,
    /// The IANA zone the page tells the time and "today" in.
    pub time_zone: String,
    /// Where stations reach the CSMS, as an installer types it
    /// (`wss://csms.example.com/ocpp`) — printed on the onboarding sheet.
    #[serde(default)]
    pub ocpp_url: Option<String>,
}

/// `[roaming]` — `roamd`'s face to its partners.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
/// and [`EstateError::Invalid`] for an invalid or repeated actor name or a
/// repeated credential value.
pub fn operator_tokens(
    operators: &[Operator],
    env: &Env,
) -> Result<crate::authority::Operators, EstateError> {
    use crate::authority::{OperatorAccess, Token};
    let mut tokens: Vec<(crate::Actor, Token, OperatorAccess)> =
        Vec::with_capacity(operators.len());
    for (index, operator) in operators.iter().enumerate() {
        let actor =
            crate::Actor::operator(&operator.name).map_err(|error| EstateError::Invalid {
                key: format!("operators[{index}].name"),
                reason: error.to_string(),
            })?;
        let secret = operator
            .token_env
            .resolve_in(&format!("operators[{index}].token_env"), env)?;
        if operator.name.trim() != operator.name || tokens.iter().any(|(name, _, _)| name == &actor)
        {
            return Err(EstateError::Invalid {
                key: format!("operators[{index}].name"),
                reason: "every operator needs a distinct name without surrounding spaces"
                    .to_owned(),
            });
        }
        if tokens
            .iter()
            .any(|(_, token, _)| token.verify(secret.expose()))
        {
            return Err(EstateError::Invalid {
                key: format!("operators[{index}].token_env"),
                reason: "each operator credential must have a distinct value".to_owned(),
            });
        }
        let access = OperatorAccess::new(operator.write, operator.finance, operator.privacy);
        tokens.push((actor, Token::new(secret.expose()), access));
    }
    Ok(crate::authority::Operators::from_grants(tokens))
}

/// The read-only credential used for background requests by the operator desk.
///
/// # Errors
///
/// Returns an estate error if the secret is missing, unknown or write-enabled.
pub fn desk_read_credential(
    desk: &Desk,
    env: &Env,
    operators: &crate::Operators,
) -> Result<Secret, EstateError> {
    let key = "desk.token_env";
    let secret = desk.token_env.resolve_in(key, env)?;
    if !matches!(
        operators.identify(secret.expose()),
        Some((_, access)) if access == crate::authority::OperatorAccess::default()
    ) {
        return Err(EstateError::Invalid {
            key: key.to_owned(),
            reason: "the desk needs a configured read-only operator credential".to_owned(),
        });
    }
    Ok(secret)
}

/// The station authorization service credential, separately held from operator bearers.
///
/// # Errors
///
/// Returns an estate error if the secret is missing or belongs to an operator.
pub fn service_credential(
    services: &Services,
    env: &Env,
    operators: &crate::Operators,
) -> Result<Secret, EstateError> {
    let key = "services.service_token_env";
    let secret = services.service_token_env.resolve_in(key, env)?;
    if operators.identify(secret.expose()).is_some() {
        return Err(EstateError::Invalid {
            key: key.to_owned(),
            reason: "the service token must differ from every operator credential".to_owned(),
        });
    }
    Ok(secret)
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
        assert!(
            text.contains(":11:8"),
            "the line and column are named too: {text}"
        );
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

    // heute-acts/SR-037: validation runs on the resolved secrets.
    #[test]
    fn the_desk_and_service_credentials_cannot_impersonate_a_writer() {
        let mut estate: Estate =
            from_str(include_str!("../../../deploy/estate.example.toml")).unwrap();
        let env: Env = std::sync::Arc::new(|name| Some(name.to_owned()));
        let operators = operator_tokens(&estate.operators, &env).unwrap();
        let mut desk = estate.desk.unwrap();
        let mut services = estate.services.unwrap();
        assert!(desk_read_credential(&desk, &env, &operators).is_ok());
        assert!(service_credential(&services, &env, &operators).is_ok());

        for operator in &estate.operators {
            services.service_token_env = operator.token_env.clone();
            assert!(service_credential(&services, &env, &operators).is_err());
            desk.token_env = operator.token_env.clone();
            assert_eq!(
                desk_read_credential(&desk, &env, &operators).is_ok(),
                !operator.write
            );
        }
        desk.token_env = EnvRef("UNKNOWN".to_owned());
        assert!(desk_read_credential(&desk, &env, &operators).is_err());

        let reused: Env = std::sync::Arc::new(|_| Some("shared-test-secret".to_owned()));
        let error = operator_tokens(&estate.operators, &reused)
            .unwrap_err()
            .to_string();
        assert!(error.contains("operators[1].token_env") && !error.contains("shared-test-secret"));
        estate.operators[1].name = " ".to_owned();
        assert!(operator_tokens(&estate.operators, &env).is_err());
        estate.operators[1].name = estate.operators[0].name.clone();
        assert!(
            operator_tokens(&estate.operators, &env)
                .unwrap_err()
                .to_string()
                .contains("operators[1].name")
        );
    }

    #[test]
    fn unissued_inventory_has_unique_codes_and_canonical_unassigned_uids() {
        // native-driver-enrolment/SR-301
        let make = |code: &str, uid: &str| super::CardStock {
            code: code.into(),
            uid: uid.into(),
        };
        let estate: super::Estate =
            from_str(include_str!("../../../deploy/estate.example.toml")).unwrap();
        super::validate_card_stock(&estate.card_stock, &estate.contracts).unwrap();
        for bad in [
            make("A-7", "04A1B2C3D4E5F6"), // already seeded
            make("MOB-1001", "1234ABCD"),  // ambiguous printed label
            make("-CARD", "1234ABCD"),
            make("CARD", "1234abcd"),
            make("CARD", "1234"),
        ] {
            assert!(
                super::validate_card_stock(std::slice::from_ref(&bad), &estate.contracts).is_err()
            );
            assert!(!format!("{bad:?}").contains(&bad.uid));
        }
        let malformed = include_str!("../../../deploy/estate.example.toml")
            .replace("uid = \"04AB0000000004\"", "uid = 12345678");
        let diagnostic = from_str::<super::Estate>(&malformed)
            .unwrap_err()
            .to_string();
        assert!(
            diagnostic.contains("invalid card configuration") && !diagnostic.contains("12345678")
        );
        for conflicting in [make("A", "12345678"), make("B", "11111111")] {
            assert!(
                super::validate_card_stock(&[make("A", "11111111"), conflicting], &[]).is_err()
            );
        }
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

    #[test]
    fn a_subscriber_is_checked_before_anything_is_sent_to_it() {
        let env: Env = std::sync::Arc::new(|name| {
            (name == "SUB_SECRET").then(|| "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw".to_owned())
        });
        let subscriber = |name: &str, url: &str, types: &[&str]| Subscriber {
            name: name.to_owned(),
            url: url.to_owned(),
            secret_env: EnvRef("SUB_SECRET".to_owned()),
            types: types.iter().map(|t| (*t).to_owned()).collect(),
        };
        let fine = subscriptions(
            &[subscriber(
                "warehouse",
                "https://dwh.example/in",
                &["de.emob.*"],
            )],
            &env,
        )
        .unwrap();
        assert!(fine[0].takes(crate::events::cdr::ISSUED));
        assert!(fine[0].takes(crate::events::invoice::ISSUED));

        // platform-interfaces/SR-030: distinct names cannot share authority.
        assert!(separate_subscriber_keys(&fine, &fine[0].secret).is_err());
        let delivery = InvoiceDelivery {
            url: "https://erp.example/invoices".into(),
            secret_env: EnvRef("SUB_SECRET".into()),
            syntax: InvoiceSyntax::Ubl,
        };
        assert!(delivery.signing_key(&env, &fine, None).is_err());
        assert!(
            delivery
                .signing_key(&env, &[], Some(&fine[0].secret))
                .is_err()
        );
        assert!(delivery.signing_key(&env, &[], None).is_ok());
        let shared_key = subscriptions(
            &[
                subscriber("billing", "https://billing.example", &["*"]),
                subscriber("warehouse", "https://warehouse.example", &["*"]),
            ],
            &env,
        )
        .unwrap_err();
        assert!(shared_key.to_string().contains("own signing key"));
        let refused = |s: Subscriber| subscriptions(&[s], &env).unwrap_err().to_string();
        assert!(
            refused(subscriber(
                "dwh",
                "https://dwh.example",
                &["de.emob.cdr.isued"]
            ))
            .contains("admits no type")
        );
        assert!(refused(subscriber("dwh", "http://dwh.example", &["de.emob.*"])).contains("https"));
        assert!(refused(subscriber("dwh", "https://dwh.example", &[])).contains("no type"));
        assert!(refused(subscriber("d\nwh", "https://dwh.example", &["*"])).contains("name"));
        assert!(
            subscriptions(
                &[
                    subscriber("a", "https://a.example", &["*"]),
                    subscriber("a", "https://b.example", &["*"])
                ],
                &env
            )
            .unwrap_err()
            .to_string()
            .contains("named twice")
        );
    }

    #[derive(serde::Deserialize)]
    struct Tariffs {
        tariffs: Vec<TariffSpec>,
    }

    fn tariffs(text: &str) -> Vec<TariffSpec> {
        toml::from_str::<Tariffs>(text).unwrap().tariffs
    }

    const TWO_VERSIONS: &str = r#"
        [[tariffs]]
        id = "ad-hoc"
        currency = "EUR"
        time_zone = "Europe/Berlin"
        energy = "0.59"
        vat = "19"

        [[tariffs]]
        id = "ad-hoc"
        currency = "EUR"
        time_zone = "Europe/Berlin"
        energy = "0.52"
        vat = "19"
        session_fee = "0.50"
        valid_from = "2027-01-01T00:00:00+01:00"
    "#;

    #[test]
    fn a_tariff_has_versions_and_each_ends_where_the_next_begins() {
        let histories = histories(&tariffs(TWO_VERSIONS)).unwrap();
        let history = &histories["ad-hoc"];
        assert_eq!(history.versions().len(), 2);
        let new_year = time::macros::datetime!(2027-01-01 00:00 +1);
        assert_eq!(history.versions()[0].valid_until, Some(new_year));
        let before = history
            .in_force_at(new_year - time::Duration::seconds(1))
            .unwrap();
        let after = history.in_force_at(new_year).unwrap();
        assert_ne!(before.fingerprint(), after.fingerprint());
        assert_eq!(after.dimensions().len(), 2, "energy and a session fee");
    }

    #[test]
    fn a_version_off_the_quarter_hour_or_without_a_start_is_named() {
        for rate in ["-0.01", "79228162514264337593543950335"] {
            let supplied = format!("occupancy_per_minute = \"{rate}\"\n");
            let config = TWO_VERSIONS.replace("session_fee =", &(supplied + "session_fee ="));
            let error = histories(&tariffs(&config)).unwrap_err().to_string();
            assert!(error.contains("tariffs[1].occupancy_per_minute"), "{error}");
        }
        let off_grid = TWO_VERSIONS.replace("00:00:00+01:00", "00:07:00+01:00");
        let error = histories(&tariffs(&off_grid)).unwrap_err().to_string();
        assert!(error.contains("tariffs[0]"), "{error}");
        let no_start = TWO_VERSIONS.replace("valid_from = \"2027-01-01T00:00:00+01:00\"", "");
        let error = histories(&tariffs(&no_start)).unwrap_err().to_string();
        assert!(error.contains("tariffs[1].valid_from"), "{error}");
    }
}

mod public_plan;
mod retail_offer;
pub use public_plan::{
    PublicPlan, PublicSchedule, check_public_plans, validate_public_origin, validate_public_plans,
};
pub use retail_offer::{RetailOffer, RetailTerms};

mod retail_account;
pub use retail_account::{
    RetailAccount, RetailAccountKind, valid_account_id, validate_retail_accounts,
};

mod portal_user;
pub use portal_user::{PortalUser, portal_credentials};
