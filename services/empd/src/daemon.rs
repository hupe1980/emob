//! `empd`, started: the signed journal opened, the book folded, the estate's
//! contracts seeded, the routes mounted.

use std::net::SocketAddr;
use std::path::PathBuf;
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

/// What `empd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// Whom tokens are issued by.
    pub parties: Parties,
    /// The contracts to seed.
    #[serde(default)]
    pub contracts: Vec<ContractSeed>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The service token `csmsd` presents.
    pub services: Services,
}

/// What an `empd` is started with.
pub struct Options {
    /// The HTTP address.
    pub http: SocketAddr,
    /// The journal file.
    pub journal: PathBuf,
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
    pub fn kill(self) {
        self.task.abort();
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
/// An unset secret (`EMPD_DATA_KEY`, `EMPD_SIGNING_KEY`), a journal shorter
/// than or diverged from its signed head, a seed the book refuses, a journal that does not fold, or
/// an address that cannot be bound.
pub async fn run(options: Options) -> Result<Running> {
    let Options {
        http,
        journal,
        estate,
        env,
        shutdown,
        identity,
    } = options;
    let key = emob_service::vault::DataKey::from_env(DATA_KEY, &env)?;
    let service = estate
        .services
        .service_token_env
        .resolve_in("services.service_token_env", &env)?;
    let operators = emob_service::estate::operator_tokens(&estate.operators, &env)?;

    let readiness = Readiness::new().expecting(CONTRACTS);
    let signing = SigningKey::for_daemon("empd", &env)?;
    let file = Signing::open(
        File::open(&journal).context("opening empd's journal")?,
        signing,
        head::beside(&journal),
    )
    .context("opening empd's journal against its signed head")?;
    let latest = file.latest();
    let mut book = Book::open(Empd::new(estate.parties.provider.clone(), key), file)
        .context("folding empd's journal")?;
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
