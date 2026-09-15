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
        self.routes.clone().merge(shell)
    }

    /// Bind and serve until the shutdown signal, then drain.
    ///
    /// # Errors
    ///
    /// [`ServerError`] when the address cannot be bound or the server stops
    /// with one.
    pub async fn listen(self, addr: SocketAddr) -> Result<(), ServerError> {
        let router = self.router();
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| ServerError::Bind { addr, source })?;

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
}
