//! The module OCPI requires of everybody, and the ledger under it.
//!
//! > *The Credentials module is symmetric and has to be implemented by all OCPI
//! > implementations: all implementations need to be able to call this module on
//! > any other platform and handle receiving requests from another party.*
//! > `[OCPI 2.3.0 §credentials]`
//!
//! A node that can push a document and cannot be **registered with** is not an
//! OCPI implementation. What is here is the receiving half: the versions
//! endpoint a peer discovers this node through, the credentials handshake it
//! registers over, and the token store that decides whether a later request is
//! anybody's.
//!
//! # Registration is a fact about the past
//!
//! The tokens are the whole of the authentication: a node that forgot them on a
//! restart answers `401` to every partner it has ever onboarded, and — worse —
//! re-issues `CREDENTIALS_TOKEN_C` values that partners are still presenting.
//! So a registration is journalled like every other decision this node makes,
//! and the store is a projection of the journal rather than a second place the
//! answer lives.
//!
//! # What `POST credentials` must do before it answers
//!
//! `[OCPI 2.3.0 §credentials]` requires the receiver to fetch the client's
//! versions **and** version details with the token it was just given, and to
//! answer `3001` when that fails. That is the one step a registration cannot
//! skip: a peer that hands over a token nobody can use has not registered, and
//! recording it would leave this node holding a credential for a platform it
//! cannot reach.

use std::sync::{Arc, Mutex};

use emob_core::PartyId;
use ocpi_kit::server::{AuthenticatedPeer, TokenStore};
use ocpi_kit::transport::{CredentialsToken, TokenRole};
use ocpi_kit::types::PartyRef;
use ocpi_kit::{ModuleId, VersionNumber};

/// One endpoint a peer published.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Endpoint {
    /// The module, as OCPI spells it.
    pub module: String,
    /// Which interface of it — `SENDER` or `RECEIVER`.
    pub role: String,
    /// Where it is.
    pub url: String,
}

/// One registered platform, as this node holds it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Registration {
    /// The platform's own identifier, for logging and for the ledger.
    pub peer_id: String,
    /// The token **this** node issued, which the peer presents on every later
    /// request. `CREDENTIALS_TOKEN_C`.
    pub incoming: String,
    /// The token the **peer** issued, which this node presents to it.
    pub outgoing: String,
    /// Where the peer's versions endpoint is.
    pub versions_url: String,
    /// Which version the connection was registered at.
    pub version: String,
    /// The endpoints the peer published for that version — module, role, URL.
    ///
    /// Read off the version-details document during the handshake and kept,
    /// because this is the **only** moment they are known and the only place
    /// they are written down. A node that recorded the versions URL and threw
    /// the endpoints away knows where a peer's directory is and not where to
    /// push a record to it, which is rule 1 pointed at a fetch rather than at a
    /// field (D330).
    pub endpoints: Vec<Endpoint>,
    /// The parties the platform speaks for, as `country` and `party_id`.
    pub parties: Vec<(String, String)>,
}

impl Registration {
    /// Where this peer takes a record of `module` in `role`, if it published
    /// one.
    #[must_use]
    pub fn endpoint(&self, module: &str, role: &str) -> Option<&str> {
        self.endpoints
            .iter()
            .find(|endpoint| {
                endpoint.module.eq_ignore_ascii_case(module)
                    && endpoint.role.eq_ignore_ascii_case(role)
            })
            .map(|endpoint| endpoint.url.as_str())
    }

    /// The peer as the **client** addresses it: the token this node presents,
    /// the version registered, and the endpoints the peer published.
    ///
    /// Built from the registration and nowhere else, which is the point: a
    /// second description of a partner is a second answer to *where does a
    /// record go*, out of date the moment a token is rotated (D330).
    ///
    /// # Errors
    ///
    /// [`PeerError`] when the token the peer issued is not one the
    /// specification's character set allows, or its party is not a party.
    pub fn as_client_peer(&self) -> Result<ocpi_kit::client::Peer, PeerError> {
        let token = CredentialsToken::new(self.outgoing.clone()).map_err(PeerError::Token)?;
        let version = self
            .version
            .parse()
            .unwrap_or(ocpi_kit::VersionNumber::V2_3_0);
        let mut peer = ocpi_kit::client::Peer::builder(version, token);
        for endpoint in &self.endpoints {
            if let (Ok(module), Ok(role)) = (
                endpoint.module.parse::<ModuleId>(),
                endpoint.role.parse::<ocpi_kit::InterfaceRole>(),
            ) {
                peer = peer.endpoint(
                    module,
                    role,
                    ocpi_kit::types::Url::new_lenient(&endpoint.url),
                );
            }
        }
        for (country, party) in &self.parties {
            peer = peer
                .party(PartyRef::new(country.clone(), party.clone()).map_err(PeerError::Party)?);
        }
        Ok(peer.build())
    }

    /// The peer as the server's authenticator holds it.
    fn as_peer(&self) -> AuthenticatedPeer {
        AuthenticatedPeer {
            peer_id: self.peer_id.clone(),
            // Always `C`: a registration that completed is past the bootstrap
            // token, and a `Token A` that still opened the CDRs module would be
            // the whole handshake for nothing.
            role: TokenRole::C,
            parties: self
                .parties
                .iter()
                .filter_map(|(country, party)| PartyRef::new(country.clone(), party.clone()).ok())
                .collect(),
            version: self
                .version
                .parse()
                .unwrap_or(ocpi_kit::VersionNumber::V2_3_0),
        }
    }

    /// Whether this platform speaks for a party.
    #[must_use]
    pub fn speaks_for(&self, party: &PartyId) -> bool {
        self.parties.iter().any(|(country, id)| {
            country.eq_ignore_ascii_case(party.country_code())
                && id.eq_ignore_ascii_case(party.party_id())
        })
    }
}

/// Everything this node has registered, and the bootstrap tokens it has handed
/// out but not yet seen used.
#[derive(Debug, Default)]
pub struct Peers {
    pub(crate) registered: Vec<Registration>,
    /// `CREDENTIALS_TOKEN_A`s issued out of band, each naming the platform it
    /// was handed to.
    ///
    /// Kept apart from the registrations rather than in the same list, because
    /// what a `Token A` may reach is different: `versions` and `credentials`
    /// and nothing else `[OCPI 2.3.0 §transport_and_format]`. One list with a
    /// role field would make that a value somebody has to check.
    pub(crate) bootstrap: Vec<(String, String)>,
}

impl Peers {
    /// A node nobody has registered with.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a bootstrap token handed to a platform out of band.
    pub fn offer(&mut self, peer_id: impl Into<String>, token: impl Into<String>) {
        self.bootstrap.push((token.into(), peer_id.into()));
    }

    /// Record a completed registration, replacing any earlier one for the same
    /// platform.
    pub fn register(&mut self, registration: Registration) {
        // The bootstrap token is spent the moment the registration completes:
        // `[OCPI 2.3.0 §credentials]` makes `Token A` valid only until the
        // exchange is finished, and a node that left it working would keep a
        // second credential for the same platform with a wider window than the
        // one it just issued.
        self.bootstrap
            .retain(|(_, peer)| peer != &registration.peer_id);
        self.registered
            .retain(|held| held.peer_id != registration.peer_id);
        self.registered.push(registration);
    }

    /// Forget a platform — `DELETE credentials`, which unregisters.
    pub fn unregister(&mut self, peer_id: &str) {
        self.registered.retain(|held| held.peer_id != peer_id);
        self.bootstrap.retain(|(_, peer)| peer != peer_id);
    }

    /// Every registered platform.
    pub fn registered(&self) -> impl Iterator<Item = &Registration> {
        self.registered.iter()
    }

    /// The platform registered for a party, where there is one.
    #[must_use]
    pub fn for_party(&self, party: &PartyId) -> Option<&Registration> {
        self.registered.iter().find(|held| held.speaks_for(party))
    }
}

/// A [`TokenStore`] over the node's own folded state.
///
/// Reading through [`crate::Node`] rather than through a second copy is the
/// point: the registrations are journalled, so the tokens a partner presents
/// after a restart are the ones this node issued before it. A store beside the
/// node would be a second place the answer lives, and rule 16's own sentence.
///
/// The lookup is a linear scan with a constant-time comparison per entry, which
/// is the kit's own trade-off and the right one up to a few thousand peers.
pub struct Registry<J: emob_service::journal::Journal>(Arc<Mutex<crate::Node<J>>>);

impl<J: emob_service::journal::Journal> Clone for Registry<J> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<J: emob_service::journal::Journal> core::fmt::Debug for Registry<J> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Registry")
    }
}

impl<J: emob_service::journal::Journal> Registry<J> {
    /// A registry over the node's own peer list.
    #[must_use]
    pub const fn new(node: Arc<Mutex<crate::Node<J>>>) -> Self {
        Self(node)
    }
}

impl<J: emob_service::journal::Journal> TokenStore for Registry<J> {
    fn resolve(&self, token: &CredentialsToken) -> Option<AuthenticatedPeer> {
        let node = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let peers = node.state().peers();
        if let Some(registration) = peers.registered.iter().find(|entry| {
            CredentialsToken::new(entry.incoming.clone()).is_ok_and(|held| &held == token)
        }) {
            return Some(registration.as_peer());
        }
        // A bootstrap token reaches `versions` and `credentials` and nothing
        // else, which `TokenRole::A` is what says — the kit checks the module
        // against the role, so this does not have to.
        peers
            .bootstrap
            .iter()
            .find(|(offered, _)| {
                CredentialsToken::new(offered.clone()).is_ok_and(|held| &held == token)
            })
            .map(|(_, peer_id)| AuthenticatedPeer {
                peer_id: peer_id.clone(),
                role: TokenRole::A,
                parties: Vec::new(),
                version: VersionNumber::V2_3_0,
            })
    }
}

/// Why a peer could not be described from its registration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PeerError {
    /// The token the peer issued is not one `[OCPI 2.3.0 §credentials]` allows.
    #[error("the credentials token is not one the specification allows: {0}")]
    Token(ocpi_kit::transport::InvalidToken),

    /// The partner is not a party an OCPI routing header can carry.
    #[error("the partner is not a party an OCPI routing header can carry: {0}")]
    Party(ocpi_kit::types::InvalidString),
}

/// The modules this node serves, for the version-details document a peer reads
/// before it registers.
///
/// Named here rather than derived from the router so that adding a handler and
/// advertising it are one change: a module in the document and not on the
/// router is a peer told to call an endpoint that answers 404, and the reverse
/// is an endpoint nobody will ever call.
#[must_use]
pub const fn served() -> &'static [ModuleId] {
    &[ModuleId::Credentials, ModuleId::Cdrs]
}
