//! `billd` — the daemon that closes a month.
//!
//! A socket, a clock and a calendar. What a document *says* is decided by
//! [`emob_billing`] and tested there; what this process adds is **when** the
//! closing happens — see [`billd`] for why that split is the whole design.
//!
//! # The loop is a calendar, not a queue
//!
//! Every other daemon here drains work. This one waits for a date, and the tick
//! asks three things in order, stopping at the first `no`:
//!
//! 1. is the period over,
//! 2. are the records in — `roamd::Roamd::unsettled` is the same question from
//!    the other side, because a partner's CDR may turn up days after the session
//!    and closing before it lands bills the driver for less than they used, and
//! 3. has this period already been closed.
//!
//! Only then is a number spent. What follows is a submission and an answer:
//! [`billd::Billd::accepted`], [`billd::Billd::rejected`], and
//! [`billd::Billd::book`] only once the recipient has it.
//!
//! # What is not here
//!
//! The submission. A German public buyer's *Rechnungseingangsplattform* is
//! somebody else's endpoint behind a registration, exactly as `csmsd`'s WebSocket
//! and the Mobilithek subscription are, and CI cannot run it. The bytes it would
//! carry are `emob_billing::en16931::to_en16931`'s, validated in a test.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use billd::{Books, ChartOfAccounts, Numbering};
use emob_service::journal::Journal as _;
use emob_service::{Identity, Readiness, Server, Shutdown, identity};

/// What the daemon waits for before it takes traffic.
///
/// The number series. A closing run against a counter that has not been read
/// back out of the store starts at one and issues a number an earlier document
/// already carries, which `[UStG §14(4) Nr. 4]` forbids by name — so answering a
/// readiness probe before it is loaded is worse than being slow to start.
const NUMBERING: &str = "numbering";

/// How often the calendar is asked.
///
/// Hourly, because the answer changes at most once a month and a closing that
/// happens an hour late is a closing; one that happens twice is a second
/// invoice.
const TICK: std::time::Duration = std::time::Duration::from_secs(3600);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,billd=debug", false);

    let http: SocketAddr = emob_service::serve::bind_address("billd", "127.0.0.1:9584")?;

    let readiness = Readiness::new().expecting(NUMBERING);
    let shutdown = Shutdown::new();

    // The counter resumes from the journal, not from one. `Numbering::series`
    // alone is a fresh counter, and a fresh counter against a journal that
    // already holds `R-2026-0007` is the one failure `[UStG §14(4) Nr. 4]`
    // names — so the fold runs **before** the readiness probe goes up, and a
    // journal that does not chain stops the process here rather than letting it
    // serve from a past it cannot account for.
    let series = std::env::var("BILLD_SERIES").unwrap_or_else(|_| "R".to_owned());
    let year = time::OffsetDateTime::now_utc().year();
    let journal = emob_service::journal::for_daemon("billd")
        .context("opening billd's journal — see EMOB_JOURNAL_DIR")?;
    tracing::info!(path = %journal.path().display(), "folding the journal");
    let books = Books::open(
        "emob",
        Numbering::series(series, year),
        ChartOfAccounts::new(),
        journal,
    )
    .context("folding billd's journal")?;
    tracing::info!(
        documents = books.state().documents().count(),
        head = %books.journal().head().unwrap_or_default(),
        "the books are what the journal says they are"
    );
    let service = Arc::new(Mutex::new(books));
    readiness.up(NUMBERING);

    let calendar = Arc::clone(&service);
    let stopping = shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            if stopping.is_stopping() {
                return;
            }
            close_what_is_due(&calendar);
        }
    });

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}

/// One turn of the calendar.
///
/// Split out because it is the whole of what the daemon does, and because the
/// only thing that must never happen here is a second number for one month.
fn close_what_is_due(service: &Mutex<Books<emob_service::journal::File>>) {
    let books = lock(service);

    // Which periods are over and unclosed is an operator's question and the
    // assembly is `emob_billing::InvoiceBuilder`'s; what is here is the tick
    // that notices. `Books::issue` refuses a period it has already closed —
    // across a restart, because the closing is a record rather than a field.
    for document in books.state().documents() {
        tracing::debug!(
            number = %document.invoice.number,
            booked = document.booked,
            "held"
        );
    }

    // The list an operator is paged for: accepted, and still not in the books.
    // Not a backlog item — a document the recipient holds and the ledger does
    // not, which nothing else in the system will notice.
    for document in books.state().documents() {
        if document.submission.is_accepted() && !document.booked {
            tracing::error!(
                number = %document.invoice.number,
                "the recipient has this document and the books do not"
            );
        }
    }
}

/// A lock that survives a panic elsewhere.
///
/// The guarded value is a map and a journal, and a service that answered a
/// poisoned lock by refusing to close a month is a worse failure than the one it
/// is avoiding — the same reasoning `csmsd` states for its ledgers.
fn lock(
    service: &Mutex<Books<emob_service::journal::File>>,
) -> std::sync::MutexGuard<'_, Books<emob_service::journal::File>> {
    service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
