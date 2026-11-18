//! `billd`, started: the books folded, the receiver mounted, the calendar that
//! closes a month once it has ended.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use emob_billing::{Counterparty, PaymentDetails, Specification, TaxStatus};
use emob_service::estate::{
    Billing, BillingSection, Env, InvoiceSpecification, Operator, Parties, Partner, Services,
};
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::{File, Journal as _};
use emob_service::vault::{DataKey, Sealing};
use emob_service::{Actor, Identity, Readiness, Server, Shutdown};
use serde::Deserialize;

use crate::{Books, ChartOfAccounts, Closing, Numbering};

/// The probe `/health/ready` names while it waits.
pub const NUMBERING: &str = "numbering";

/// The variable holding the data key billd's journal is sealed under
/// (SR-087): 32 random bytes, base64.
pub const DATA_KEY: &str = "BILLD_DATA_KEY";

/// billd's journal: every record body sealed under [`DATA_KEY`], because the
/// CDRs it receives carry the card's UID and the invoices it issues the
/// debtor's name and IBAN (SR-087). A journal written under another key does
/// not fold, so billd refuses to start rather than open books it cannot read.
///
/// Signed over that: after every append the head is signed under
/// `BILLD_SIGNING_KEY` into `<journal>.head`, so a journal cut at its end —
/// the one edit the chain cannot see — is refused at start (SR-096).
pub type Journal = Signing<Sealing<File>>;

/// How often the calendar looks for a month that has ended.
const TICK: std::time::Duration = std::time::Duration::from_hours(1);

/// What `billd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// The operator.
    pub parties: Parties,
    /// Whom invoices go to.
    #[serde(default)]
    pub partners: Vec<Partner>,
    /// The operator's invoicing identity.
    pub billing: BillingSection,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The webhook secret.
    pub services: Services,
}

/// The party on an invoice.
#[must_use]
pub fn counterparty(billing: &Billing) -> Counterparty {
    let party = Counterparty::new(
        &billing.name,
        &billing.city,
        TaxStatus::business(&billing.country, &billing.vat_id),
    );
    let party = match (&billing.street, &billing.post_code) {
        (Some(street), Some(post_code)) => party.at(street, post_code),
        _ => party,
    };
    match &billing.electronic_address {
        // UNTDID 3155 / EAS `EM`: an e-mail address.
        Some(address) => party.reachable_at(address, "EM"),
        None => party,
    }
}

/// How a partner pays, from the estate: its mandate under the operator's
/// creditor identifier (SEPA direct debit, BG-19), else a transfer to the
/// operator's account (BG-17), else nothing stated.
///
/// A mandate without a creditor identifier states no collection rather than
/// half of one: `BR-DE-30` and `BR-DE-31` need both.
#[must_use]
pub fn payment(estate: &Estate, partner: &Partner) -> Option<PaymentDetails> {
    let billing = &estate.billing;
    if let (Some(mandate), Some(creditor)) = (&partner.mandate, &billing.creditor_id) {
        return Some(PaymentDetails::DirectDebit {
            mandate_reference: mandate.reference.clone(),
            creditor_identifier: creditor.clone(),
            debited_iban: mandate.iban.clone(),
        });
    }
    billing
        .iban
        .as_ref()
        .map(|iban| PaymentDetails::CreditTransfer {
            iban: iban.clone(),
            holder: Some(billing.seller.name.clone()),
        })
}

/// The EN 16931 specification a partner's documents are written in.
#[must_use]
pub const fn specification(partner: &Partner) -> Specification {
    match partner.specification {
        InvoiceSpecification::En16931 => Specification::Core,
        InvoiceSpecification::Xrechnung => Specification::XRechnung,
    }
}

/// The closing of one partner's month.
#[must_use]
pub fn closing(
    estate: &Estate,
    partner: &Partner,
    year: i32,
    month: time::Month,
    issued_on: time::Date,
) -> Closing {
    Closing {
        partner: partner.party.clone(),
        year,
        month,
        issued_on,
        seller: match &estate.billing.contact {
            Some(contact) => counterparty(&estate.billing.seller).contactable(
                &contact.name,
                &contact.phone,
                &contact.email,
            ),
            None => counterparty(&estate.billing.seller),
        },
        buyer: counterparty(&partner.billing),
        vat_rate: estate.billing.vat_rate,
        specification: specification(partner),
        payment: payment(estate, partner),
        buyer_reference: partner.buyer_reference.clone(),
    }
}

/// What a `billd` is started with.
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

/// A `billd` that is running.
pub struct Running {
    /// Where it answers.
    pub http: SocketAddr,
    /// The books.
    pub books: Arc<Mutex<Books<Journal>>>,
    /// The key the invoice pages are linked under (spec 010).
    pub invoices: Arc<crate::invoice_page::Invoices>,
    /// The estate it runs on.
    pub estate: Estate,
    shutdown: Shutdown,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Stop as a crash would.
    pub fn kill(self) {
        for task in &self.tasks {
            task.abort();
        }
    }

    /// Wait for the shutdown signal and the drain.
    pub async fn wait(self) {
        self.shutdown.wait().await;
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Close every partner's previous month that is not closed yet.
///
/// The closings are the calendar's: nobody asked for them, so the record
/// names [`Actor::Timer`].
pub fn close_what_is_due<J: emob_service::journal::Journal>(
    books: &Mutex<Books<J>>,
    estate: &Estate,
    today: time::Date,
) {
    let previous = today
        .replace_day(1)
        .map_or(today, |first| first - time::Duration::days(1));
    for partner in &estate.partners {
        let period = crate::period_key(&partner.party, previous.year(), previous.month());
        if lock(books).state().closes(&period).is_some() {
            continue;
        }
        let closing = closing(estate, partner, previous.year(), previous.month(), today);
        match lock(books).close(&Actor::Timer, &closing) {
            Ok(Some(number)) => tracing::info!(%number, %period, "a month closed"),
            Ok(None) => {}
            Err(error) => tracing::error!(%error, %period, "a month could not be closed"),
        }
    }
}

/// The operator system's dispatcher, where the estate configures one
/// (spec 010): the secret resolved, the target refused unless https or
/// loopback (SR-121).
fn dispatcher(estate: &Estate, env: &Env) -> Result<Option<crate::deliver::Dispatcher>> {
    let Some(delivery) = &estate.billing.delivery else {
        return Ok(None);
    };
    let key = "billing.delivery.secret_env";
    let secret = delivery.secret_env.resolve_in(key, env)?;
    let secret = emob_service::webhook::Secret::standard(secret.expose())
        .map_err(|error| anyhow::anyhow!("{key}: {error}"))?;
    Ok(Some(crate::deliver::Dispatcher::new(
        &delivery.url,
        secret,
        delivery.syntax,
        std::time::Duration::from_secs(10),
    )?))
}

/// Drain the dispatcher every `every` until shutdown.
fn dispatching(
    dispatcher: crate::deliver::Dispatcher,
    books: Arc<Mutex<Books<Journal>>>,
    invoices: Arc<crate::invoice_page::Invoices>,
    stopping: Shutdown,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        loop {
            tokio::select! {
                _ = ticker.tick() => { dispatcher.drain(&books, &invoices).await; }
                () = stopping.wait() => return,
            }
        }
    })
}

/// billd's journal, sealed and signed, folded into its books.
fn open_books(journal: &std::path::Path, estate: &Estate, env: &Env) -> Result<Books<Journal>> {
    let year = time::OffsetDateTime::now_utc().year();
    let key = Arc::new(DataKey::from_env(DATA_KEY, env)?);
    let signing = SigningKey::for_daemon("billd", env)?;
    let file = Signing::open(
        Sealing::new(
            File::open(journal).context("opening billd's journal")?,
            key,
            "billd/journal",
        ),
        signing,
        head::beside(journal),
    )
    .context("opening billd's journal against its signed head")?;
    Books::open(
        "emob",
        Numbering::series(estate.billing.series.clone(), year),
        ChartOfAccounts::new(),
        file,
    )
    .context("folding billd's journal")
}

/// Start `billd`.
///
/// # Errors
///
/// An unset secret, data key or signing key, a journal that does not fold
/// (one sealed under another `BILLD_DATA_KEY` among them) or is shorter than
/// the head billd last signed, an address that cannot be bound.
pub async fn run(options: Options) -> Result<Running> {
    let Options {
        http,
        journal,
        estate,
        env,
        shutdown,
        identity,
    } = options;
    let secret = emob_service::estate::webhook_secret(&estate.services, &env)?;
    let operators = emob_service::estate::operator_tokens(&estate.operators, &env)?;

    let dispatcher = dispatcher(&estate, &env)?;
    let readiness = Readiness::new().expecting(NUMBERING);
    let books = open_books(&journal, &estate, &env)?;
    tracing::info!(
        documents = books.state().documents().count(),
        received = books.state().received().count(),
        head = books
            .journal()
            .head()
            .map(|h| h.position)
            .unwrap_or_default(),
        "the books are what the journal says they are"
    );
    let invoices = Arc::new(
        crate::invoice_page::Invoices::open(
            journal
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
            estate.billing.public_url.as_deref(),
        )
        .context("opening billd's invoice.key")?,
    );

    readiness.up(NUMBERING);
    let latest = books.journal().latest();
    let books = Arc::new(Mutex::new(books));

    let routes = crate::receive::router(
        crate::receive::Shared {
            books: Arc::clone(&books),
            secret,
            partners: estate.partners.iter().map(|p| p.party.clone()).collect(),
            refused: Arc::new(AtomicU64::new(0)),
        },
        latest,
        operators,
    )
    .merge(crate::invoice_page::router(
        Arc::clone(&books),
        Arc::clone(&invoices),
    ));
    let listener = tokio::net::TcpListener::bind(http)
        .await
        .with_context(|| format!("binding {http}"))?;
    let addr = listener.local_addr()?;

    let mut tasks = Vec::new();
    let calendar = Arc::clone(&books);
    let due = estate.clone();
    let stopping = shutdown.clone();
    tasks.push(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                _ = ticker.tick() => close_what_is_due(&calendar, &due, time::OffsetDateTime::now_utc().date()),
                () = stopping.wait() => return,
            }
        }
    }));
    if let Some(dispatcher) = dispatcher {
        let every = std::time::Duration::from_secs(estate.services.retry_secs.max(1));
        tasks.push(dispatching(
            dispatcher,
            Arc::clone(&books),
            Arc::clone(&invoices),
            shutdown.clone(),
            every,
        ));
    }
    let server = Server::new(identity, readiness, shutdown.clone())
        .with(routes)
        .draining_for(std::time::Duration::from_secs(3));
    tasks.push(tokio::spawn(async move {
        if let Err(error) = server.serve_on(listener).await {
            tracing::error!(%error, "billd stopped");
        }
    }));
    Ok(Running {
        http: addr,
        books,
        invoices,
        estate,
        shutdown,
        tasks,
    })
}
