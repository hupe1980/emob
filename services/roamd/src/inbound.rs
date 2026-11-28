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
        previous: Option<&emob_service::vault::Digest>,
    ) -> Handled<Credentials> {
        let issued = (self.mint)(peer_id);
        // Digested and sealed before anything is recorded: the journal holds
        // neither token (SR-044).
        let mut node = lock(&self.node);
        let peers = node.state().peers();
        let current = peers.registered().find(|r| r.peer_id == peer_id);
        if !peers.has_offer_for(peer_id) || current.map(|r| &r.incoming) != previous {
            return Err(OcpiError::Unauthorized(
                "registration changed during discovery".into(),
            ));
        }
        let key = node.key();
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
            roles: approved_roles(node.state(), peer_id, credentials)?,
        };
        // Journalled before the peer is told, and a journal that refuses halts
        // the node: a token this node has issued and cannot reproduce is one a
        // partner will present to a `401` for ever.
        let by = partner(peer_id)?;
        node.register(&by, registration)
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

fn approved_roles(
    state: &crate::Roamd,
    peer_id: &str,
    credentials: &Credentials,
) -> Result<Vec<(emob_core::PartyId, emob_core::Role)>, OcpiError> {
    let unavailable = || OcpiError::Unauthorized("unapproved business role".into());
    let grant = state
        .registry()
        .get(&peer_id.parse().map_err(|_| unavailable())?)
        .ok_or_else(unavailable)?;
    if credentials.roles.is_empty() {
        return Err(unavailable());
    }
    credentials
        .roles
        .iter()
        .map(|r| {
            let party = emob_core::PartyId::new(r.country_code.as_str(), r.party_id.as_str())
                .map_err(|_| unavailable())?;
            let role = match r.role {
                ocpi_kit::v2_3_0::types::Role::Cpo => emob_core::Role::Cpo,
                ocpi_kit::v2_3_0::types::Role::Emsp => emob_core::Role::Emsp,
                _ => return Err(unavailable()),
            };
            grant
                .permits_role(&party, role)
                .then_some((party, role))
                .ok_or_else(unavailable)
        })
        .collect()
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

// The kit authenticates the header but omits the token from RequestContext.
// Scope it to this read so the peer can retrieve its credentials (§7.2.1)
// while the journal continues to hold only a digest of the issued token.
tokio::task_local! {
    static CREDENTIAL_READ_TOKEN: CredentialsToken;
}

impl<J: emob_service::journal::Journal> CredentialsHandler for Handshake<J> {
    async fn get(&self, _context: RequestContext) -> Handled<Credentials> {
        let mut mine = self.me.clone();
        mine.token = CREDENTIAL_READ_TOKEN
            .try_with(CredentialsToken::to_credentials_field)
            .map_err(|_| OcpiError::Unauthorized("credentials token required".into()))?;
        Ok(mine)
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
            return Err(OcpiError::MethodNotAllowed(
                "already registered: use PUT to rotate the credentials".into(),
            ));
        }
        approved_roles(
            lock(&self.node).state(),
            &context.peer.peer_id,
            &credentials,
        )?;
        let selected = self.reach_back(&credentials).await?;
        self.accept(&context.peer.peer_id, &credentials, &selected, None)
    }

    async fn put(&self, credentials: Credentials, context: RequestContext) -> Handled<Credentials> {
        let previous = {
            let node = lock(&self.node);
            let digest = node
                .state()
                .peers()
                .registered()
                .find(|r| r.peer_id == context.peer.peer_id)
                .map(|r| r.incoming.clone())
                .ok_or_else(|| OcpiError::MethodNotAllowed("registration required".into()))?;
            approved_roles(node.state(), &context.peer.peer_id, &credentials)?;
            digest
        };
        let selected = self.reach_back(&credentials).await?;
        self.accept(
            &context.peer.peer_id,
            &credentials,
            &selected,
            Some(&previous),
        )
    }

    async fn delete(&self, context: RequestContext) -> Handled<()> {
        let by = partner(&context.peer.peer_id)?;
        let mut node = lock(&self.node);
        if !node
            .state()
            .peers()
            .registered()
            .any(|r| r.peer_id == context.peer.peer_id)
        {
            return Err(OcpiError::MethodNotAllowed("registration required".into()));
        }
        node.unregister(&by, &context.peer.peer_id)
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

// OCPI CDR ids are CiString<39>. Keep the original spelling in the document.
pub(crate) fn receipt_key(key: &emob_cdr::CdrKey) -> emob_cdr::CdrKey {
    emob_cdr::CdrKey {
        party: key.party.clone(),
        id: key
            .id
            .as_str()
            .to_ascii_uppercase()
            .parse()
            .expect("case folding preserves a validated id"),
    }
}

fn handle(key: &emob_cdr::CdrKey) -> String {
    format!(
        "r1.{}.{}",
        key.party,
        hex::encode(key.id.as_str().as_bytes())
    )
}

fn parse_handle(raw: &str) -> Option<emob_cdr::CdrKey> {
    let (owner, encoded) = raw.strip_prefix("r1.")?.split_once('.')?;
    if encoded.len() > 2 * ocpi_kit::types::CiString::<39>::MAX_LEN {
        return None;
    }
    let key = emob_cdr::CdrKey {
        party: owner.parse().ok()?,
        id: String::from_utf8(hex::decode(encoded).ok()?)
            .ok()?
            .parse()
            .ok()?,
    };
    (handle(&key) == raw).then_some(key)
}

fn cpo_principal(
    state: &crate::Roamd,
    context: &RequestContext,
    owner: &emob_core::PartyId,
    cap: &'static str,
) -> Option<emob_service::Principal> {
    let peer = &context.peer;
    if peer.role != ocpi_kit::transport::TokenRole::C {
        return None;
    }
    let registered = state
        .peers()
        .registered()
        .find(|r| r.peer_id == peer.peer_id)?;
    let grant = state.registry().get(&peer.peer_id.parse().ok()?)?;
    let matches_owner = |p: &ocpi_kit::types::PartyRef| {
        p.country_code
            .as_str()
            .eq_ignore_ascii_case(owner.country_code())
            && p.party_id.as_str().eq_ignore_ascii_case(owner.party_id())
    };
    if !grant.permits_role(owner, emob_core::Role::Cpo)
        || !registered
            .roles
            .contains(&(owner.clone(), emob_core::Role::Cpo))
        || !peer.parties.iter().any(matches_owner)
        || context
            .routing
            .as_ref()
            .is_some_and(|r| !matches_owner(&r.from))
    {
        return None;
    }
    Some(emob_service::Principal::peer(
        owner.clone(),
        emob_core::Role::Cpo,
        emob_service::Capabilities::of([cap]),
    ))
}

impl<J: emob_service::journal::Journal> CdrsReceiver for Inbox<J> {
    async fn cdr(
        &self,
        cdr_id: String,
        context: RequestContext,
    ) -> Handled<ocpi_kit::v2_3_0::cdrs::Cdr> {
        let unavailable = || OcpiError::NotFound("record unavailable".into());
        let key = parse_handle(&cdr_id).ok_or_else(unavailable)?;
        let node = lock(&self.node);
        let state = node.state();
        let cap = emob_service::caps::CDR_READ;
        let principal = cpo_principal(state, &context, &key.party, cap).ok_or_else(unavailable)?;
        let document = state.document_received(&key).ok_or_else(unavailable)?;
        if node.is_halted()
            || state
                .may(
                    &principal,
                    cap,
                    &key.party,
                    document.id.as_str(),
                    time::OffsetDateTime::now_utc(),
                )
                .is_err()
        {
            return Err(unavailable());
        }
        Ok(document.clone())
    }

    async fn post_cdr(
        &self,
        cdr: ocpi_kit::v2_3_0::cdrs::Cdr,
        context: RequestContext,
    ) -> Handled<Url> {
        let at = time::OffsetDateTime::now_utc();
        let owner = emob_core::PartyId::new(cdr.country_code.as_str(), cdr.party_id.as_str())
            .map_err(|_| OcpiError::Unauthorized("unknown record owner".into()))?;
        let by = partner(&context.peer.peer_id)?;
        let verdict = {
            let mut node = lock(&self.node);
            let principal = cpo_principal(
                node.state(),
                &context,
                &owner,
                emob_service::caps::CDR_WRITE,
            )
            .ok_or_else(|| {
                OcpiError::Unauthorized(format!(
                    "the credential has no emob.cdr.write grant for {owner}"
                ))
            })?;
            node.receive(&by, &principal, &cdr, None, at)
                .map_err(|error| OcpiError::Remote {
                    status_code: ocpi_kit::transport::StatusCode::new(3000),
                    status_message: Some(error.to_string()),
                })?
        };
        let key = match &verdict {
            Verdict::Accepted(inbound) => &inbound.cdr.key,
            Verdict::Duplicate { key } => key,
            Verdict::Unauthorised { reasons } => {
                return Err(OcpiError::Unauthorized(reasons.join("; ")));
            }
            other => {
                return Err(OcpiError::Remote {
                    status_code: ocpi_kit::transport::StatusCode::new(2001),
                    status_message: Some(format!("{other:?}")),
                });
            }
        };
        Ok(Url::new_lenient(format!(
            "{}/{}",
            self.base.as_str(),
            handle(&receipt_key(key))
        )))
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
    let accept_unencoded = config.quirks.accept_unencoded_token;
    let inner = OcpiRouter::new(VersionNumber::V2_3_0, base, Arc::new(registry))
        .with_config(config)
        .credentials(handshake)
        .cdrs_receiver(inbox)
        .build()
        .route_layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| async move {
                if request.method() == http::Method::GET
                    && request.uri().path() == "/credentials"
                    && let Some(token) = request
                        .headers()
                        .get(http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| {
                            CredentialsToken::parse_header(value, accept_unencoded).ok()
                        })
                {
                    return CREDENTIAL_READ_TOKEN.scope(token, next.run(request)).await;
                }
                next.run(request).await
            },
        ));
    axum::Router::new().nest(path, inner)
}
