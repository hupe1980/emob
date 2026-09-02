//! The identifiers the e-mobility market runs on.
//!
//! # Several grammars, one identity, and the text that arrived
//!
//! Every identifier here accepts more than one written form of the same thing.
//! An `EvseId` may be `DE*AB7*E840*6487` or `DEAB7E8406487`; an [`Emaid`] may
//! follow ISO 15118 (`NL-TNM-000122045-U`), EMI3 (`NL-TNM-C00122045-K`) or
//! DIN SPEC 91286 (`NL-TNM-012204-5`). Two rules follow, and both are
//! load-bearing:
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
//! # And the check digit is checked
//!
//! A contract identifier ends in a digit whose only job is to catch a
//! transcription error — a card read wrong, a character lost in a support
//! form, a column shifted in a partner's export. An identifier that has lost
//! one still parses, still routes, and bills a session to somebody else's
//! contract; so [`Emaid`] verifies the digit it was given and computes the one
//! it was not. The two grammars use **different algorithms**, which is why
//! telling them apart is the first thing the parser does.
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
    /// Separators present, e.g. `DE*AB7*E840*6487` or `NL-TNM-000122045-U`.
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

/// What a party does on an OCPI wire `[OCPI 2.3.0 §credentials]`.
///
/// # Why this is in `emob-core`
///
/// Two crates state rules about it, which is the same argument the settlement
/// grid and [`Crossing`] are here for. `emob-roam` routes a record by asking
/// whether a partner is an eMSP or a hub; `emob-service` carries the role a
/// credentials exchange declared, because it decides which modules a peer may
/// call at all. Two enums for one concept is a conversion table between two
/// vocabularies that agree — and the narrower of the two silently drops the
/// roles it never learned.
///
/// # It is carried, and it is not the authorisation
///
/// A role says what a party *does*, not what it may reach. A CPO's credential
/// does not thereby reach another CPO's records, and
/// `emob_service::Principal` keeps the two apart for exactly that reason.
///
/// [`Crossing`]: crate::Crossing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "SCREAMING_SNAKE_CASE"))]
#[non_exhaustive]
pub enum Role {
    /// Charge Point Operator. Operates points and sends CDRs.
    Cpo,
    /// e-Mobility Service Provider. Holds driver contracts, receives CDRs and
    /// pays against them.
    Emsp,
    /// A hub, which routes for others.
    Hub,
    /// National Access Point — the authority a country publishes through, and
    /// a party a German CPO genuinely peers with `[AFIR Art. 20(2)]`.
    Nap,
    /// Navigation Service Provider.
    Nsp,
    /// Smart Charging Service Provider.
    Scsp,
    /// Something the enumeration does not name.
    Other,
}

impl Role {
    /// The spelling OCPI uses on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpo => "CPO",
            Self::Emsp => "EMSP",
            Self::Hub => "HUB",
            Self::Nap => "NAP",
            Self::Nsp => "NSP",
            Self::Scsp => "SCSP",
            Self::Other => "OTHER",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
        let spelling = if trimmed.contains('*') {
            Spelling::Separated
        } else {
            Spelling::Packed
        };
        // Under eMI3/ISO 15118-1 the `*` separator is optional *throughout*,
        // including inside the power-outlet section — `DE*AB7*E840*6487` and
        // `DEAB7E8406487` are one charge point. Canonicalising therefore strips
        // every `*`, not just the two structural ones. `oicp-kit` reaches the
        // same conclusion from the same grammar, and the two must agree: they
        // meet in the roaming translation layer, and an id that compares equal
        // on one side and not the other routes a session to nobody.
        //
        // `-` is *not* a separator here, unlike in a contract id. The EVSE
        // grammar does not define one, and silently eating a hyphen would make
        // `DE-AB7-E840` — which is not an EVSE id — parse as one and compare
        // equal to a real charge point.
        let canonical = canonicalise(trimmed, &['*']);

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
        // Every `*` is already gone, so what remains must be alphanumeric.
        if !outlet.chars().all(|c| c.is_ascii_alphanumeric()) {
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
///
/// Three, not two, and the difference is the length of the instance section:
///
/// | Grammar | Instance | Packed length | Example |
/// |---|---|---|---|
/// | [`Din91286`](ContractGrammar::Din91286) | 6 alphanumerics | 11, or 12 with the check digit | `NL-TNM-012204-5` |
/// | [`Iso15118`](ContractGrammar::Iso15118) | 9 alphanumerics | 14, or 15 with the check digit | `NL-TNM-000122045-U` |
/// | [`Emi3`](ContractGrammar::Emi3) | `C` + 8 alphanumerics | 14, or 15 with the check digit | `NL-TNM-C00122045-K` |
///
/// EMI3 is ISO's instance section with a `C` in front, so the two are one
/// grammar with a marker rather than two shapes — but the marker is what makes
/// a DIN identifier convertible, so it is worth naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ContractGrammar {
    /// DIN SPEC 91286 — the EVCOID a German RFID card carries.
    Din91286,
    /// ISO 15118-1 — the EMAID a contract certificate carries.
    Iso15118,
    /// EMI3 — ISO's shape with a `C`-marked instance, which is what a DIN
    /// identifier becomes when it is lifted into the ISO world.
    Emi3,
}

impl ContractGrammar {
    /// How many characters the instance section has.
    #[must_use]
    pub const fn instance_len(self) -> usize {
        match self {
            Self::Din91286 => 6,
            Self::Iso15118 | Self::Emi3 => 9,
        }
    }

    /// The whole identifier's length, without the check digit.
    #[must_use]
    pub const fn body_len(self) -> usize {
        5 + self.instance_len()
    }

    /// Whether this grammar's check digit is the DIN one rather than the ISO
    /// one. They are different algorithms and they do not agree.
    #[must_use]
    pub const fn uses_din_check_digit(self) -> bool {
        matches!(self, Self::Din91286)
    }

    /// The name a diagnostic should call this grammar.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Din91286 => "DIN SPEC 91286",
            Self::Iso15118 => "ISO 15118-1",
            Self::Emi3 => "EMI3",
        }
    }
}

impl fmt::Display for ContractGrammar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A contract identifier — an EMAID (ISO 15118 / EMI3) or an EVCOID
/// (DIN SPEC 91286).
///
/// This is what a Plug & Charge contract, an app authorisation and a roaming
/// `Authorize` all key on. All three grammars carry the same three parts and a
/// check digit, so one type models them and remembers which it was given.
///
/// # The check digit is checked
///
/// It exists to catch a transcription error — a card read wrong, a digit lost
/// in a support form, a column shifted in a partner's export — and a contract
/// id that has lost a digit still *parses* and bills a session to somebody
/// else. So a supplied check digit is verified against the grammar's own
/// algorithm, and an omitted one is computed:
///
/// ```
/// use emob_core::ids::{ContractGrammar, Emaid};
///
/// let iso: Emaid = "NL-TNM-000122045-U".parse()?;
/// assert_eq!(iso.grammar(), ContractGrammar::Iso15118);
/// assert_eq!(iso.provider_id(), "TNM");
/// assert_eq!(iso.to_string(), "NL-TNM-000122045-U"); // written back as it arrived
///
/// // The same contract, spelled without separators and without the digit.
/// let packed: Emaid = "NLTNM000122045".parse()?;
/// assert_eq!(iso, packed);
/// assert_eq!(packed.check_digit(), 'U');             // …computed, not invented
///
/// // And one digit wrong is refused rather than routed.
/// assert!("NL-TNM-000122045-X".parse::<Emaid>().is_err());
/// # Ok::<(), emob_core::error::IdError>(())
/// ```
#[derive(Debug, Clone, Eq)]
pub struct Emaid {
    raw: String,
    /// Country, provider and instance — uppercase, separators removed, **no**
    /// check digit. This is the identity.
    body: String,
    check_digit: char,
    carried_check_digit: bool,
    grammar: ContractGrammar,
    spelling: Spelling,
}

impl Emaid {
    /// Parse a contract id in any of the three grammars and either spelling.
    ///
    /// The grammar follows from the length and the shape: 11 or 12 characters
    /// is DIN SPEC 91286, 14 or 15 is ISO 15118-1 — or EMI3 when the instance
    /// begins with `C`, which is the marker that makes it convertible back to
    /// DIN.
    ///
    /// # Errors
    ///
    /// [`IdError::BadContractLength`] when the length fits no grammar,
    /// [`IdError::BadCountry`], [`IdError::BadProvider`] or
    /// [`IdError::BadInstance`] for a malformed section, and
    /// [`IdError::BadCheckDigit`] when the supplied digit is not the one the
    /// grammar computes.
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

        // 11/12 is DIN's six-character instance, 14/15 is ISO's nine — the
        // lengths do not overlap, so nothing has to be guessed. The longer
        // pair splits again on the instance's first character: a leading `C`
        // is EMI3's marker, and it is what carries a DIN identifier's six
        // digits and its check digit into a nine-character instance.
        let (grammar, carried_check_digit) = match canonical.len() {
            11 => (ContractGrammar::Din91286, false),
            12 => (ContractGrammar::Din91286, true),
            14 | 15 => {
                let emi3 = canonical.as_bytes().get(5) == Some(&b'C');
                let grammar = if emi3 {
                    ContractGrammar::Emi3
                } else {
                    ContractGrammar::Iso15118
                };
                (grammar, canonical.len() == 15)
            }
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

        let (body, supplied) = canonical.split_at(grammar.body_len());
        let computed = compute_check_digit(grammar, body).ok_or(IdError::BadInstance)?;
        if let Some(given) = supplied.chars().next()
            && given != computed
        {
            return Err(IdError::BadCheckDigit {
                grammar: grammar.as_str(),
                given,
                computed,
            });
        }

        Ok(Self {
            raw: trimmed.to_owned(),
            body: body.to_owned(),
            check_digit: computed,
            carried_check_digit,
            grammar,
            spelling,
        })
    }

    /// Parse, and require one particular grammar.
    ///
    /// Use it where the wire already says which grammar it speaks — an OICP
    /// `EvcoID` field, a DIN-only card reader — so that an identifier of the
    /// wrong shape is refused at the boundary rather than three layers in.
    ///
    /// # Errors
    ///
    /// [`IdError::WrongContractGrammar`] when the id parses as something else,
    /// plus everything [`Self::parse`] can return.
    pub fn parse_as(raw: &str, grammar: ContractGrammar) -> Result<Self, IdError> {
        let id = Self::parse(raw)?;
        if id.grammar != grammar {
            return Err(IdError::WrongContractGrammar {
                expected: grammar.as_str(),
                found: id.grammar.as_str(),
            });
        }
        Ok(id)
    }

    /// The normalised form: uppercase, separators removed, check digit present.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut out = self.body.clone();
        out.push(self.check_digit);
        out
    }

    /// The identity: country, provider and instance, without the check digit.
    ///
    /// What equality and hashing are over — the digit is derived from these, so
    /// including it would only make two spellings of one contract differ.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The ISO 3166-1 alpha-2 country code.
    #[must_use]
    pub fn country_code(&self) -> &str {
        &self.body[..2]
    }

    /// The mobility provider identifier.
    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.body[2..5]
    }

    /// The instance part — the contract itself, without the check digit.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.body[5..]
    }

    /// The check digit: the one that arrived, or the one the grammar computes
    /// when the id was written without it.
    ///
    /// Always available, because it is always derivable. Ask
    /// [`Self::carried_check_digit`] whether it was actually written down.
    #[must_use]
    pub const fn check_digit(&self) -> char {
        self.check_digit
    }

    /// Whether the identifier as it arrived carried its check digit.
    #[must_use]
    pub const fn carried_check_digit(&self) -> bool {
        self.carried_check_digit
    }

    /// Which grammar this id was read as.
    #[must_use]
    pub const fn grammar(&self) -> ContractGrammar {
        self.grammar
    }

    /// How this id was spelled where it came from.
    #[must_use]
    pub const fn spelling(&self) -> Spelling {
        self.spelling
    }

    /// The same contract as an EMI3 identifier.
    ///
    /// A DIN identifier lifts into EMI3 by becoming its own instance: `C0`,
    /// then the six-character instance, then the DIN check digit — so
    /// `NL-TNM-012204-5` and `NL-TNM-C00122045-K` are one contract written for
    /// two worlds. An identifier that is already ISO or EMI3 is returned
    /// unchanged in identity.
    ///
    /// # Errors
    ///
    /// [`IdError::BadInstance`] if the conversion cannot produce a valid
    /// identifier, which the grammars make unreachable.
    pub fn to_emi3(&self) -> Result<Self, IdError> {
        match self.grammar {
            ContractGrammar::Emi3 | ContractGrammar::Iso15118 => Ok(self.clone()),
            ContractGrammar::Din91286 => Self::parse(&format!(
                "{}{}C0{}{}",
                self.country_code(),
                self.provider_id(),
                self.instance(),
                self.check_digit,
            )),
        }
    }

    /// The same contract as a DIN SPEC 91286 identifier, when it has one.
    ///
    /// Only an EMI3 instance can go back: it has to begin `C0`, and its last
    /// character has to be the DIN check digit of what precedes it. An ISO
    /// identifier that was never a DIN one has no DIN spelling, and inventing
    /// one would mint a contract nobody issued.
    ///
    /// # Errors
    ///
    /// [`IdError::NotConvertibleToDin`] when the instance carries no DIN
    /// identifier, including when its embedded check digit does not check out.
    pub fn to_din(&self) -> Result<Self, IdError> {
        if self.grammar == ContractGrammar::Din91286 {
            return Ok(self.clone());
        }
        let instance = self.instance();
        if !instance.starts_with("C0") {
            return Err(IdError::NotConvertibleToDin {
                instance: instance.to_owned(),
            });
        }
        Self::parse(&format!(
            "{}{}{}",
            self.country_code(),
            self.provider_id(),
            &instance[2..],
        ))
        .map_err(|_| IdError::NotConvertibleToDin {
            instance: instance.to_owned(),
        })
    }
}

/// The check digit for a body, by the algorithm its grammar names.
fn compute_check_digit(grammar: ContractGrammar, body: &str) -> Option<char> {
    if grammar.uses_din_check_digit() {
        crate::check_digit::din(body)
    } else {
        crate::check_digit::iso(body)
    }
}

impl PartialEq for Emaid {
    /// Two contract ids are the same contract when their country, provider and
    /// instance agree — the check digit is a transcription guard derived from
    /// them, not part of the identity, and one wire carries it while another
    /// does not.
    ///
    /// A DIN identifier and its EMI3 lift are **not** equal, because their
    /// instances differ; [`Emaid::to_emi3`] is how a caller says it wants them
    /// compared, and saying so is the point. Treating the two as one silently
    /// would make an ISO identifier that merely happens to start `C0` collide
    /// with a DIN contract nobody issued.
    fn eq(&self, other: &Self) -> bool {
        self.body == other.body
    }
}

impl core::hash::Hash for Emaid {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.body.hash(state);
    }
}

impl PartialOrd for Emaid {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Emaid {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.body.cmp(&other.body)
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
/// An alias rather than a separate type: one value carries all three grammars,
/// [`Emaid::grammar`] records which one it was read as, and
/// [`Emaid::parse_as`] is how a wire that speaks only one says so. A separate
/// type would need a conversion at every boundary and would still not stop a
/// DIN identifier being handed to an ISO field, because the compiler cannot
/// see which wire a string came off.
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
serde_via_string!(Emaid, "a contract id such as NL-TNM-000122045-U");

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

impl FromStr for PartyId {
    type Err = IdError;

    /// Read a party from the one string it is written as.
    ///
    /// A type with a `Display` and no `FromStr` cannot round-trip, and every
    /// caller that reads a party out of a configuration file, a URL segment or
    /// a partner's registration form ends up splitting the string by hand —
    /// each in its own slightly different way, which is how `DE*ABC` and
    /// `DEABC` come to be two entries in one routing table.
    ///
    /// Both separators the field uses are accepted, and so is the packed
    /// spelling: OCPI writes the pair as two members, the EVSE-ID format joins
    /// them with `*`, the contract-ID format with `-`, and a URL path carries
    /// them bare. All five are one party.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let canonical = canonicalise(raw.trim(), &['-', '*']);
        if canonical.len() != 5 {
            return Err(IdError::BadOperator);
        }
        Self::new(&canonical[..2], &canonical[2..])
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
            EvseId::parse("NLTNM000122045U"),
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
    fn evse_id_does_not_eat_hyphens() {
        // The contract grammar separates with `-`; the EVSE grammar does not.
        // Stripping it here would make a hyphenated string that is not an EVSE
        // id parse as one — and compare equal to a real charge point.
        assert!(matches!(
            EvseId::parse("DE-AB7-E840-6487"),
            Err(IdError::BadOperator)
        ));
        // …and the `*` spelling of the same id still works.
        assert!(EvseId::parse("DE*AB7*E840*6487").is_ok());
    }

    #[test]
    fn emaid_spellings_agree_on_identity() {
        let separated: Emaid = "NL-TNM-000122045-U".parse().unwrap();
        let packed: Emaid = "NLTNM000122045U".parse().unwrap();
        let without_check: Emaid = "NLTNM000122045".parse().unwrap();

        assert_eq!(separated, packed);
        assert_eq!(
            separated, without_check,
            "the check digit is derived from the identity, not part of it"
        );
        assert_eq!(separated.provider_id(), "TNM");
        assert_eq!(separated.instance(), "000122045");
        assert_eq!(separated.check_digit(), 'U');
        assert!(separated.carried_check_digit());
        assert!(!without_check.carried_check_digit());
        assert_eq!(
            without_check.check_digit(),
            'U',
            "an omitted digit is computed rather than absent"
        );
        assert_eq!(without_check.canonical(), "NLTNM000122045U");
    }

    #[test]
    fn a_transcription_error_is_refused_rather_than_routed() {
        // The whole purpose of the digit. An identifier that has lost a
        // character still parses, still routes, and bills a session to
        // somebody else's contract.
        let err = Emaid::parse("NL-TNM-000122045-X").unwrap_err();
        assert!(
            matches!(
                err,
                IdError::BadCheckDigit {
                    given: 'X',
                    computed: 'U',
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("bill the wrong contract"), "{err}");
    }

    #[test]
    fn the_three_grammars_are_told_apart_by_shape() {
        // Six-character instance or nine, and a `C` marker on the nine. The
        // lengths do not overlap, so nothing is guessed.
        let din: Emaid = "NL-TNM-012204-5".parse().unwrap();
        assert_eq!(din.grammar(), ContractGrammar::Din91286);
        assert_eq!(din.instance(), "012204");
        assert_eq!(din.check_digit(), '5');

        let iso: Emaid = "NL-TNM-000122045-U".parse().unwrap();
        assert_eq!(iso.grammar(), ContractGrammar::Iso15118);

        let emi3: Emaid = "NL-TNM-C00122045-K".parse().unwrap();
        assert_eq!(emi3.grammar(), ContractGrammar::Emi3);
        assert_eq!(emi3.instance(), "C00122045");

        // …and a caller that already knows which wire it is on can say so.
        assert!(Emaid::parse_as("NL-TNM-012204-5", ContractGrammar::Din91286).is_ok());
        assert!(matches!(
            Emaid::parse_as("NL-TNM-012204-5", ContractGrammar::Iso15118),
            Err(IdError::WrongContractGrammar { .. })
        ));
    }

    #[test]
    fn a_din_card_and_its_emi3_lift_are_one_contract_written_twice() {
        // The conversion a German RFID card needs to reach a 15118 world:
        // `C0`, the six-character instance, then the DIN check digit.
        let din: Emaid = "NL-TNM-012204-5".parse().unwrap();
        let emi3 = din.to_emi3().unwrap();

        assert_eq!(emi3.canonical(), "NLTNMC00122045K");
        assert_eq!(emi3.grammar(), ContractGrammar::Emi3);
        assert_eq!(emi3, "NL-TNM-C00122045-K".parse::<Emaid>().unwrap());

        // …and back again, losslessly.
        assert_eq!(emi3.to_din().unwrap(), din);
        assert_eq!(din.to_din().unwrap(), din);
        assert_eq!(emi3.to_emi3().unwrap(), emi3);
    }

    #[test]
    fn an_iso_contract_that_was_never_a_din_one_has_no_din_spelling() {
        // Minting one would invent a contract nobody issued.
        let iso: Emaid = "NL-TNM-000122045-U".parse().unwrap();
        assert!(matches!(
            iso.to_din(),
            Err(IdError::NotConvertibleToDin { .. })
        ));

        // Even a `C0` prefix is not enough: the embedded digit has to check
        // out as a DIN check digit, or the "conversion" is a guess.
        let looks_convertible: Emaid = "NLTNMC00122040".parse().unwrap();
        assert_eq!(looks_convertible.grammar(), ContractGrammar::Emi3);
        assert!(matches!(
            looks_convertible.to_din(),
            Err(IdError::NotConvertibleToDin { .. })
        ));
    }

    #[test]
    fn emaid_writes_back_what_arrived() {
        for raw in [
            "NL-TNM-000122045-U",
            "NLTNM000122045U",
            "nl-tnm-000122045-u",
            "NL-TNM-012204-5",
        ] {
            let id: Emaid = raw.parse().unwrap();
            assert_eq!(id.to_string(), raw, "spelling must survive the round trip");
        }
    }

    #[test]
    fn emaid_rejects_wrong_length() {
        assert!(Emaid::parse("NLTNM").is_err());
        assert!(Emaid::parse("NLTNM0001220451234").is_err());
        // Thirteen characters is between the two grammars and belongs to
        // neither, which is a fact worth reporting rather than rounding to the
        // nearest.
        assert!(matches!(
            Emaid::parse("NLTNM00012204"),
            Err(IdError::BadContractLength { len: 13 })
        ));
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
    fn a_party_survives_the_round_trip_it_is_written_for() {
        // A `Display` with no `FromStr` is a value that cannot be read back,
        // and every caller then splits the string its own way — which is how
        // `DE*ABC` and `DEABC` become two entries in one routing table.
        let party = PartyId::new("DE", "ABC").unwrap();
        assert_eq!(party.to_string().parse::<PartyId>().unwrap(), party);

        for spelling in ["DE*ABC", "DE-ABC", "DEABC", "de*abc", "  DE*ABC  "] {
            assert_eq!(spelling.parse::<PartyId>().unwrap(), party, "{spelling}");
        }
    }

    #[test]
    fn a_party_that_is_not_a_pair_is_refused_rather_than_padded() {
        for wrong in ["DE", "DE*ABCD", "D*ABC", "DE*AB", "", "DE**ABC*X"] {
            assert!(wrong.parse::<PartyId>().is_err(), "{wrong}");
        }
    }

    #[test]
    fn opaque_ids_refuse_blanks() {
        assert!(SessionId::new("").is_err());
        assert!(SessionId::new("   ").is_err());
        assert_eq!(SessionId::new("s-1").unwrap().as_str(), "s-1");
    }
}
