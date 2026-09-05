//! How a charging session was authorised.
//!
//! Six paths reach the same outcome — a station starts delivering energy — and
//! they are not interchangeable. Two things depend on which one was used:
//!
//! - **AFIR.** A publicly accessible point must offer at least one path that
//!   needs no contract `[AFIR Art. 5(1)]`, and the price on that path carries
//!   no roaming surcharge `[AFIR Art. 5(4)]`.
//! - **Eichrecht.** The signed record states how strongly the user was
//!   identified `[OCMF Tab. 11]`. A session recorded as recognised by its MAC
//!   address, whose signed record claims a secure feature established the
//!   assignment, is telling two different stories, and
//!   [`AuthPath::strongest_plausible_level`] is what lets the CDR layer notice.
//!
//!   The comparison runs in **one direction only**, and that is deliberate: the
//!   record over-claiming against the path is a fault, the record under-claiming
//!   is not. A station may omit its identification section, and a chain that did
//!   not hold up reports no identification at all — so treating a weak record as
//!   contradicting a strong path would refuse a session for the crime of having
//!   less evidence than it might have had.

use emob_core::{ContractId, Emaid, IdentificationStrength};

/// The mechanism that authorised a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum AuthPath {
    /// The station's own local authorisation list or cache decided.
    LocalList,
    /// An `Authorize` went to the e-mobility provider, directly or through a
    /// roaming hub, and came back affirmative.
    Roaming,
    /// No contract at all: a payment card at the point, or a web/QR flow.
    /// The path AFIR requires every public point to offer.
    AdHoc,
    /// ISO 15118 Plug & Charge: the vehicle presented a contract certificate.
    PlugAndCharge,
    /// The vehicle was recognised by its MAC address.
    ///
    /// Not a standard, not authenticated, and trivially spoofable — it is
    /// modelled because the field uses it, and kept distinct from
    /// [`Self::PlugAndCharge`] because the two are constantly conflated in
    /// marketing and must never be conflated in evidence.
    AutoCharge,
    /// A backend command (`RemoteStartTransaction`, `StartSession`) began it.
    RemoteCommand,
}

impl AuthPath {
    /// Whether this path works without the driver holding a contract.
    ///
    /// The AFIR Art. 5(1) question, asked of a session rather than of a
    /// station.
    #[must_use]
    pub const fn is_contract_free(self) -> bool {
        matches!(self, Self::AdHoc)
    }

    /// Whether the driver was identified cryptographically.
    #[must_use]
    pub const fn is_cryptographic(self) -> bool {
        matches!(self, Self::PlugAndCharge)
    }

    /// The strongest OCMF identification level this path can honestly support.
    ///
    /// Used to cross-check a session against its own signed records: a session
    /// recorded as [`Self::AutoCharge`] whose record claims `SECURE` is two
    /// stories about one event, and the one with a signature behind it is not
    /// the one that can be right — a MAC address off the wire is not a secure
    /// feature under any reading of `[OCMF Tab. 11]`.
    ///
    /// The mapping is deliberately generous — it is a ceiling, not an
    /// expectation — because a station may under-report and that is not a
    /// fault. Over-reporting is.
    ///
    /// # Read off Tables 13–16, not off the decision
    ///
    /// `[OCMF Tab. 11]` grades **how the user was identified**; Tables 13–16
    /// say which identifications each mechanism can carry. The two axes are
    /// largely orthogonal, so a ceiling set from *who decided* refuses ordinary
    /// hardware: a local list decides against an RFID card, and `RFID_PSK`
    /// `[OCMF Tab. 13]` — a secured card — is Table 11's own example of
    /// `SECURE`. A backend `Authorize` can answer `OCPP_CERTIFIED`
    /// `[OCMF Tab. 14]`, which is `CERTIFIED`; so can a remote start.
    ///
    /// What stays low is what cannot rise: an unauthenticated MAC address, and
    /// an ad-hoc session that presented no contract for anything to certify.
    #[must_use]
    #[allow(
        clippy::match_same_arms,
        reason = "the arms coincide today and hold for different reasons, and each reason \
                  is a different row of OCMF Tables 13–16 that a reviewer has to check; \
                  merging them would delete the argument and make the next change silent"
    )]
    pub const fn strongest_plausible_level(self) -> IdentificationStrength {
        match self {
            // A contract certificate verified by the station — `ISO15118_PNC`
            // `[OCMF Tab. 15]`, which is Table 11's other example of `SECURE`.
            Self::PlugAndCharge => IdentificationStrength::Secure,
            // An RFID card read locally, which `[OCMF Tab. 13]` grades from
            // `RFID_PLAIN` up to `RFID_PSK` — a secured card, and `SECURE`.
            Self::LocalList => IdentificationStrength::Secure,
            // `OCPP_AUTH`, `OCPP_AUTH_TLS`, `OCPP_CACHE`, `OCPP_WHITELIST` or
            // `OCPP_CERTIFIED` `[OCMF Tab. 14]`. The last certifies the user
            // mapping with a backend certificate, which is `CERTIFIED` — but
            // not `SECURE`, which Table 11 reserves for an assignment the
            // signature component itself established by a secure feature.
            Self::Roaming | Self::RemoteCommand => IdentificationStrength::Certified,
            // A payment instrument or a web flow. There is no contract, so
            // there is nothing for a certificate to certify and nothing a
            // secure feature could have established.
            Self::AdHoc => IdentificationStrength::Trusted,
            // A MAC address off the wire: not a standard, not authenticated,
            // trivially spoofable. `PLMN_NONE`-grade evidence at best.
            Self::AutoCharge => IdentificationStrength::Hearsay,
        }
    }
}

impl core::fmt::Display for AuthPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::LocalList => "local authorisation list",
            Self::Roaming => "roaming authorisation",
            Self::AdHoc => "ad-hoc",
            Self::PlugAndCharge => "Plug & Charge",
            Self::AutoCharge => "AutoCharge",
            Self::RemoteCommand => "remote command",
        })
    }
}

/// Who the session is for.
///
/// An ad-hoc session has no contract by construction, which is why this is an
/// enum rather than an `Option<ContractId>`: "no contract" and "contract not
/// recorded" are different facts, and only the first one is lawful to bill
/// against an ad-hoc tariff.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Subject {
    /// A contract at an e-mobility provider.
    Contract {
        /// The provider-side contract.
        id: ContractId,
        /// The contract identifier the vehicle or token presented, when there
        /// is one.
        emaid: Option<Emaid>,
    },
    /// A driver with no contract, paying at the point.
    AdHoc,
}

impl Subject {
    /// The contract, if there is one.
    #[must_use]
    pub const fn contract_id(&self) -> Option<&ContractId> {
        match self {
            Self::Contract { id, .. } => Some(id),
            Self::AdHoc => None,
        }
    }
}

/// The complete authorisation of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Authorization {
    /// Which mechanism.
    pub path: AuthPath,
    /// Who for.
    pub subject: Subject,
    /// An opaque, keyed hash of the token that was presented.
    ///
    /// Never the RFID UID or the eMAID itself. A UID is a lifelong identifier
    /// of a physical object a person carries; storing it in every session row
    /// builds a movement profile that no part of this platform needs.
    pub token_ref: Option<TokenRef>,
    /// The provider's authorisation reference, for a roaming session.
    pub authorization_reference: Option<String>,
}

impl Authorization {
    /// An ad-hoc authorisation: no contract, by construction.
    #[must_use]
    pub const fn ad_hoc() -> Self {
        Self {
            path: AuthPath::AdHoc,
            subject: Subject::AdHoc,
            token_ref: None,
            authorization_reference: None,
        }
    }

    /// Whether this session may be billed against an ad-hoc tariff.
    ///
    /// Both halves have to agree. A session on the ad-hoc *path* that carries a
    /// contract is a contract session that used a card reader, and billing it
    /// at the ad-hoc price would be charging a customer twice over.
    #[must_use]
    pub fn is_ad_hoc(&self) -> bool {
        self.path.is_contract_free() && matches!(self.subject, Subject::AdHoc)
    }
}

/// An opaque reference to a token, safe to store.
///
/// Constructed by hashing the token with a key the platform holds — the
/// construction itself lives in a service, because it needs the key and this
/// crate does no I/O. What is enforced here is that the type cannot be built
/// from something that merely *looks* like a hash: a caller who passes a raw
/// UID gets a refusal rather than a privacy incident.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct TokenRef(String);

/// Read back through [`TokenRef::new`], because the refusal is the whole type.
///
/// *"A caller who passes a raw UID gets a refusal rather than a privacy
/// incident"* — and a `#[serde(transparent)]` derive is a caller that never
/// asked. An RFID UID or an eMAID arriving from a store, an outbox or a
/// partner's document went straight into the field the platform stores instead
/// of the driver's identity, which is the incident the type exists to prevent
/// (D264).
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for TokenRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(raw.into_owned()).map_err(D::Error::custom)
    }
}

impl TokenRef {
    /// The length a reference must have: 64 lowercase hex characters, i.e. a
    /// 256-bit digest.
    pub const HEX_LEN: usize = 64;

    /// Wrap a keyed digest.
    ///
    /// # Errors
    ///
    /// [`AuthError::NotADigest`] unless the value is exactly 64 lowercase hex
    /// characters. An RFID UID is 8 or 14 hex characters and an eMAID is 14 or
    /// 15 alphanumerics, so neither can pass — which is the point.
    pub fn new(digest_hex: impl Into<String>) -> Result<Self, AuthError> {
        let value = digest_hex.into();
        if value.len() != Self::HEX_LEN
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(AuthError::NotADigest { len: value.len() });
        }
        Ok(Self(value))
    }

    /// The digest as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What can be wrong with an authorisation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The value is not a 256-bit hex digest — very possibly a raw token.
    #[error(
        "a token reference is {} lowercase hex characters (a keyed digest), got {len}; never store a raw UID or eMAID",
        TokenRef::HEX_LEN
    )]
    NotADigest {
        /// The length that was offered.
        len: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "serde")]
    #[test]
    fn a_raw_uid_cannot_arrive_as_a_token_reference() {
        // The refusal is the whole type: *"a caller who passes a raw UID gets a
        // refusal rather than a privacy incident"* — and a
        // `#[serde(transparent)]` derive was a caller that never asked (D264).
        assert!(serde_json::from_str::<TokenRef>("\"04A1B2C3D4E5F6\"").is_err());
        assert!(serde_json::from_str::<TokenRef>("\"DE-8AA-CA2E4XY9-4\"").is_err());
        let digest = "a".repeat(TokenRef::HEX_LEN);
        assert_eq!(
            serde_json::from_str::<TokenRef>(&format!("\"{digest}\""))
                .unwrap()
                .as_str(),
            digest
        );
    }

    #[test]
    fn only_ad_hoc_is_contract_free() {
        assert!(AuthPath::AdHoc.is_contract_free());
        for path in [
            AuthPath::LocalList,
            AuthPath::Roaming,
            AuthPath::PlugAndCharge,
            AuthPath::AutoCharge,
            AuthPath::RemoteCommand,
        ] {
            assert!(!path.is_contract_free());
        }
    }

    #[test]
    fn autocharge_is_not_plug_and_charge() {
        // Conflated everywhere in marketing; never in evidence.
        assert!(AuthPath::PlugAndCharge.is_cryptographic());
        assert!(!AuthPath::AutoCharge.is_cryptographic());
        assert!(
            AuthPath::AutoCharge.strongest_plausible_level() < IdentificationStrength::Trusted,
            "a MAC address is hearsay"
        );
    }

    #[test]
    fn the_ceiling_is_read_off_the_identification_tables_not_off_the_decision() {
        // A station's own list decides against an RFID card, and `RFID_PSK`
        // `[OCMF Tab. 13]` — a secured card — is `[OCMF Tab. 11]`'s own example
        // of `SECURE`. A ceiling read off "who decided" would put this at
        // hearsay and refuse every session from a secure-card installation:
        // exactly the installation that went to the trouble of not being one.
        assert_eq!(
            AuthPath::LocalList.strongest_plausible_level(),
            IdentificationStrength::Secure
        );

        // `OCPP_CERTIFIED` `[OCMF Tab. 14]` certifies the user mapping with a
        // backend certificate — `CERTIFIED`, and not `SECURE`, which Table 11
        // reserves for the signature component's own secure feature.
        for path in [AuthPath::Roaming, AuthPath::RemoteCommand] {
            assert_eq!(
                path.strongest_plausible_level(),
                IdentificationStrength::Certified,
                "{path}"
            );
        }

        // What stays low is what cannot rise: no contract was presented, so
        // there is nothing for a certificate to certify.
        assert_eq!(
            AuthPath::AdHoc.strongest_plausible_level(),
            IdentificationStrength::Trusted
        );
        assert_eq!(
            AuthPath::AutoCharge.strongest_plausible_level(),
            IdentificationStrength::Hearsay
        );
    }

    #[test]
    fn paths_and_strengths_render_as_prose() {
        assert_eq!(AuthPath::AdHoc.to_string(), "ad-hoc");
        assert_eq!(AuthPath::PlugAndCharge.to_string(), "Plug & Charge");
    }

    #[test]
    fn an_ad_hoc_session_needs_both_halves() {
        let ad_hoc = Authorization::ad_hoc();
        assert!(ad_hoc.is_ad_hoc());

        // The ad-hoc path with a contract behind it is a contract session that
        // happened to use the card reader.
        let mixed = Authorization {
            path: AuthPath::AdHoc,
            subject: Subject::Contract {
                id: "c-1".parse().unwrap(),
                emaid: None,
            },
            token_ref: None,
            authorization_reference: None,
        };
        assert!(!mixed.is_ad_hoc());
    }

    #[test]
    fn a_token_ref_refuses_a_raw_identifier() {
        // An RFID UID…
        assert!(TokenRef::new("1F2D3A4F5506C7").is_err());
        // …an eMAID…
        assert!(TokenRef::new("NLTNM000122045U").is_err());
        // …and anything that is not a 256-bit digest.
        assert!(TokenRef::new("").is_err());
        assert!(
            TokenRef::new("A".repeat(64)).is_err(),
            "uppercase is not the canonical form"
        );

        let digest = "a".repeat(64);
        assert_eq!(TokenRef::new(digest.clone()).unwrap().as_str(), digest);
    }

    #[test]
    fn the_refusal_says_what_to_do_instead() {
        let err = TokenRef::new("1F2D3A4F5506C7").unwrap_err();
        assert!(err.to_string().contains("never store a raw UID"));
    }
}
