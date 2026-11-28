//! The demonstration estate: Stadtwerke Musterstadt eMobility, written from a
//! seed and a set of ports.
//!
//! Every daemon reads its own sections of this one file, exactly as from the
//! `estate.toml` a production node is given. Secrets are never written here:
//! each `*_env` key names a variable the demo's in-process environment holds
//! (SR-071).

use std::net::SocketAddr;
use std::path::Path;

use emob_sim::{Rng, VirtualStation};
use ocpp_kit::Version;

/// One charge point of the estate.
#[derive(Debug, Clone)]
pub struct ChargePoint {
    /// The OCPP identity it connects as.
    pub identity: &'static str,
    /// The OCPP version it speaks.
    pub version: Version,
    /// The EVSE id its records are issued under.
    pub evse: &'static str,
}

/// The three charge points, one per OCPP generation.
pub const CHARGE_POINTS: [ChargePoint; 3] = [
    ChargePoint {
        identity: "CP-1",
        version: Version::V1_6,
        evse: "DE*MUS*E00001",
    },
    ChargePoint {
        identity: "CP-2",
        version: Version::V2_0_1,
        evse: "DE*MUS*E00002",
    },
    ChargePoint {
        identity: "CP-3",
        version: Version::V2_1,
        evse: "DE*MUS*E00003",
    },
];

/// The card the partner's contract carries, and the one it blocked.
pub const GOOD_CARD: &str = "04A1B2C3D4E5F6";
/// See [`GOOD_CARD`].
pub const BLOCKED_CARD: &str = "04DEADBEEF0001";

/// The variable names the estate refers to. Their values live only in the
/// demo's process.
pub mod env {
    /// The named operator's write credential.
    pub const OPERATOR: &str = "EMOB_OPERATOR_TOKEN";
    /// The desk's background read credential.
    pub const DESK_READ: &str = "EMOB_DESK_READ_TOKEN";
    /// Service-to-service bearer.
    pub const SERVICE: &str = "EMOB_SERVICE_TOKEN";
    /// The deliveries' webhook secret.
    pub const WEBHOOK: &str = "EMOB_WEBHOOK_SECRET";
    /// The partner's bootstrap token.
    pub const TOKEN_A: &str = "EMOB_PARTNER_DE_MSP_TOKEN_A";
    /// One data key per private journal.
    pub const DATA_KEYS: [&str; 5] = [
        "CSMSD_DATA_KEY",
        "ROAMD_DATA_KEY",
        "EMPD_DATA_KEY",
        "BILLD_DATA_KEY",
        "ACCOUNTINGD_DATA_KEY",
    ];
    /// The head-signing keys (spec 008), one per journalled daemon.
    pub const SIGNING_KEYS: [&str; 5] = [
        "CSMSD_SIGNING_KEY",
        "ROAMD_SIGNING_KEY",
        "EMPD_SIGNING_KEY",
        "BILLD_SIGNING_KEY",
        "ACCOUNTINGD_SIGNING_KEY",
    ];
}

/// Where each listener is.
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    /// csmsd's OCPP socket.
    pub ocpp: SocketAddr,
    /// csmsd's shell (read model, receipts).
    pub csmsd: SocketAddr,
    /// roamd.
    pub roamd: SocketAddr,
    /// billd.
    pub billd: SocketAddr,
    /// empd.
    pub empd: SocketAddr,
    /// deskd, Heute.
    pub deskd: SocketAddr,
    /// Finance ledger.
    pub accountingd: SocketAddr,
    /// Customer portal.
    pub portald: SocketAddr,
}

/// The meters: `emob-sim`'s virtual stations 1..=3 from the seed's `estate`
/// stream — station 1 of seed 2026 is the key `deploy/estate.example.toml`
/// lists.
#[must_use]
pub fn meters(seed: u64) -> Vec<VirtualStation> {
    let mut rng = Rng::stream(seed, "estate");
    (1..=3)
        .map(|index| VirtualStation::new(index, &mut rng))
        .collect()
}

/// The estate's text.
///
/// `hashes` are the Argon2id hashes of the three stations' passwords, in
/// [`CHARGE_POINTS`] order; `tls` is the directory `just dev-ca` wrote.
/// `price_change` is the day the tariff's announced second version starts
/// (spec 015): after every session the demo runs, so its totals never move.
#[must_use]
pub fn text(
    seed: u64,
    ports: &Ports,
    hashes: &[String; 3],
    tls: &Path,
    price_change: time::Date,
) -> String {
    let meters = meters(seed);
    let mut out = String::from(
        "# The demonstration estate `emob demo` writes — Stadtwerke Musterstadt eMobility.\n\
         # Throwaway: every *_env names a variable that lives only in the demo's process.\n\n\
         [parties]\noperator = \"DE*MUS\"\nprovider = \"DE*MSP\"\n\n",
    );
    for ((point, meter), hash) in CHARGE_POINTS.iter().zip(&meters).zip(hashes) {
        out.push_str(&format!(
            "[[stations]]\npublicly_accessible = true\nidentity = \"{identity}\"\nocpp = \"{ocpp}\"\nsecurity_profile = 2\n\
             password_hash = \"{hash}\"\nevse = \"{evse}\"\nrated_power_kw = \"{power}\"\n\
             tariff = \"ad-hoc\"\nconnector = \"{connector}\"\n{display}{price}\
             location = {{ id = \"LOC-MUS-1\", name = \"Betriebshof Musterstadt\", \
             street = \"Am Stadtwerk\", house_number = \"1\", postal_code = \"12345\", \
             city = \"Musterstadt\", country = \"DE\", latitude = \"51.163400\", \
             longitude = \"10.447700\", time_zone = \"Europe/Berlin\" }}\n\n",
            identity = point.identity,
            ocpp = point.version,
            // Spec 011: the 2.1 station shows its receipt link for ten minutes.
            display = if point.version == Version::V2_1 {
                "receipt_display_minutes = 10\n"
            } else {
                ""
            },
            // Spec 015: the 2.x stations show the price and its Preisstand.
            price = if point.version == Version::V1_6 {
                ""
            } else {
                "show_price = true\n"
            },
            evse = point.evse,
            power = meter.rated_power_kw,
            connector = if meter.is_fast() {
                "iec62196_t2_combo"
            } else {
                "iec62196_t2"
            },
        ));
    }
    for meter in &meters {
        out.push_str(&format!(
            "[[meter_keys]]\nmeter = \"{serial}\"\ncurve = \"secp256r1\"\nkey = \"{key}\"\n\
             provenance = \"emob-sim virtual meter, seed {seed} — demonstration only\"\n\n",
            serial = meter.meter_serial,
            key = hex::encode(meter.public_key().sec1_bytes()),
        ));
    }
    let tls = |name: &str| tls.join(name).display().to_string().replace('\\', "/");
    out.push_str(&format!(
        r#"[[tariffs]]
id = "ad-hoc"
currency = "EUR"
time_zone = "Europe/Berlin"
energy = "0.49"
vat = "19"

# Announced: from the first of the month after next, a session fee and a lower
# price per kWh. The price page shows it ahead, and the station's display
# switches on its own at that minute.
[[tariffs]]
id = "ad-hoc"
currency = "EUR"
time_zone = "Europe/Berlin"
energy = "0.45"
session_fee = "0.50"
vat = "19"
valid_from = "{price_change}"

[[public_plans]]
provider = "DE*MSP"
code = "standard"
name = "MSP Standard"
uniform_public_terms = true
points = ["DE*MUS*E00001", "DE*MUS*E00002"]

[[public_plans.schedules]]
version = "v1"
from = "2020-01-01T00:00:00Z"
currency = "EUR"
vat = "19"
energy = "0.11"
session = "0"
roaming = "0.30"
monthly = "0"

[[partners]]
party = "DE*MSP"
token_a_env = "{token_a}"
signed_data = true
settles_within_days = 30
billing = {{ name = "MSP Mobility GmbH", street = "Am Markt 2", post_code = "50667", city = "Köln", country = "DE", vat_id = "DE123456789", electronic_address = "eingangsrechnung@msp-mobility.example" }}
specification = "xrechnung"
buyer_reference = "MSP-KREDITOR-4711"
mandate = {{ reference = "MSP-LS-2026-001", iban = "DE02120300000000202051" }}

[billing]
series = "R"
vat_rate = "19"
seller = {{ name = "Stadtwerke Musterstadt eMobility GmbH", street = "Am Stadtwerk 1", post_code = "12345", city = "Musterstadt", country = "DE", vat_id = "DE999999999", electronic_address = "rechnung@stadtwerke-musterstadt.example" }}
retail_seller = {{ name = "MSP Mobility GmbH", street = "Ring 9", post_code = "10115", city = "Berlin", country = "DE", vat_id = "DE111222333" }}
creditor_id = "DE98ZZZ09999999999"
contact = {{ name = "Rechnungswesen", phone = "+49 3601 4711-0", email = "rechnung@stadtwerke-musterstadt.example" }}

# Unissued RFID for the standalone driver-enrolment walkthrough.
[[retail_accounts]]
id = "PAYER-PRIVATE"
kind = "private"
name = "Mika Muster"
street = "Beispielweg 2"
post_code = "10115"
city = "Berlin"
country = "DE"

[[retail_accounts]]
id = "PAYER-FLEET"
kind = "fleet"
name = "Musterflotte GmbH"
street = "Beispielweg 4"
post_code = "10115"
city = "Berlin"
country = "DE"
vat_id = "DE111333555"
electronic_address = "fleet@example.invalid"

[[card_stock]]
code = "STOCK-1004"
uid = "04AB0000000004"

[[card_stock]]
code = "STOCK-1005"
uid = "04AB0000000005"

[[card_stock]]
code = "STOCK-1006"
uid = "04AB0000000006"

[[card_stock]]
code = "STOCK-1007"
uid = "04AB0000000007"

[[card_stock]]
code = "STOCK-1008"
uid = "04AB0000000008"

[[contracts]]
id = "DE-MSP-C00000001-W"
from = "2026-01-01"
tokens = [
  {{ uid = "{good}", visual_number = "MOB-1001", type = "RFID", whitelist = "ALLOWED" }},
  {{ uid = "{blocked}", visual_number = "MOB-1003", type = "RFID", whitelist = "ALLOWED", blocked = true }},
]

[[operators]]
name = "demo operator"
token_env = "{operator}"
write = true
finance = true
privacy = true

[[operators]]
name = "desk reader"
token_env = "{desk_read}"

[csms]
tls_cert = "{cert}"
tls_key = "{key}"
client_ca = "{ca}"
max_connections = 64
messages_per_minute = 600
idle_timeout_secs = 900

[desk]
csmsd = "http://{csmsd}"
token_env = "{desk_read}"
time_zone = "Europe/Berlin"

[[portal_users]]
subject = "mika"
secret_env = "EMOB_PORTAL_MIKA"

[portal]
public_url = "http://{portald}"

[receipts]
public_url = "http://{csmsd}"

[services]
accountingd = "http://{accountingd}"
billd = "http://{billd}"
roamd = "http://{roamd}"
empd = "http://{empd}"
webhook_secret_env = "{webhook}"
service_token_env = "{service}"
authorize_timeout_ms = 2000
retry_secs = 1

[roaming]
public_url = "http://{roamd}/ocpi"
reach_back_allow = ["127.0.0.1", "::1", "localhost"]
"#,
        // Local midnight in the stations' zone, as the instant it is.
        price_change = emob_core::TimeZone::new("Europe/Berlin")
            .ok()
            .and_then(|zone| zone.instant_at(price_change, time::Time::MIDNIGHT))
            .and_then(|at| at.format(&time::format_description::well_known::Rfc3339).ok())
            .unwrap_or_default(),
        token_a = env::TOKEN_A,
        good = GOOD_CARD,
        blocked = BLOCKED_CARD,
        operator = env::OPERATOR,
        desk_read = env::DESK_READ,
        webhook = env::WEBHOOK,
        service = env::SERVICE,
        cert = tls("server.pem"),
        key = tls("server.key"),
        ca = tls("ca.pem"),
        portald = ports.portald,
        accountingd = ports.accountingd,
        csmsd = ports.csmsd,
        billd = ports.billd,
        roamd = ports.roamd,
        empd = ports.empd,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_service::estate;

    fn ports() -> Ports {
        let at = |port| SocketAddr::from(([127, 0, 0, 1], port));
        Ports {
            ocpp: at(9000),
            csmsd: at(9001),
            roamd: at(9583),
            billd: at(9584),
            empd: at(9585),
            deskd: at(9587),
            portald: at(9589),
            accountingd: at(9591),
        }
    }

    fn demo() -> String {
        let hash = csmsd::edge::hash_password(Version::V2_0_1, "a-password-of-16-chars").unwrap();
        text(
            2026,
            &ports(),
            &[hash.clone(), hash.clone(), hash],
            Path::new("/tmp/dev-tls"),
            time::macros::date!(2027 - 01 - 01),
        )
    }

    #[test]
    fn every_daemon_runs_on_the_estate_the_demo_writes() {
        let text = demo();
        let csmsd: csmsd::estate::Estate = estate::from_str(&text).expect("csmsd's sections");
        assert_eq!(csmsd.stations.len(), 3);
        assert_eq!(csmsd.meter_keys.len(), 3);
        let _: billd::daemon::Estate = estate::from_str(&text).expect("billd's");
        let _: roamd::daemon::Estate = estate::from_str(&text).expect("roamd's");
        let _: empd::daemon::Estate = estate::from_str(&text).expect("empd's");
        let _: deskd::Estate = estate::from_str(&text).expect("deskd's");
    }

    #[test]
    fn meter_one_of_seed_2026_is_the_key_the_example_estate_lists() {
        // The demo and `deploy/estate.example.toml` stand on the same meter.
        let example = include_str!("../../../../deploy/estate.example.toml");
        let key = hex::encode(meters(2026)[0].public_key().sec1_bytes());
        assert!(example.contains(&key), "{key}");
        assert!(demo().contains(&key));
    }

    #[test]
    fn another_seed_is_another_set_of_meters() {
        let a = hex::encode(meters(2026)[1].public_key().sec1_bytes());
        let b = hex::encode(meters(2027)[1].public_key().sec1_bytes());
        assert_ne!(a, b);
    }
}
