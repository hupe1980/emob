//! The HTTP surface every daemon mounts, and the shape of the one it adds.
//!
//! # What is here and what is not
//!
//! Three routes, and they are the three an orchestrator needs: `/health/live`,
//! `/health/ready` and `/about`. A daemon adds its own router beside them.
//!
//! There is no route here that answers a question about charging, because a
//! route that answered one would be a rule living in the shell — and a rule in
//! the shell is a rule CI does not run. The daemons are thin for the same
//! reason `csmsd` is: everything that could be *wrong* is in a domain crate,
//! under test there.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;

use crate::health::{Identity, Readiness};
use crate::shutdown::Shutdown;

/// Where a daemon listens, from the environment.
///
/// Three answers in order, and the order is the point.
///
/// 1. `<DAEMON>_HTTP_BIND` — the specific one, for an operator running two of
///    something on one host.
/// 2. `EMOB_HTTP_BIND` — the estate-wide one. A container image sets this, and
///    it is the reason it exists: **the default is loopback, and loopback
///    inside a container is unreachable.** A daemon that came up healthy,
///    logged that it was listening, and answered nothing is the failure this
///    closes — found by running the image rather than by reading it.
/// 3. `fallback` — the daemon's own default, which is a **local** address on
///    purpose. A platform that decides money should not become reachable
///    because somebody forgot to configure it.
///
/// # Errors
///
/// [`ServerError::Bind`] when whichever value was found is not a socket
/// address, naming the variable it came from — because "invalid socket address"
/// with three possible sources is a message that costs an hour.
pub fn bind_address(daemon: &str, fallback: &str) -> Result<SocketAddr, ServerError> {
    let specific = format!("{}_HTTP_BIND", daemon.to_ascii_uppercase());
    let (from, text) = std::env::var(&specific)
        .map(|value| (specific.clone(), value))
        .or_else(|_| {
            std::env::var("EMOB_HTTP_BIND").map(|value| ("EMOB_HTTP_BIND".to_owned(), value))
        })
        .unwrap_or_else(|_| ("the default".to_owned(), fallback.to_owned()));
    text.parse()
        .map_err(|_| ServerError::BadAddress { from, text })
}

/// Where a daemon's **second** socket listens — a CSMS's OCPP endpoint, a site
/// controller's SHIP session.
///
/// Its own variable and **not** `EMOB_HTTP_BIND`: the estate-wide one names the
/// address the shell answers on, and a daemon that applied it to two sockets
/// would bind the second to the first's port and fail at start-up with a
/// message about an address already in use.
///
/// # Errors
///
/// [`ServerError::BadAddress`] when the value is not a socket address.
pub fn bind_socket(variable: &str, fallback: &str) -> Result<SocketAddr, ServerError> {
    let (from, text) = std::env::var(variable).map_or_else(
        |_| ("the default".to_owned(), fallback.to_owned()),
        |value| (variable.to_owned(), value),
    );
    text.parse()
        .map_err(|_| ServerError::BadAddress { from, text })
}

/// What the infrastructure routes read.
#[derive(Debug, Clone)]
struct Infra {
    identity: Identity,
    readiness: Readiness,
}

/// A daemon's HTTP server.
#[derive(Debug)]
pub struct Server {
    identity: Identity,
    readiness: Readiness,
    shutdown: Shutdown,
    routes: Router,
    drain: std::time::Duration,
}

impl Server {
    /// A server with the infrastructure routes and nothing else.
    #[must_use]
    pub fn new(identity: Identity, readiness: Readiness, shutdown: Shutdown) -> Self {
        Self {
            identity,
            readiness,
            shutdown,
            routes: Router::new(),
            drain: std::time::Duration::from_secs(5),
        }
    }

    /// Mount a daemon's own routes.
    #[must_use]
    pub fn with(mut self, routes: Router) -> Self {
        self.routes = self.routes.merge(routes);
        self
    }

    /// How long to keep serving after the stop signal.
    ///
    /// The right window is a property of what the daemon does — a CSMS holding
    /// a two-hour session cannot drain it and a publisher can drain in a second
    /// — so it is stated rather than assumed.
    #[must_use]
    pub const fn draining_for(mut self, window: std::time::Duration) -> Self {
        self.drain = window;
        self
    }

    /// The finished router, for a test that wants to call it without a socket.
    pub fn router(&self) -> Router {
        let infra = Infra {
            identity: self.identity,
            readiness: self.readiness.clone(),
        };
        // The infrastructure routes carry their own state and are resolved to a
        // stateless router *before* the merge, so a daemon's routes keep whatever
        // state they were built with. A shell that imposed its state on the
        // daemon above it would be a shell nothing could mount.
        let shell: Router = Router::new()
            .route("/health/live", get(live))
            .route("/health/ready", get(ready))
            .route("/about", get(about))
            .with_state(infra);
        self.routes
            .clone()
            .merge(shell)
            .layer(axum::middleware::from_fn(traced))
    }

    /// Bind and serve until the shutdown signal, then drain.
    ///
    /// # Errors
    ///
    /// [`ServerError`] when the address cannot be bound or the server stops
    /// with one.
    pub async fn listen(self, addr: SocketAddr) -> Result<(), ServerError> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| ServerError::Bind { addr, source })?;
        self.serve_on(listener).await
    }

    /// Serve on a listener the caller bound — what a daemon started in-process
    /// on port `0` uses, so it can say which port it got.
    ///
    /// # Errors
    ///
    /// [`ServerError::Serve`] when the server stops with one.
    pub async fn serve_on(self, listener: tokio::net::TcpListener) -> Result<(), ServerError> {
        let router = self.router();
        let addr = listener
            .local_addr()
            .map_err(|source| ServerError::Serve { source })?;

        tracing::info!(
            daemon = self.identity.name,
            version = self.identity.version,
            %addr,
            "listening"
        );

        // The two-step stop. Readiness goes first so the orchestrator takes this
        // instance out of rotation, and only then does the drain window start —
        // so the window is time nothing new is being routed into.
        let readiness = self.readiness.clone();
        let shutdown = self.shutdown.clone();
        let drain = self.drain;
        let signal = async move {
            shutdown.wait().await;
            for (name, _) in readiness.report() {
                readiness.set(
                    name,
                    crate::health::Probe::down("this instance is stopping"),
                );
            }
            tracing::info!(?drain, "draining");
            tokio::time::sleep(drain).await;
        };

        axum::serve(listener, router)
            .with_graceful_shutdown(signal)
            .await
            .map_err(|source| ServerError::Serve { source })
    }
}

/// The header a request id travels in, both ways.
pub const REQUEST_ID: &str = "x-request-id";

/// An inbound request id, when it is one a log line can carry as it is: 1 to
/// 64 of `[A-Za-z0-9._-]`. Anything else — a newline, a quote, a kilobyte — is
/// replaced by a fresh id rather than written into somebody's incident
/// timeline (SR-102).
fn request_id(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|id| {
            (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        .map_or_else(
            || {
                let mut bytes = [0_u8; 16];
                aws_lc_rs::rand::fill(&mut bytes)
                    .unwrap_or_else(|_| unreachable!("the system RNG does not fail"));
                hex::encode(bytes)
            },
            str::to_owned,
        )
}

/// One span per request: its id, its method, the **route** it matched and —
/// once a gate admitted somebody — the actor. Never the raw path: for
/// `/r/{token}` that is a capability, and a query string can hold anything.
async fn traced(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tracing::Instrument as _;
    let id = request_id(request.headers());
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or("(unrouted)", axum::extract::MatchedPath::as_str)
        .to_owned();
    let probe = route.starts_with("/health/");
    let span = tracing::info_span!(
        "request",
        id = %id,
        method = %request.method(),
        route = %route,
        actor = tracing::field::Empty,
    );
    let mut response = next.run(request).instrument(span.clone()).await;
    let status = response.status().as_u16();
    span.in_scope(|| {
        if probe {
            tracing::debug!(status, "answered");
        } else {
            tracing::info!(status, "answered");
        }
    });
    if let Ok(value) = axum::http::HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    response
}

/// Say who a request is acting as: the [`crate::Actor`] a gate authenticated,
/// as a request extension a handler takes with `Extension<Actor>`, and on the
/// request's log span.
pub fn acting(request: &mut axum::extract::Request, actor: crate::Actor) {
    tracing::Span::current().record("actor", tracing::field::display(&actor));
    request.extensions_mut().insert(actor);
}

/// Why a daemon could not serve.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The address could not be bound.
    #[error("{addr} could not be bound: {source}")]
    Bind {
        /// Which address.
        addr: SocketAddr,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A configured bind address is not a socket address.
    #[error(
        "{from} is {text:?}, which is not a socket address — a bind is \
         `<host>:<port>`, and inside a container the host has to be `0.0.0.0` \
         because loopback there is reachable by nothing"
    )]
    BadAddress {
        /// Which variable it came from, or "the default".
        from: String,
        /// What it said.
        text: String,
    },
    /// The server stopped with an error.
    #[error("the server stopped: {source}")]
    Serve {
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// Liveness: the runtime is still scheduling.
///
/// Deliberately says nothing about dependencies. A liveness probe that failed
/// because a database was down would make a restart the cure for something that
/// was never in this process — and restarting a CSMS drops every station's
/// socket.
async fn live() -> StatusCode {
    StatusCode::OK
}

/// Readiness: every declared dependency is up.
///
/// The body lists what is not, because an operator reading a `503` needs the
/// reason in the same request rather than in a log they have to go and find.
async fn ready(State(infra): State<Infra>) -> (StatusCode, String) {
    if infra.readiness.is_ready() {
        return (StatusCode::OK, String::new());
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        infra.readiness.blockers().join("\n"),
    )
}

/// Who this is.
async fn about(State(infra): State<Infra>) -> String {
    format!("{} {}", infra.identity.name, infra.identity.version)
}

/// Read a request's headers and body, refusing one over `limit` bytes with `413`.
///
/// On its declared `content-length` before a byte of it is read, and otherwise
/// as soon as the bytes read pass the limit, so an endless body is never
/// buffered whole.
///
/// # Errors
///
/// [`StatusCode::PAYLOAD_TOO_LARGE`].
pub async fn read_bounded(
    request: axum::extract::Request,
    limit: usize,
) -> Result<(axum::http::HeaderMap, axum::body::Bytes), StatusCode> {
    let (parts, body) = request.into_parts();
    let declared = parts
        .headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > limit) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    Ok((parts.headers, bytes))
}

/// Put a daemon's read model behind the estate's operator tokens.
///
/// `401` without a body for anybody else — including everybody, when the estate
/// names no operator: a read model nobody configured access to is one nobody
/// may read, not one everybody may. An admitted request carries its operator
/// as an [`crate::Actor`] extension ([`acting`]).
pub fn operator_only(routes: Router, operators: crate::authority::Operators) -> Router {
    operator_gate(routes, operators, false)
}

/// Require a named operator whose estate grant allows writes. Unknown
/// credentials receive 401 and a valid read-only credential receives 403.
pub fn operator_write(routes: Router, operators: crate::authority::Operators) -> Router {
    operator_gate(routes, operators, true)
}

fn operator_gate(routes: Router, operators: crate::authority::Operators, write: bool) -> Router {
    routes.layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| {
            let operators = operators.clone();
            async move {
                use axum::response::IntoResponse as _;
                let authorization = request
                    .headers()
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                match operators.admit(authorization) {
                    None => (
                        StatusCode::UNAUTHORIZED,
                        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                    )
                        .into_response(),
                    Some((_, access)) if write && !access.can_write() => {
                        StatusCode::FORBIDDEN.into_response()
                    }
                    Some((actor, access)) => {
                        acting(&mut request, actor.clone());
                        request.extensions_mut().insert(access);
                        next.run(request).await
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower_service::Service as _;

    fn server(readiness: Readiness) -> Server {
        Server::new(Identity::new("testd", "0.1.0"), readiness, Shutdown::new())
    }

    async fn call(router: &mut Router, path: &str) -> (StatusCode, String) {
        let response = router
            .call(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .expect("infallible");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("a body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn liveness_is_up_while_readiness_waits() {
        // The distinction that stops a restart being the cure for a dependency
        // being down.
        let readiness = Readiness::new().expecting("registry");
        let mut router = server(readiness.clone()).router();

        assert_eq!(call(&mut router, "/health/live").await.0, StatusCode::OK);

        let (status, body) = call(&mut router, "/health/ready").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("registry"), "{body}");

        readiness.up("registry");
        assert_eq!(call(&mut router, "/health/ready").await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_daemons_own_routes_mount_beside_the_infrastructure_ones() {
        let readiness = Readiness::new().expecting("x");
        readiness.up("x");
        let mine = Router::new().route("/sessions", get(|| async { "none" }));
        let mut router = server(readiness).with(mine).router();

        assert_eq!(
            call(&mut router, "/sessions").await,
            (StatusCode::OK, "none".to_owned())
        );
        assert_eq!(call(&mut router, "/health/ready").await.0, StatusCode::OK);
        assert_eq!(
            call(&mut router, "/about").await.1,
            "testd 0.1.0",
            "and the daemon says who it is"
        );
    }

    #[tokio::test]
    async fn sr_034_a_read_model_answers_its_operators_and_nobody_else() {
        use crate::authority::{Operators, Token};
        let model = Router::new().route("/v1/testd", get(|| async { "{}" }));
        let mut guarded = operator_only(
            model.clone(),
            Operators::new(vec![(
                crate::Actor::operator("ops").unwrap(),
                Token::new("ops"),
            )]),
        );
        let request = |token: Option<&str>| {
            let mut builder = Request::builder().uri("/v1/testd");
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        let status = |response: axum::response::Response| response.status();
        assert_eq!(
            status(guarded.call(request(Some("ops"))).await.unwrap()),
            StatusCode::OK
        );
        assert_eq!(
            status(guarded.call(request(Some("guess"))).await.unwrap()),
            StatusCode::UNAUTHORIZED
        );
        let challenge = guarded.call(request(None)).await.unwrap();
        assert_eq!(challenge.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            challenge.headers()[axum::http::header::WWW_AUTHENTICATE],
            "Bearer"
        );
        let mut unconfigured = operator_only(model, Operators::default());
        assert_eq!(
            status(unconfigured.call(request(Some(""))).await.unwrap()),
            StatusCode::UNAUTHORIZED
        );
    }
}
