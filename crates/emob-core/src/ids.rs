//! The identifiers the e-mobility market runs on.
//!
//! # Two grammars, one identity, and the text that arrived
//!
//! Every identifier here accepts more than one written form of the same thing.
//! An `EvseId` may be `DE*AB7*E840*6487` or `DEAB7E8406487`; an [`Emaid`] may
//! follow ISO 15118 (`DE-8AA-CA2B3C4D5-1`) or DIN SPEC 91286
//! (`DE8AA1234567890`). Two rules follow, and both are load-bearing:
//!
//! 1. **Equality is canonical.** The separated and packed spellings of one
//!    charge point are the same charge point, hash the same and compare equal.
//! 2. **`Display` returns the text that arrived, byte for byte.** Hubject
//!    compares the identifier in a URL against the TLS client certificate *as
//!    text* and answers a mismatch with `017 Unauthorized Access`; a partner
//!    that sent a separated id and gets a packed one back sees a different
//!    party. Normalising on ingest is the single most common way a roaming
//!    integration fails in production, so this module refuses to do it.
//!
//! [`canonical`](EvseId::canonical) is the normalised form, for anyone who
//! wants it deliberately.
//!
//! # Why not reuse the sibling kits' types
//!
//! `ocpi-kit` and `oicp-kit` each carry their own `EvseId`, correct for their
//! own wire. A platform that speaks both needs one type the handlers are
//! written against, or the translation layer becomes a spiderweb of
//! `From`/`TryFrom` between two vocabularies that agree. These are that type;
//! the adapters at the edge convert to and from the kits' spellings.

use core::fmt;
use core::str::FromStr;

use crate::error::IdError;

/// How an identifier was spelled on the wire it arrived on.
///
/// Kept beside every parsed id so [`fmt::Display`] can reproduce the input
/// exactly. Never used for equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Spelling {
    /// Separators present, e.g. `DE*AB7*E840*6487` or `DE-8AA-CA2B3C4D5-1`.
    Separated,
    /// No separators, e.g. `DEAB7E8406487`.
    Packed,
}

/// Uppercase-and-strip helper shared by the identifier parsers.
fn canonicalise(raw: &str, separators: &[char]) -> String {
    raw.chars()
        .filter(|c| !separators.contains(c))
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

// ── EvseId ──────────────────────────────────────────────────────────────────

/// An EVSE identifier: country, operator, and the charge point within it.
///
/// Grammar (ISO 15118-1 / eMI3, as OICP and OCPI both use it):
///
/// ```text
/// <country: 2 alpha> <operator: 3 alnum> "E" <power outlet: 1..=30 alnum or *>
/// ```
///
/// The separated form writes `*` between country, operator and the `E`-prefixed
/// outlet id. The `E` is part of the grammar, not a separator, and an id whose
/// outlet section does not start with it is not an EVSE id — it is very
/// probably an [`EvcoId`], and mixing the two is how a session gets billed to a
/// contract that does not exist.
///
/// ```
/// use emob_core::ids::EvseId;
///
/// let separated: EvseId = "DE*AB7*E840*6487".parse()?;
/// let packed: EvseId = "DEAB7E8406487".parse()?;
///
/// assert_eq!(separated, packed);                       // the same charge point…
/// assert_eq!(separated.to_string(), "DE*AB7*E840*6487"); // …each written back as it arrived
/// assert_eq!(packed.to_string(), "DEAB7E8406487");
/// assert_eq!(separated.canonical(), packed.canonical());
/// assert_eq!(separated.operator_id(), "AB7");          // Hubject's own routing rule, for free
/// # Ok::<(), emob_core::error::IdError>(())
/// ```
#[derive(Debug, Clone, Eq)]
pub struct EvseId {
    /// Exactly as it arrived.
    raw: String,
    /// Uppercased, separators removed.
    canonical: String,
    spelling: Spelling,
}

impl EvseId {
    /// Parse an EVSE id in either spelling.
    ///
    /// # Errors
    ///
    /// [`IdError`] when the country, operator or outlet section is malformed.
    pub fn parse(raw: &str) -> Result<Self, IdError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(IdError::Empty { kind: "EvseId" });
        }
        let spelling = if trimmed.contains('*') || trimmed.contains('-') {
            Spelling::Separated
        } else {
            Spelling::Packed
        };
        // Under eMI3/ISO 15118-1 the `*` separator is optional *throughout*,
        // including inside the power-outlet section — `DE*AB7*E840*6487` and
        // `DEAB7E8406487` are one charge point. Canonicalising therefore strips
        // every separator, not just the two structural ones. `oicp-kit` reaches
        // the same conclusion from the same grammar, and the two must agree:
        // they meet in the roaming translation layer, and an id that compares
        // equal on one side and not the other routes a session to nobody.
        let canonical = canonicalise(trimmed, &['-', '*']);

        let bytes = canonical.as_bytes();
        if bytes.len() < 4 {
            return Err(IdError::TooShort {
                kind: "EvseId",
                min: 4,
            });
        }
        if !bytes[..2].iter().all(u8::is_ascii_alphabetic) {
            return Err(IdError::BadCountry);
        }
        if !bytes[2..5.min(bytes.len())]
            .iter()
            .all(u8::is_ascii_alphanumeric)
        {
            return Err(IdError::BadOperator);
        }
        if bytes.len() < 6 {
            return Err(IdError::TooShort {
                kind: "EvseId",
                min: 6,
            });
        }
        if bytes[5] != b'E' {
            return Err(IdError::NotAnEvse);
        }
        let outlet = &canonical[6..];
        if outlet.is_empty() || outlet.len() > 30 {
            return Err(IdError::BadOutlet);
        }
        if !outlet
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '*')
        {
            return Err(IdError::BadOutlet);
        }

        Ok(Self {
            raw: trimmed.to_owned(),
            canonical,
            spelling,
        })
    }

    /// The normalised form: uppercase, structural separators removed.
    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// The ISO 3166-1 alpha-2 country code.
    #[must_use]
    pub fn country_code(&self) -> &str {
        &self.canonical[..2]
    }

    /// The operator identifier — what a hub routes on.
    #[must_use]
    pub fn operator_id(&self) -> &str {
        &self.canonical[2..5]
    }

    /// The power-outlet section, without its `E` prefix.
    #[must_use]
    pub fn power_outlet_id(&self) -> &str {
        &self.canonical[6..]
    }

    /// How this id was spelled where it came from.
    #[must_use]
    pub fn spelling(&self) -> Spelling {
        self.spelling
    }
}

impl PartialEq for EvseId {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl core::hash::Hash for EvseId {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.canonical.hash(state);
    }
}

impl PartialOrd for EvseId {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EvseId {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.canonical.cmp(&other.canonical)
    }
}

impl fmt::Display for EvseId {
    /// The text that arrived, byte for byte.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for EvseId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

// ── Emaid / EvcoId ──────────────────────────────────────────────────────────

/// Which grammar a contract identifier was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContractGrammar {
    /// ISO 15118-1: `<country 2><provider 3><instance 9>[<check 1>]`.
    Iso15118,
    /// DIN SPEC 91286: `<country 2><provider 3><instance 9><check 1>`, always
    /// 15 characters, always with the check digit.
    Din91286,
}

/// A contract identifier — an eMAID (ISO 15118) or an EVCOID (DIN SPEC 91286).
///
/// This is what a Plug & Charge contract, an app authorisation and a roaming
/// `Authorize` all key on. Both grammars carry the same three parts and differ
/// in whether the check digit is optional, so one type models both and
/// remembers which it was given.
///
/// ```
/// use emob_core::ids::{ContractGrammar, Emaid};
///
/// let iso: Emaid = "DE-8AA-CA2B3C4D5-1".parse()?;
/// assert_eq!(iso.grammar(), ContractGrammar::Iso15118);
/// assert_eq!(iso.provider_id(), "8AA");
/// assert_eq!(iso.to_string(), "DE-8AA-CA2B3C4D5-1"); // written back as it arrived
///
/// let packed: Emaid = "DE8AACA2B3C4D51".parse()?;
/// assert_eq!(iso, packed);                           // the same contract
/// # Ok::<(), emob_core::error::IdError>(())
/// ```
#[derive(Debug, Clone, Eq)]
pub struct Emaid {
    raw: String,
    canonical: String,
    grammar: ContractGrammar,
    spelling: Spelling,
}

impl Emaid {
    /// Parse a contract id in either grammar and either spelling.
    ///
    /// # Errors
    ///
    /// [`IdError`] when the length or character classes do not fit either
    /// grammar.
    pub fn parse(raw: &str) -> Result<Self, IdError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(IdError::Empty { kind: "Emaid" });
        }
        let spelling = if trimmed.contains('-') || trimmed.contains('*') {
            Spelling::Separated
        } else {
            Spelling::Packed
        };
        let canonical = canonicalise(trimmed, &['-', '*']);

        // 14 = ISO without the optional check digit; 15 = either grammar with
        // it. DIN SPEC 91286 always carries the check digit, so a 14-character
        // id can only be ISO.
        // Both lengths read as ISO here. A 15-character id *could* be either
        // grammar — they are indistinguishable by shape — so the ambiguous case
        // resolves to ISO and `parse_din` is how a caller who knows better says
        // so. Guessing DIN from the length alone would be a coin flip recorded
        // as a fact.
        let grammar = match canonical.len() {
            14 | 15 => ContractGrammar::Iso15118,
            _ => {
                return Err(IdError::BadContractLength {
                    len: canonical.len(),
                });
            }
        };

        let bytes = canonical.as_bytes();
        if !bytes[..2].iter().all(u8::is_ascii_alphabetic) {
            return Err(IdError::BadCountry);
        }
        if !bytes[2..5].iter().all(u8::is_ascii_alphanumeric) {
            return Err(IdError::BadProvider);
        }
        if !bytes[5..].iter().all(u8::is_ascii_alphanumeric) {
            return Err(IdError::BadInstance);
        }

        Ok(Self {
            raw: trimmed.to_owned(),
            canonical,
            grammar,
            spelling,
        })
    }

    /// Parse, and require the DIN SPEC 91286 grammar (15 characters, check
    /// digit present).
    ///
    /// # Errors
    ///
    /// [`IdError::BadContractLength`] when the id is not 15 characters.
    pub fn parse_din(raw: &str) -> Result<Self, IdError> {
        let mut id = Self::parse(raw)?;
        if id.canonical.len() != 15 {
            return Err(IdError::BadContractLength {
                len: id.canonical.len(),
            });
        }
        id.grammar = ContractGrammar::Din91286;
        Ok(id)
    }

    /// The normalised form: uppercase, separators removed.
    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// The ISO 3166-1 alpha-2 country code.
    #[must_use]
    pub fn country_code(&self) -> &str {
        &self.canonical[..2]
    }

    /// The mobility provider identifier.
    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.canonical[2..5]
    }

    /// The instance part — the contract itself, without the check digit.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.canonical[5..14]
    }

    /// The check digit, when the id carries one.
    #[must_use]
    pub fn check_digit(&self) -> Option<char> {
        self.canonical.chars().nth(14)
    }

    /// Which grammar this id was read as.
    #[must_use]
    pub fn grammar(&self) -> ContractGrammar {
        self.grammar
    }

    /// How this id was spelled where it came from.
    #[must_use]
    pub fn spelling(&self) -> Spelling {
        self.spelling
    }
}

impl PartialEq for Emaid {
    /// Two contract ids are the same contract when their country, provider and
    /// instance agree — the check digit is a transcription guard, not part of
    /// the identity, and one wire carries it while another does not.
    fn eq(&self, other: &Self) -> bool {
        self.canonical[..14] == other.canonical[..14]
    }
}

impl core::hash::Hash for Emaid {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.canonical[..14].hash(state);
    }
}

impl PartialOrd for Emaid {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Emaid {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.canonical[..14].cmp(&other.canonical[..14])
    }
}

impl fmt::Display for Emaid {
    /// The text that arrived, byte for byte.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for Emaid {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// An EVCOID — the DIN SPEC 91286 spelling of a contract id.
///
/// An alias rather than a separate type: the grammars describe the same
/// identity and [`Emaid::grammar`] records which one was used.
pub type EvcoId = Emaid;

// ── Serde ───────────────────────────────────────────────────────────────────
//
// Hand-written rather than derived, and the reason is the whole point of these
// types: an id serialises as **the text that arrived**, and deserialising
// re-parses it. A derive would either emit the canonical form — changing what a
// partner sent, which is how a Hubject certificate check starts failing — or
// emit all three fields and let a hand-edited document set `canonical` to
// something the `raw` does not mean.

#[cfg(feature = "serde")]
macro_rules! serde_via_string {
    ($name:ident, $expecting:literal) => {
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.raw)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;

                impl serde::de::Visitor<'_> for Visitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str($expecting)
                    }

                    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<$name, E> {
                        $name::parse(v).map_err(serde::de::Error::custom)
                    }
                }

                deserializer.deserialize_str(Visitor)
            }
        }
    };
}

#[cfg(feature = "serde")]
serde_via_string!(EvseId, "an EVSE id such as DE*AB7*E840*6487");
#[cfg(feature = "serde")]
serde_via_string!(Emaid, "a contract id such as DE-8AA-CA2B3C4D5-1");

// ── Party identity ──────────────────────────────────────────────────────────

/// An OCPI party: a country code and a three-character party id.
///
/// The pair is the routing key for every roaming message, so it is one value
/// rather than two fields that can drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PartyId {
    country_code: String,
    party_id: String,
}

impl PartyId {
    /// Build a party id from its two parts.
    ///
    /// # Errors
    ///
    /// [`IdError`] when the country code is not two letters or the party id is
    /// not three alphanumerics.
    pub fn new(country_code: &str, party_id: &str) -> Result<Self, IdError> {
        if country_code.len() != 2 || !country_code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(IdError::BadCountry);
        }
        if party_id.len() != 3 || !party_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(IdError::BadOperator);
        }
        Ok(Self {
            country_code: country_code.to_ascii_uppercase(),
            party_id: party_id.to_ascii_uppercase(),
        })
    }

    /// The ISO 3166-1 alpha-2 country code.
    #[must_use]
    pub fn country_code(&self) -> &str {
        &self.country_code
    }

    /// The three-character party id.
    #[must_use]
    pub fn party_id(&self) -> &str {
        &self.party_id
    }
}

impl fmt::Display for PartyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}*{}", self.country_code, self.party_id)
    }
}

// ── Opaque ids ──────────────────────────────────────────────────────────────

macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident, $label:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #[cfg_attr(feature = "serde", serde(transparent))]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Build a ", $label, " from a non-empty string.")]
            ///
            /// # Errors
            ///
            /// [`IdError::Empty`] when the value is blank.
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(IdError::Empty { kind: $label });
                }
                Ok(Self(value))
            }

            /// The value as it was given.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }
    };
}

opaque_id!(
    /// A charging station, as the CSMS knows it — OCPP's charge point identity.
    StationId,
    "StationId"
);
opaque_id!(
    /// One charging session, minted by whoever started it.
    SessionId,
    "SessionId"
);
opaque_id!(
    /// One charge detail record. Unique per CPO, and the idempotency key for
    /// every roaming exchange it appears in.
    CdrId,
    "CdrId"
);
opaque_id!(
    /// A location — a site with one or more stations at one address.
    LocationId,
    "LocationId"
);
opaque_id!(
    /// A tariff, as published by a CPO or an EMP.
    TariffId,
    "TariffId"
);
opaque_id!(
    /// A driver contract at an EMP.
    ContractId,
    "ContractId"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evse_id_both_spellings_are_one_charge_point() {
        let separated: EvseId = "DE*AB7*E840*6487".parse().unwrap();
        let packed: EvseId = "DEAB7E8406487".parse().unwrap();

        assert_eq!(separated, packed);
        assert_eq!(separated.canonical(), packed.canonical());
        assert_eq!(separated.operator_id(), packed.operator_id());
    }

    #[test]
    fn evse_id_writes_back_what_arrived() {
        for raw in [
            "DE*AB7*E840*6487",
            "DEAB7E8406487",
            "DE*AB7*E8406487",
            "de*ab7*e840*6487",
        ] {
            let id: EvseId = raw.parse().unwrap();
            assert_eq!(id.to_string(), raw, "spelling must survive the round trip");
        }
    }

    #[test]
    fn evse_id_lowercase_still_compares_equal() {
        let lower: EvseId = "de*ab7*e840*6487".parse().unwrap();
        let upper: EvseId = "DE*AB7*E840*6487".parse().unwrap();
        assert_eq!(lower, upper);
        assert_eq!(lower.to_string(), "de*ab7*e840*6487");
    }

    #[test]
    fn evse_id_parts() {
        let id: EvseId = "DE*AB7*E840*6487".parse().unwrap();
        assert_eq!(id.country_code(), "DE");
        assert_eq!(id.operator_id(), "AB7");
        // Separators are optional throughout, so the outlet section is the
        // packed remainder — the same string the packed spelling yields.
        assert_eq!(id.power_outlet_id(), "8406487");
        let packed: EvseId = "DEAB7E8406487".parse().unwrap();
        assert_eq!(id.power_outlet_id(), packed.power_outlet_id());
    }

    #[test]
    fn evse_id_agrees_with_the_roaming_kits_on_equality() {
        // `oicp-kit` documents `DE*ABC*E123 == deabce123`. emob-core meets it in
        // the translation layer, so the two must reach the same verdict.
        let a: EvseId = "DE*ABC*E123".parse().unwrap();
        let b: EvseId = "deabce123".parse().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn evse_id_rejects_a_contract_id() {
        // No `E` marker: this is a contract id, and treating it as an EVSE id
        // is how a session gets billed to a charge point.
        assert!(matches!(
            EvseId::parse("DE8AACA2B3C4D51"),
            Err(IdError::NotAnEvse)
        ));
    }

    #[test]
    fn evse_id_rejects_malformed() {
        assert!(EvseId::parse("").is_err());
        assert!(EvseId::parse("D*AB7*E840").is_err());
        assert!(EvseId::parse("DE*AB7*E").is_err());
    }

    #[test]
    fn emaid_grammars_agree_on_identity() {
        let iso: Emaid = "DE-8AA-CA2B3C4D5-1".parse().unwrap();
        let packed: Emaid = "DE8AACA2B3C4D51".parse().unwrap();
        let without_check: Emaid = "DE8AACA2B3C4D5".parse().unwrap();

        assert_eq!(iso, packed);
        assert_eq!(iso, without_check, "the check digit is not the identity");
        assert_eq!(iso.provider_id(), "8AA");
        assert_eq!(iso.instance(), "CA2B3C4D5");
        assert_eq!(iso.check_digit(), Some('1'));
        assert_eq!(without_check.check_digit(), None);
    }

    #[test]
    fn emaid_writes_back_what_arrived() {
        let id: Emaid = "DE-8AA-CA2B3C4D5-1".parse().unwrap();
        assert_eq!(id.to_string(), "DE-8AA-CA2B3C4D5-1");
    }

    #[test]
    fn emaid_rejects_wrong_length() {
        assert!(Emaid::parse("DE8AA").is_err());
        assert!(Emaid::parse("DE8AACA2B3C4D5123").is_err());
    }

    #[test]
    fn party_id_normalises_but_stays_a_pair() {
        let party = PartyId::new("de", "abc").unwrap();
        assert_eq!(party.country_code(), "DE");
        assert_eq!(party.party_id(), "ABC");
        assert_eq!(party.to_string(), "DE*ABC");
        assert!(PartyId::new("DEU", "ABC").is_err());
        assert!(PartyId::new("DE", "ABCD").is_err());
    }

    #[test]
    fn opaque_ids_refuse_blanks() {
        assert!(SessionId::new("").is_err());
        assert!(SessionId::new("   ").is_err());
        assert_eq!(SessionId::new("s-1").unwrap().as_str(), "s-1");
    }
}
