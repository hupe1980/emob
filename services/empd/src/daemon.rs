//! `empd`, started: the signed journal opened, the book folded, the estate's
//! contracts seeded, the routes mounted.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use emob_service::estate::{ContractSeed, Env, Operator, Parties, Services};
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::{File, Journal as _};
use emob_service::{Identity, Readiness, Server, Shutdown, Token};
use serde::Deserialize;

use crate::{Book, Empd};

/// The probe `/health/ready` names while it waits.
pub const CONTRACTS: &str = "contracts";

/// The variable holding the data key token references are derived and UIDs
/// sealed under (SR-046, SR-047): 32 random bytes, base64. Destroying it erases
/// every UID the journal holds without breaking the chain.
pub const DATA_KEY: &str = "EMPD_DATA_KEY";

/// Directory holding the current per-customer keys and erasure receipts.
pub fn custody_directory() -> PathBuf {
    std::env::var_os("EMPD_CUSTODY_DIR")
        .map_or_else(|| PathBuf::from("custody/empd"), PathBuf::from)
}

/// What `empd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// Whom tokens are issued by.
    pub parties: Parties,
    /// The contracts to seed.
    #[serde(default)]
    pub contracts: Vec<ContractSeed>,
    /// Principals authenticated by the local customer identity service.
    #[serde(default)]
    pub portal_users: Vec<emob_service::estate::PortalUser>,
    /// Private payer profiles used to assign native agreements.
    #[serde(default)]
    pub retail_accounts: Vec<emob_service::estate::RetailAccount>,
    /// Unissued RFID inventory.
    #[serde(default)]
    pub card_stock: Vec<emob_service::estate::CardStock>,
    /// Public retail selections for signed agreements.
    #[serde(default)]
    pub public_plans: Vec<emob_service::estate::PublicPlan>,
    /// Address of the local CSMS that owns price publication.
    pub desk: Option<emob_service::estate::Desk>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The service token `csmsd` presents.
    pub services: Services,
}

/// Validate native stock and its configured point selections before starting the owner.
///
/// # Errors
///
/// Rejects duplicate or invalid stock, too many selections or missing price source.
pub fn validate_enrolment(estate: &Estate) -> Result<()> {
    crate::enrol::Catalog::configuration(estate).map(drop)
}

/// What an `empd` is started with.
pub struct Options {
    /// The HTTP address.
    pub http: SocketAddr,
    /// The journal file.
    pub journal: PathBuf,
    /// Current key store, managed independently of the journal backup.
    pub custody: PathBuf,
    /// The estate.
    pub estate: Estate,
    /// Where secrets are looked up.
    pub env: Env,
    /// The stop signal.
    pub shutdown: Shutdown,
    /// Who this is.
    pub identity: Identity,
}

/// The journal `empd` keeps: a file whose head is signed after every append, so
/// a copy cut short by a restored backup or an edit refuses to start (SR-096).
pub type Journal = Signing<File>;

/// An `empd` that is running.
pub struct Running {
    /// Where it answers.
    pub http: SocketAddr,
    /// The book.
    pub book: Arc<Mutex<Book<Journal>>>,
    shutdown: Shutdown,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    /// Stop as a crash would.
    pub async fn kill(self) {
        self.task.abort();
        let _ = self.task.await;
    }

    /// Wait for the shutdown signal and the drain.
    pub async fn wait(self) {
        self.shutdown.wait().await;
        let _ = self.task.await;
    }
}

/// Start `empd`.
///
/// # Errors
///
/// Startup checks secrets, the signed journal and its independent custody binding
/// before replaying grants, applying estate seeds and binding the listener.
pub async fn run(options: Options) -> Result<Running> {
    run_with_clock(options, emob_service::system_clock()).await
}

/// Start this provider on the same decision clock as its CSMS.
///
/// # Errors
///
/// The same configuration, journal and listener errors as [`run`].
pub async fn run_with_clock(options: Options, clock: emob_service::Clock) -> Result<Running> {
    let Options {
        http,
        journal,
        custody,
        estate,
        env,
        shutdown,
        identity,
    } = options;
    let key = emob_service::vault::DataKey::from_env(DATA_KEY, &env)?;
    let operators = emob_service::estate::operator_tokens(&estate.operators, &env)?;
    let service = emob_service::estate::service_credential(&estate.services, &env, &operators)?;

    let catalog = Arc::new(crate::enrol::Catalog::from_estate(&estate)?);
    let readiness = Readiness::new().expecting(CONTRACTS);
    let signing = SigningKey::for_daemon("empd", &env)?;
    let file = Signing::open(
        File::open(&journal).context("opening empd's journal")?,
        signing,
        head::beside(&journal),
    )
    .context("opening empd's journal against its signed head")?;
    let latest = file.latest();
    let custody_dir = independent_directory(&journal, &custody)?;
    let provider = Empd::new(estate.parties.provider.clone(), key);
    let provider = attach_custody(provider, &file, &custody_dir)?;
    let mut users = Vec::new();
    for user in &estate.portal_users {
        if !provider.is_customer_erased(&user.subject)? {
            users.push(user.clone());
        }
    }
    let credentials =
        emob_service::estate::portal_credentials(&users, &env, &operators, Some(&service))?;
    let mut book = Book::open(provider, file).context("folding empd's journal")?;
    anyhow::ensure!(
        book.state().grants.readable(),
        "customer grants could not be replayed with the current custody"
    );
    let seeded = crate::seed::apply(&mut book, &estate.contracts)?;
    tracing::info!(
        seeded,
        contracts = book.state().contracts().count(),
        head = book
            .journal()
            .head()
            .map(|head| head.position)
            .unwrap_or_default(),
        "the book is what the journal says it is"
    );
    readiness.up(CONTRACTS);
    let book = Arc::new(Mutex::new(book));

    let routes = crate::http::router(
        crate::http::Shared {
            book: Arc::clone(&book),
            service: Token::new(service.expose()),
            customers: Arc::new(Mutex::new(crate::customer::Identity::new(credentials))),
            catalog,
            clock,
        },
        operators.clone(),
    )
    .merge(emob_service::serve::operator_only(
        head::router("empd", latest),
        operators,
    ));
    let listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("binding {http}"))?;
    let addr = listener.local_addr()?;
    let server = Server::new(identity, readiness, shutdown.clone())
        .with(routes)
        .draining_for(std::time::Duration::from_secs(3));
    let task = tokio::spawn(async move {
        if let Err(error) = server.serve_on(listener).await {
            tracing::error!(%error, "empd stopped");
        }
    });
    Ok(Running {
        http: addr,
        book,
        shutdown,
        task,
    })
}

fn attach_custody(provider: Empd, file: &Journal, path: &Path) -> Result<Empd> {
    use emob_service::journal::Kinded as _;
    let first = file.records()?.into_iter().next();
    let bound = match first {
        None => None,
        Some(r) if r.kind == "custody_bound" => {
            let crate::Event::CustodyBound { id } = serde_json::from_str(&r.body)? else {
                anyhow::bail!("invalid provider custody binding");
            };
            Some(id)
        }
        Some(_) => anyhow::bail!("existing provider journal has no custody binding"),
    };
    let (custody, id) =
        crate::custody::Custody::disk(path, Arc::clone(&provider.key), bound.as_deref())?;
    if bound.is_none() {
        let event = crate::Event::CustodyBound { id };
        file.append(
            event.kind(),
            &emob_service::Actor::Estate,
            &serde_json::to_string(&event)?,
        )?;
    }
    Ok(provider.with_custody(custody))
}

fn independent_directory(journal: &Path, dir: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        !dir.as_os_str().is_empty(),
        "the empd custody directory is empty"
    );
    // The journal has already opened. Resolve the existing ancestry of the custody
    // path too, including symlinks, before a fresh store creates missing directories.
    let journal_dir = std::fs::canonicalize(journal)?
        .parent()
        .context("empd journal has no parent")?
        .to_owned();
    let absolute = std::path::absolute(dir)?;
    let mut ancestor = absolute.as_path();
    while !ancestor.try_exists()? {
        ancestor = ancestor
            .parent()
            .context("empd custody path has no existing parent")?;
    }
    let path = std::fs::canonicalize(ancestor)?.join(absolute.strip_prefix(ancestor)?);
    anyhow::ensure!(
        !path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
            && !path.starts_with(&journal_dir)
            && !journal_dir.starts_with(&path),
        "EMPD_CUSTODY_DIR must be independent of the provider journal directory"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_service::journal::Memory;
    use emob_service::vault::DataKey;

    #[test]
    fn a_signed_journal_binds_current_custody_before_any_customer_grant() {
        let root = std::env::temp_dir().join(format!(
            "empd-pin-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let journal = root.join("journals/empd.ndjson");
        let dir = root.join("custody");
        let journal_file = || {
            Signing::open(
                File::open(&journal).unwrap(),
                SigningKey::from_seed(&[2; 32], "empd"),
                head::beside(&journal),
            )
            .unwrap()
        };
        let provider = || Empd::new("DE*MSP".parse().unwrap(), DataKey::from_bytes(&[1; 32]));
        let file = journal_file();
        assert!(independent_directory(&journal, &journal.with_extension("custody")).is_err());
        assert!(independent_directory(&journal, &root).is_err());
        assert!(independent_directory(&journal, &dir).is_ok());
        let bound = attach_custody(provider(), &file, &dir).unwrap();
        let records = file.records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "custody_bound");
        let bytes = std::fs::read(&journal).unwrap();
        let copy = Memory::from_cold_cut(&bytes).unwrap();
        assert!(Book::open(provider(), copy).is_err());
        assert!(Book::open(bound, Memory::from_cold_cut(&bytes).unwrap()).is_ok());
        drop(file);
        std::fs::rename(&dir, root.join("offline")).unwrap();
        assert!(attach_custody(provider(), &journal_file(), &dir).is_err());
        assert!(!dir.exists());
        std::fs::rename(root.join("offline"), &dir).unwrap();
        assert!(attach_custody(provider(), &journal_file(), &dir).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
