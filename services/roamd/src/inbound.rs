//! The HTTP surface a peer registers with and pushes to.
//!
//! Two modules, and the first is not optional: `[OCPI 2.3.0 §credentials]`
//! makes Credentials symmetric and required of every implementation. The second
//! is the one this node exists for — a partner's charge detail record, through
//! the four gates `Roamd::receive` already applies, in the order that makes them
//! mean something.
//!
//! Nothing here decides anything. Every rule is [`crate::Roamd`]'s and every document
//! is `emob-roam`'s; what is added is the socket, and the one fact a socket
//! brings with it — **who** is calling.

use std::sync::{Arc, Mutex};

use ocpi_kit::VersionNumber;
use ocpi_kit::client::{ClientConfig, OcpiClient};
use ocpi_kit::server::{
    CdrsReceiver, CredentialsHandler, Handled, OcpiRouter, RequestContext, ServerConfig,
};
use ocpi_kit::transport::{CredentialsToken, OcpiError};
use ocpi_kit::types::Url;
use ocpi_kit::v2_3_0::credentials::Credentials;

use crate::peering::{Registration, Registry, served};
use crate::{Node, Verdict};
use emob_service::Actor;

/// How a registration's own token is minted.
///
/// An argument rather than a `rand` call inside the handler, for the reason
/// every instant in this workspace is one: a handshake replayed in a test has to
/// produce the same bytes, and a token generator reached through ambient
/// randomness is the one thing in a handshake that cannot be replayed.
pub type MintToken = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// The credentials module: registration, rotation and unregistration.
pub struct Handshake<J: emob_service::journal::Journal> {
    /// This node's own credentials, as it hands them to a peer.
    me: Credentials,
    node: Arc<Mutex<Node<J>>>,
    client: OcpiClient,
    mint: MintToken,
    reach: crate::reach::ReachBack,
}

impl<J: emob_service::journal::Journal> Handshake<J> {
    /// A handler over this node's own credentials object.
    ///
    /// # Errors
    ///
    /// The `reqwest` failure when no TLS backend is available, as text — see
    /// `dispatch::DispatchSetupError` for why the type is not named.
    /// `reach` is the guard every reach-back passes before a request is made
    /// (SR-037): the router is never mounted without one.
    pub fn new(
        me: Credentials,
        node: Arc<Mutex<Node<J>>>,
        mint: MintToken,
        config: ClientConfig,
        reach: crate::reach::ReachBack,
    ) -> Result<Self, String> {
        Ok(Self {
            me,
            node,
            client: OcpiClient::with_config(config).map_err(|error| error.to_string())?,
            mint,
            reach,
        })
    }

    /// The step `[OCPI 2.3.0 §credentials]` will not let a registration skip.
    ///
    /// > *When the initializing party requests data from the other party during
    /// > the open POST call to its credentials endpoint. If one of the GETs can
    /// > not be processed, the party should return this error in the POST
    /// > response.*
    ///
    /// A peer that hands over a token nobody can use has not registered, and
    /// recording it would leave this node holding a credential for a platform it
    /// cannot reach. The failure is `3001`, which is what the paragraph names.
    async fn reach_back(
        &self,
        credentials: &Credentials,
    ) -> Result<ocpi_kit::client::Selected, OcpiError> {
        // Before anything is sent: a registrant's URL is a request this node
        // makes on its behalf.
        self.reach
            .admit(credentials.url.as_str())
            .await
            .map_err(|why| OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(3001),
                status_message: Some(why),
            })?;
        let token = CredentialsToken::new(credentials.token.as_str())
            .map_err(|error| OcpiError::Unauthorized(error.to_string()))?;
        let discovered = ocpi_kit::client::Registration::new(credentials.url.clone(), token)
            .discover(self.client.transport())
            .await
            .map_err(|error| OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(3001),
                status_message: Some(format!(
                    "this node could not read {}'s versions with the token it was given, so \
                         the registration would leave it holding a credential for a platform it \
                         cannot reach: {error}",
                    credentials.url.as_str()
                )),
            })?;
        // `select_best` fetches the **version details**, which is where the
        // peer's endpoints are. Reading the versions list alone tells this node
        // where the peer's directory is and not where to push a record — and
        // this is the only moment the endpoints are on offer (D330).
        discovered
            .select_best(self.client.transport())
            .await
            .map_err(|error| OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(3002),
                status_message: Some(format!(
                    "{} published no version details this node can use, so there is nothing to \
                     register at: {error}",
                    credentials.url.as_str()
                )),
            })
    }

    /// Turn a peer's credentials, once reached, into the registration this node
    /// keeps — and the credentials it answers with.
    fn accept(
        &self,
        peer_id: &str,
        credentials: &Credentials,
        selected: &ocpi_kit::client::Selected,
    ) -> Handled<Credentials> {
        let issued = (self.mint)(peer_id);
        // Digested and sealed before anything is recorded: the journal holds
        // neither token (SR-044).
        let key = lock(&self.node).key();
        let registration = Registration {
            peer_id: peer_id.to_owned(),
            incoming: key.digest(crate::peering::INCOMING, issued.as_bytes()),
            outgoing: key.seal_str(crate::peering::OUTGOING, credentials.token.as_str()),
            versions_url: credentials.url.as_str().to_owned(),
            version: selected.version().to_string(),
            endpoints: selected
                .details()
                .endpoints
                .iter()
                .map(|endpoint| crate::peering::Endpoint {
                    module: endpoint.identifier.to_string(),
                    role: endpoint.role.to_string(),
                    url: endpoint.url.as_str().to_owned(),
                })
                .collect(),
            parties: credentials
                .roles
                .iter()
                .map(|role| {
                    (
                        role.country_code.as_str().to_owned(),
                        role.party_id.as_str().to_owned(),
                    )
                })
                .collect(),
        };
        // Journalled before the peer is told, and a journal that refuses halts
        // the node: a token this node has issued and cannot reproduce is one a
        // partner will present to a `401` for ever.
        let by = partner(peer_id)?;
        lock(&self.node)
            .register(&by, registration)
            .map_err(|error| OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(3000),
                status_message: Some(format!(
                    "this node could not record the registration, so it will not issue a token it \
                     cannot reproduce: {error}"
                )),
            })?;
        let mut mine = self.me.clone();
        mine.token = issued.into();
        Ok(mine)
    }
}

/// Who a credentials call is from: the party the presented token resolved to
/// — an estate partner's `Token A`, or the token this node issued it — and
/// never one the request body names.
fn partner(peer_id: &str) -> Result<Actor, OcpiError> {
    peer_id
        .parse::<emob_core::PartyId>()
        .map(|party| Actor::partner(&party))
        .map_err(|error| {
            OcpiError::Unauthorized(format!("the credential's platform is not a party: {error}"))
        })
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<J: emob_service::journal::Journal> CredentialsHandler for Handshake<J> {
    async fn get(&self, _context: RequestContext) -> Handled<Credentials> {
        Ok(self.me.clone())
    }

    async fn post(
        &self,
        credentials: Credentials,
        context: RequestContext,
    ) -> Handled<Credentials> {
        if lock(&self.node)
            .state()
            .peers()
            .registered()
            .any(|held| held.peer_id == context.peer.peer_id)
        {
            // `[OCPI 2.3.0 §credentials]`: a POST to a platform that is already
            // registered is `2002`, and the remedy is PUT. Answering `200` here
            // would mint a second token for one platform, and the older one
            // would keep working.
            return Err(OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(2002),
                status_message: Some(
                    "this platform is already registered: rotate with PUT, or DELETE first"
                        .to_owned(),
                ),
            });
        }
        let selected = self.reach_back(&credentials).await?;
        self.accept(&context.peer.peer_id, &credentials, &selected)
    }

    async fn put(&self, credentials: Credentials, context: RequestContext) -> Handled<Credentials> {
        // > *The server must fetch the client's endpoints again, even if the
        // > version has not changed.*
        let selected = self.reach_back(&credentials).await?;
        self.accept(&context.peer.peer_id, &credentials, &selected)
    }

    async fn delete(&self, context: RequestContext) -> Handled<()> {
        let by = partner(&context.peer.peer_id)?;
        lock(&self.node)
            .unregister(&by, &context.peer.peer_id)
            .map_err(|error| OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(3000),
                status_message: Some(error.to_string()),
            })
    }
}

/// The CDRs module's receiving half.
///
/// `base` is the endpoint this node **advertises** for the module, because the
/// `Location` it returns has to be a URL the sender can `GET`. It is computed
/// by [`router`] rather than supplied, for the reason the path and the
/// advertised URL are one argument there: handed in separately, a `Location`
/// pointing at a 404 is a response that looks perfect and is dead (D333).
pub struct Inbox<J: emob_service::journal::Journal> {
    node: Arc<Mutex<Node<J>>>,
    base: Url,
}

impl<J: emob_service::journal::Journal> CdrsReceiver for Inbox<J> {
    async fn cdr(
        &self,
        cdr_id: String,
        _context: RequestContext,
    ) -> Handled<ocpi_kit::v2_3_0::cdrs::Cdr> {
        // `[OCPI 2.3.0 §mod_cdrs]` makes this the URL the `Location` header
        // named, so a sender can read back **what it sent**. So this returns
        // the document that arrived rather than a rebuild of it from the
        // canonical model: the two are not the same document, and answering
        // with the second would answer a question about the partner's
        // arithmetic with a claim about ours. An inbox holds the message, which
        // is D324's rule pointed inward.
        lock(&self.node)
            .state()
            .document_received(&cdr_id)
            .cloned()
            .ok_or_else(|| OcpiError::NotFound(format!("no record {cdr_id} was pushed here")))
    }

    async fn post_cdr(
        &self,
        cdr: ocpi_kit::v2_3_0::cdrs::Cdr,
        context: RequestContext,
    ) -> Handled<Url> {
        let id = cdr.id.as_str().to_owned();
        let at = time::OffsetDateTime::now_utc();

        // The principal comes from the **credential**, never from the document.
        //
        // `Roamd::receive` asks whether this caller may write records for the
        // party the document names. Building the principal out of that same
        // document makes the question compare a claim with itself, and every
        // push self-authorises: a partner could settle records in any operator's
        // name by writing that name in the payload. A check fed by the thing it
        // is checking is not a check (rule 3, D332).
        //
        // A platform speaks for the parties its credentials listed. It may push
        // for any of them and for nobody else, which is also what the wire layer
        // enforces one step earlier with a 404 `[OCPI 2.3.0
        // §transport_and_format]` — belt and braces, deliberately: the outer
        // check is the kit's and could be configured away, and this one decides
        // what reaches the ledger.
        let speaking_for = context.peer.parties.first().ok_or_else(|| {
            OcpiError::Unauthorized(
                "the credential names no party, so there is nobody this push could be for"
                    .to_owned(),
            )
        })?;
        let party = emob_core::PartyId::new(
            speaking_for.country_code.as_str(),
            speaking_for.party_id.as_str(),
        )
        .map_err(|error| OcpiError::Unauthorized(error.to_string()))?;
        let by = Actor::partner(&party);
        let principal = emob_service::Principal::peer(
            party,
            emob_core::Role::Cpo,
            emob_service::Capabilities::of([emob_service::caps::CDR_WRITE]),
        );
        let verdict = {
            let mut node = lock(&self.node);
            node.receive(&by, &principal, &cdr, None, at)
                .map_err(|error| OcpiError::Remote {
                    status_code: ocpi_kit::transport::StatusCode::new(3000),
                    status_message: Some(error.to_string()),
                })?
        };
        match verdict {
            // A retry is the first delivery's answer again, which is what makes
            // a re-send safe for the sender (D325, from the other end).
            Verdict::Accepted(_) | Verdict::Duplicate { .. } => {
                Ok(Url::new_lenient(format!("{}/{id}", self.base.as_str())))
            }
            Verdict::Unauthorised { reasons } => Err(OcpiError::Unauthorized(reasons.join("; "))),
            other => Err(OcpiError::Remote {
                status_code: ocpi_kit::transport::StatusCode::new(2001),
                status_message: Some(format!("{other:?}")),
            }),
        }
    }
}

/// The router this node serves: versions, credentials, and the CDRs receiver.
///
/// # The path and the advertised URL are one decision
///
/// The kit mounts its routes at the router's **root** — `/versions`,
/// `/credentials`, `/cdrs` — and advertises `base_url.join(module)` in the
/// version-details document. So the nesting and the base URL have to agree, and
/// getting them apart is the failure that looks like nothing: this node answers
/// perfectly on paths nobody was told about, and every peer reads a document
/// pointing at 404s.
///
/// They are one argument here for that reason. `base` is where this node is
/// reachable, the router is nested at that path, and a caller cannot set one
/// without the other.
///
/// `served()` is the module list advertised and the same list mounted — a module
/// in the document and not on the router is a peer told to call an endpoint that
/// answers 404.
pub fn router<J: emob_service::journal::Journal>(
    base: Url,
    path: &str,
    node: Arc<Mutex<Node<J>>>,
    registry: Registry<J>,
    handshake: Handshake<J>,
) -> axum::Router {
    debug_assert!(
        served().contains(&ocpi_kit::ModuleId::Cdrs)
            && served().contains(&ocpi_kit::ModuleId::Credentials),
        "the advertised module list and the mounted routes are one list"
    );
    debug_assert!(
        base.as_str().ends_with(path.trim_end_matches('/')),
        "the path this router is nested at and the URL it advertises have to agree, or every \
         peer reads a document pointing at 404s: base {base:?} does not end with {path:?}"
    );
    let config = ServerConfig::default();
    // The one place the Receiver's own URL is worked out. `version_details`
    // advertises `base / prefix / module` for a Receiver endpoint, so the
    // `Location` a push gets back is built the same way, from the same values.
    let mut receiver = base.clone();
    if let Some(prefix) = &config.receiver_path_prefix {
        receiver = receiver.join(prefix);
    }
    let inbox = Inbox {
        node,
        base: receiver.join(ocpi_kit::ModuleId::Cdrs.as_str()),
    };
    let inner = OcpiRouter::new(VersionNumber::V2_3_0, base, Arc::new(registry))
        .with_config(config)
        .credentials(handshake)
        .cdrs_receiver(inbox)
        .build();
    axum::Router::new().nest(path, inner)
}
