//! The standalone customer portal. The provider owns sign-in and grants, and billing owns each issued document.
#![warn(missing_docs, clippy::pedantic)]
mod browser;
mod client;
mod views;

use std::net::{IpAddr, SocketAddr};

use anyhow::{Context as _, Result, ensure};
use emob_service::estate::{Portal, ReceiptSection, Services};
use emob_service::{Identity, Readiness, Server, Shutdown};
use reqwest::Url;
use serde::Deserialize;

/// The portal's three estate sections.
#[derive(Clone, Deserialize)]
pub struct Estate {
    /// Browser-facing origin used for form and cookie security.
    pub portal: Portal,
    /// Owner addresses on the private service network.
    pub services: Services,
    /// Browser-facing origin of permitted charging receipts.
    pub receipts: Option<ReceiptSection>,
}

fn origin(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("expected an absolute HTTP(S) origin")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "expected an HTTP(S) origin without credentials, path, query or fragment"
    );
    Ok(url)
}

impl Estate {
    /// Validate the browser and owner origins before serving a sign-in form.
    ///
    /// # Errors
    ///
    /// Returns the affected configuration field when a URL is invalid, or when public HTTP leaves loopback.
    pub fn validate(&self) -> Result<()> {
        let portal = origin(&self.portal.public_url).context("portal.public_url")?;
        let loopback = portal.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        ensure!(
            portal.scheme() == "https" || loopback,
            "portal.public_url requires HTTPS outside loopback"
        );
        origin(&self.services.empd).context("services.empd")?;
        origin(&self.services.billd).context("services.billd")?;
        if let Some(accounting) = &self.services.accountingd {
            origin(accounting).context("services.accountingd")?;
        }
        if let Some(receipts) = &self.receipts {
            origin(&receipts.public_url).context("receipts.public_url")?;
        }
        Ok(())
    }
}

/// Dependencies for starting the portal as a process or in an embedded estate.
pub struct Options {
    /// HTTP listener address, including port zero for an embedded instance.
    pub http: SocketAddr,
    /// Customer, receipt and owner origins.
    pub estate: Estate,
    /// Graceful shutdown signal.
    pub shutdown: Shutdown,
    /// Service name and version.
    pub identity: Identity,
}

/// Running customer listener.
pub struct Running {
    /// Bound HTTP address.
    pub http: SocketAddr,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Running {
    /// Wait until the process receives its shutdown signal.
    ///
    /// # Errors
    ///
    /// Returns an error if the server task or listener failed.
    pub async fn wait(self) -> Result<()> {
        self.task.await?
    }

    /// Release the listener immediately for an embedded estate restart.
    pub async fn kill(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Start the customer listener and attach the estate health endpoints.
///
/// # Errors
///
/// Returns an error for invalid configuration, HTTP client initialization or a bind failure.
pub async fn run(opts: Options) -> Result<Running> {
    opts.estate.validate()?;
    let owners = client::Owners::new(&opts.estate)?;
    let ready = Readiness::new().expecting("portal");
    ready.up("portal");
    let service = Server::new(opts.identity, ready, opts.shutdown).with(browser::routes(owners));
    let listener = tokio::net::TcpListener::bind(opts.http).await?;
    let http = listener.local_addr()?;
    let task = tokio::spawn(async move { Ok(service.serve_on(listener).await?) });
    Ok(Running { http, task })
}
