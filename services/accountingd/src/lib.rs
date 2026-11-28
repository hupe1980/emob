//! Retail open items, booked payments and named allocations for the local eMSP.
#![warn(missing_docs, clippy::pedantic)]
mod api;
mod customer;
mod feed;
mod import;
mod ledger;
mod model;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use emob_core::PartyId;
use emob_service::estate::{Env, Operator, Parties, RetailAccount, Services};
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::{File, Journal};
use emob_service::vault::{DataKey, Sealing};
use emob_service::{Identity, Readiness, Server, Shutdown, serve};
use serde::Deserialize;

const PURPOSE: &str = "accountingd/receivables/v1";
type Disk = Sealing<Signing<File>>;

/// Provider, invoice owner and named finance operators for this estate.
#[derive(Clone, Debug, Deserialize)]
pub struct Estate {
    /// The local eMSP issues the retail documents in this ledger.
    pub parties: Parties,
    /// The private billing feed is served by billd.
    pub services: Services,
    /// Named staff credentials for finance operations.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The configured payer identities accepted in a local statement.
    #[serde(default)]
    pub retail_accounts: Vec<RetailAccount>,
}

impl Estate {
    /// Validate the billing feed destination and configured payer IDs.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid origin or payer.
    pub fn validate(&self) -> Result<()> {
        validate_origin("services.billd", &self.services.billd)?;
        if let Some(origin) = &self.services.accountingd {
            validate_origin("services.accountingd", origin)?;
        }
        emob_service::estate::validate_retail_accounts(&self.retail_accounts)?;
        Ok(())
    }
}

fn validate_origin(field: &'static str, raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw).context(field)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "{field} must be an HTTP(S) origin"
    );
    Ok(())
}

/// Dependencies of the binary and embedded estate runner.
pub struct Options {
    /// Address on which the owner listens.
    pub http: SocketAddr,
    /// Accounting's own sealed journal path.
    pub journal: PathBuf,
    /// Sections read by accounting.
    pub estate: Estate,
    /// Resolver for local secrets.
    pub env: Env,
    /// Shutdown signal.
    pub shutdown: Shutdown,
    /// Service name and version.
    pub identity: Identity,
}

/// Running finance service.
pub struct Running {
    /// Address assigned by the OS.
    pub http: SocketAddr,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Running {
    /// Wait for graceful shutdown.
    ///
    /// # Errors
    ///
    /// Returns a listener or task failure.
    pub async fn wait(self) -> Result<()> {
        self.task.await?
    }

    /// Stop the owner immediately for an estate restart.
    pub async fn kill(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Rebuild the finance state from an independently verified cut of the sealed journal.
///
/// # Errors
///
/// Returns an error for invalid, unreadable or inconsistent accounting records.
pub fn recovery_proof<J: Journal>(
    journal: J,
    key: Arc<DataKey>,
    issuer: &PartyId,
) -> Result<serde_json::Value> {
    let sealed = Sealing::new(journal, Arc::clone(&key), PURPOSE);
    let book = ledger::Book::open(
        issuer.clone(),
        sealed,
        key,
        Readiness::new().expecting("ledger"),
    )?;
    Ok(book.state.proof())
}

/// Start the finance owner after verifying and replaying its journal.
///
/// # Errors
///
/// Reports invalid configuration, key, journal and listener failures.
pub async fn run(options: Options) -> Result<Running> {
    let Options {
        http,
        journal,
        estate,
        env,
        shutdown,
        identity,
    } = options;
    estate.validate()?;
    let operators = emob_service::estate::operator_tokens(&estate.operators, &env)?;
    let key = Arc::new(DataKey::from_env("ACCOUNTINGD_DATA_KEY", &env)?);
    let signed = Signing::open(
        File::open(&journal)?,
        SigningKey::for_daemon("accountingd", &env)?,
        head::beside(&journal),
    )?;
    let latest = signed.latest();
    let ready = Readiness::new().expecting("ledger");
    let book = ledger::Book::open(
        estate.parties.provider,
        Sealing::new(signed, Arc::clone(&key), PURPOSE),
        key,
        ready.clone(),
    )?;
    let owner = api::Owner {
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .build()?,
        feed: reqwest::Url::parse(&estate.services.billd)?.join("/v1/billd/receivables")?,
        accounts: estate.retail_accounts.into_iter().map(|p| p.id).collect(),
        ledger: std::sync::Mutex::new(book),
        syncing: tokio::sync::Mutex::new(()),
    };
    let routes = api::routes(owner, operators.clone()).merge(serve::operator_only(
        head::router("accountingd", latest),
        operators,
    ));
    let service = Server::new(identity, ready, shutdown).with(routes);
    let listener = tokio::net::TcpListener::bind(http).await?;
    let http = listener.local_addr()?;
    let task = tokio::spawn(async move { Ok(service.serve_on(listener).await?) });
    Ok(Running { http, task })
}

#[cfg(test)]
mod tests;
