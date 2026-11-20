//! Who caused a fact — the one field every journal record and every request
//! log line carries.
//!
//! `[NIS2 Art. 21(2)]` asks an essential entity for incident handling, and an
//! incident nobody can attribute is one nobody can write up: which operator
//! blocked the card, which partner pushed the record, which station sent the
//! transaction. So the answer is a value set where the caller was
//! **authenticated** — the socket's identity, the credential's party, the
//! operator token's estate name — and never what a request says about itself.

use core::fmt;

/// Who caused a fact: the principal a daemon authenticated, or the daemon's
/// own loop.
///
/// A closed set, written `kind:id` (`station:CP-1`, `partner:DE*MSP`,
/// `operator:leitstelle`, `service:csmsd`, `subscriber:erp`, `timer`, `estate`). The id is 1 to
/// 64 characters of `[A-Za-z0-9*._:=+|@ -]` — enough for an OCPP identity, an
/// OCPI party and an operator's name, and nothing that can carry a token, a
/// card UID in free text, a newline into a log, or a quote into a page. A
/// value outside it is refused at construction and when a record is read.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Actor {
    /// A station, by the identity its socket authenticated as.
    Station(String),
    /// A roaming partner, by the OCPI party its credential belongs to.
    Partner(String),
    /// An operator, by the estate's `[[operators]] name` — never the token.
    Operator(String),
    /// Another daemon of the estate, by name: a signed delivery or the
    /// service token.
    Service(String),
    /// A system outside the estate that receives events, by the estate's
    /// `[[subscribers]] name` — what answered a delivery.
    Subscriber(String),
    /// The daemon's own loop: a calendar, a drain, a retry.
    Timer,
    /// The estate file, read at start.
    Estate,
}

/// Why a text is not an [`Actor`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{0:?} is not an actor: `station:`, `partner:`, `operator:`, `service:` or `subscriber:` and 1 to 64 of [A-Za-z0-9*._:=+|@ -], or `timer`, or `estate`"
)]
pub struct ActorError(String);

fn actor_id(id: &str) -> Result<String, ActorError> {
    let fine = (1..=64).contains(&id.chars().count())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "*._:=+|@ -".contains(c));
    if fine {
        Ok(id.to_owned())
    } else {
        Err(ActorError(id.to_owned()))
    }
}

impl Actor {
    /// A station by its identity.
    ///
    /// # Errors
    ///
    /// An identity outside the actor alphabet.
    pub fn station(identity: &str) -> Result<Self, ActorError> {
        actor_id(identity).map(Self::Station)
    }

    /// A partner by its party.
    #[must_use]
    pub fn partner(party: &emob_core::PartyId) -> Self {
        // A `PartyId` is `CC*XXX` by construction, inside the alphabet.
        Self::Partner(party.to_string())
    }

    /// An operator by its estate name.
    ///
    /// # Errors
    ///
    /// A name outside the actor alphabet.
    pub fn operator(name: &str) -> Result<Self, ActorError> {
        actor_id(name).map(Self::Operator)
    }

    /// A subscriber by its estate name.
    ///
    /// # Errors
    ///
    /// A name outside the actor alphabet.
    pub fn subscriber(name: &str) -> Result<Self, ActorError> {
        actor_id(name).map(Self::Subscriber)
    }

    /// Another daemon of the estate.
    #[must_use]
    pub fn service(daemon: &'static str) -> Self {
        debug_assert!(actor_id(daemon).is_ok(), "{daemon}");
        Self::Service(daemon.to_owned())
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Station(id) => write!(f, "station:{id}"),
            Self::Partner(id) => write!(f, "partner:{id}"),
            Self::Operator(id) => write!(f, "operator:{id}"),
            Self::Service(id) => write!(f, "service:{id}"),
            Self::Subscriber(id) => write!(f, "subscriber:{id}"),
            Self::Timer => f.write_str("timer"),
            Self::Estate => f.write_str("estate"),
        }
    }
}

impl fmt::Debug for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Actor({self})")
    }
}

impl core::str::FromStr for Actor {
    type Err = ActorError;
    fn from_str(text: &str) -> Result<Self, ActorError> {
        match text {
            "timer" => return Ok(Self::Timer),
            "estate" => return Ok(Self::Estate),
            _ => {}
        }
        let refused = || ActorError(text.to_owned());
        let (kind, id) = text.split_once(':').ok_or_else(refused)?;
        let id = actor_id(id).map_err(|_| refused())?;
        match kind {
            "station" => Ok(Self::Station(id)),
            "partner" => Ok(Self::Partner(id)),
            "operator" => Ok(Self::Operator(id)),
            "service" => Ok(Self::Service(id)),
            "subscriber" => Ok(Self::Subscriber(id)),
            _ => Err(refused()),
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Actor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Actor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
