//! The token OCPI requires and a canonical session deliberately does not hold.
//!
//! # A gap that is a design, not an omission
//!
//! OCPI makes `cdr_token` a required member, and its `uid` is *"the unique ID
//! by which this Token can be identified"* — for an RFID card, the UID printed
//! into the chip. [`emob_session::Authorization`] refuses to store that. It
//! keeps a [`TokenRef`](emob_session::TokenRef): a keyed hash, constructed so
//! that a caller who passes a raw UID gets an error rather than a privacy
//! incident. The reasoning is in that crate — a UID is a lifelong identifier
//! of a physical object a person carries, and a session row holding one builds
//! a movement profile nothing in this platform needs.
//!
//! Those two facts do not compose, and the resolution is not to weaken either.
//! A [`RoamingToken`] is **presented to the crossing** by the party that holds
//! the mapping — the token store, which is a service with a key and a database
//! and no place in a domain crate. So the UID appears exactly at the edge that
//! has to send it, on exactly the records that are leaving, and nowhere else.
//!
//! The shape of the argument is the one used throughout this workspace, in the
//! other direction: an [`EvidenceRef`](emob_cdr::EvidenceRef) is *read off*
//! the evidence rather than filled in, because a hand-filled field defeats the
//! check it feeds. Here nothing is being checked, so the field is an argument
//! — and the type system says the CDR could not have leaked it, because the
//! CDR never had it.
//!
//! # The check digit is verified here, because here is the last place anyone
//! looks
//!
//! A contract id ends in a digit whose only job is to catch a transcription
//! error. Once the record is at the eMSP, an id that has lost a character
//! still parses, still routes, and bills the session to somebody else's
//! contract — and the CPO has already been paid, so nobody has a reason to
//! look. [`RoamingToken::new`] refuses it.
//!
//! `ocpi-kit` parses the check digit off a contract id and does not verify it,
//! which is right for a wire library — a peer's malformed id must still
//! decode, or one bad record poisons a page. Verifying is this layer's job,
//! and `emob-core` knows all three grammars and both algorithms.

use emob_core::{ContractId, Emaid, PartyId};

use crate::error::RoamError;

/// What kind of credential the driver presented.
///
/// OCPI's own list `[OCPI 2.3.0 §mod_tokens_tokentype_enum]`. It is a
/// narrower question than [`AuthPath`](emob_session::AuthPath) asks: the path
/// is *how the session was authorised*, this is *what was held up*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    /// A one-time id generated for a driver with no contract.
    AdHocUser,
    /// An app user's id.
    AppUser,
    /// An EMAID, presented over ISO 15118.
    Emaid,
    /// An RFID card.
    Rfid,
    /// Something else.
    Other,
}

impl TokenType {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdHocUser => "AD_HOC_USER",
            Self::AppUser => "APP_USER",
            Self::Emaid => "EMAID",
            Self::Rfid => "RFID",
            Self::Other => "OTHER",
        }
    }
}

impl From<TokenType> for ocpi_kit::v2_3_0::tokens::TokenType {
    fn from(value: TokenType) -> Self {
        match value {
            TokenType::AdHocUser => Self::AdHocUser,
            TokenType::AppUser => Self::AppUser,
            TokenType::Emaid => Self::Emaid,
            TokenType::Rfid => Self::Rfid,
            TokenType::Other => Self::Other,
        }
    }
}

/// The token a CDR names on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoamingToken {
    /// The party that issued the token — the eMSP, not the operator.
    pub issuer: PartyId,
    /// The token's own unique id.
    pub uid: String,
    /// What was presented.
    pub token_type: TokenType,
    /// The contract the session bills against.
    pub contract_id: ContractId,
}

impl RoamingToken {
    /// Present a token for a crossing, verifying the identifier that routes
    /// the money.
    ///
    /// # Errors
    ///
    /// [`RoamError::ContractCheckDigit`] when the contract id is in one of the
    /// three grammars this workspace knows and its check digit is not the one
    /// that grammar computes.
    ///
    /// An id whose **shape** matches no grammar is accepted, because an eMSP
    /// is free to use its own scheme and refusing one would make this crate the
    /// reason a perfectly ordinary partner cannot be settled with.
    ///
    /// Shape is the test, and it is not a registry of schemes anybody opted
    /// into: a grammar is chosen by length and character class alone — 11 or
    /// 12 characters is DIN SPEC 91286, 14 or 15 is ISO 15118-1 or eMI3. So a
    /// provider's own scheme that *happens* to be twelve alphanumerics
    /// beginning with two letters is checked as a DIN identifier and can be
    /// refused by it. That is the right answer rather than a false positive:
    /// such an id is indistinguishable from a DIN card with a character
    /// transcribed, and telling those apart is the whole job of the digit.
    /// [`Self::canonical_contract`] returns `None` for an id nothing checked,
    /// which is how a caller finds out that this refusal could not have fired.
    pub fn new(
        issuer: PartyId,
        uid: impl Into<String>,
        token_type: TokenType,
        contract_id: ContractId,
    ) -> Result<Self, RoamError> {
        if let Err(emob_core::IdError::BadCheckDigit { .. }) = Emaid::parse(contract_id.as_str()) {
            return Err(RoamError::ContractCheckDigit {
                id: contract_id.as_str().to_owned(),
            });
        }
        Ok(Self {
            issuer,
            uid: uid.into(),
            token_type,
            contract_id,
        })
    }

    /// The contract in its canonical form, when it is in a grammar this
    /// workspace knows.
    ///
    /// Two spellings of one contract — `NL-TNM-012204-5` on a German card and
    /// `NL-TNM-C00122045-K` in an ISO certificate — are the same driver, and a
    /// partner keying its whitelist on the text will not think so. `None` for
    /// a provider's own scheme, which is *"not comparable"* rather than *"no
    /// match"*.
    #[must_use]
    pub fn canonical_contract(&self) -> Option<String> {
        Emaid::parse(self.contract_id.as_str())
            .ok()
            .map(|id| id.canonical())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(code: &str, id: &str) -> PartyId {
        PartyId::new(code, id).expect("a valid party")
    }

    fn contract(text: &str) -> ContractId {
        text.parse().expect("a valid contract id")
    }

    #[test]
    fn a_transcribed_contract_id_does_not_leave() {
        // `NL-TNM-000122045-U` is the ISO 15118-1 reference vector. One
        // character of the check digit wrong is a session billed to a contract
        // nobody holds, and this is the last place anybody looks at it.
        let err = RoamingToken::new(
            party("NL", "TNM"),
            "045F2C",
            TokenType::Rfid,
            contract("NL-TNM-000122045-X"),
        )
        .unwrap_err();
        assert!(matches!(err, RoamError::ContractCheckDigit { .. }));
    }

    #[test]
    fn the_reference_vector_crosses() {
        let token = RoamingToken::new(
            party("NL", "TNM"),
            "045F2C",
            TokenType::Rfid,
            contract("NL-TNM-000122045-U"),
        )
        .unwrap();
        assert_eq!(token.canonical_contract().unwrap(), "NLTNM000122045U");
    }

    #[test]
    fn a_providers_own_scheme_is_not_comparable_rather_than_invalid() {
        // Refusing this would make this crate the reason an ordinary partner
        // cannot be settled with. Seventeen characters is no grammar's shape.
        let token = RoamingToken::new(
            party("DE", "XYZ"),
            "u-9931",
            TokenType::AppUser,
            contract("acct-9931-2026-fleet"),
        )
        .expect("a scheme whose shape matches no grammar is not an invalid one");
        assert_eq!(
            token.canonical_contract(),
            None,
            "nothing checked it, and a caller has to be able to find that out"
        );
    }

    #[test]
    fn a_scheme_that_borrows_a_grammars_shape_is_checked_as_that_grammar() {
        // `acct-9931-2026` strips to twelve alphanumerics beginning with two
        // letters, which is exactly a DIN EVCOID carrying its check digit.
        // Passing it through unchecked would mean a DIN card with one
        // character transcribed also passes — and telling those two apart is
        // the entire job of the digit.
        let err = RoamingToken::new(
            party("DE", "XYZ"),
            "u-9931",
            TokenType::AppUser,
            contract("acct-9931-2026"),
        )
        .unwrap_err();
        assert!(matches!(err, RoamError::ContractCheckDigit { .. }), "{err}");
    }

    #[test]
    fn two_spellings_of_one_contract_canonicalise_together() {
        // A German card and an ISO certificate, one driver. A partner keying a
        // whitelist on the text will not think so.
        let din = RoamingToken::new(
            party("NL", "TNM"),
            "a",
            TokenType::Rfid,
            contract("NL-TNM-012204-5"),
        )
        .unwrap();
        let emi3 = RoamingToken::new(
            party("NL", "TNM"),
            "b",
            TokenType::Emaid,
            contract("NL-TNM-C00122045-K"),
        )
        .unwrap();

        assert_ne!(din.contract_id, emi3.contract_id);
        assert_ne!(
            din.canonical_contract(),
            emi3.canonical_contract(),
            "the two grammars canonicalise within themselves; converting between them \
             is `Emaid::to_emi3` and is deliberately explicit"
        );
    }
}
