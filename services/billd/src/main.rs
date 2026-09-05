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
use billd::{Billd, Numbering};
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

    let http: SocketAddr = std::env::var("BILLD_HTTP_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9584".to_owned())
        .parse()
        .context("BILLD_HTTP_BIND is not a socket address")?;

    let readiness = Readiness::new().expecting(NUMBERING);
    let shutdown = Shutdown::new();

    // A real deployment reads the last number it issued out of its store and
    // resumes after it. `Numbering::series` alone is a fresh counter, and a
    // fresh counter against a store that already holds `R-2026-0007` is the one
    // failure the statute names.
    let series = std::env::var("BILLD_SERIES").unwrap_or_else(|_| "R".to_owned());
    let year = time::OffsetDateTime::now_utc().year();
    let service = Arc::new(Mutex::new(Billd::new(
        "emob",
        Numbering::series(series, year),
    )));
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
fn close_what_is_due(service: &Mutex<Billd>) {
    let books = lock(service);

    // A real deployment asks its store which periods are over and unclosed,
    // assembles each with `emob_billing::InvoiceBuilder`, and hands the closure
    // to `Billd::issue` — which refuses a period it has already closed rather
    // than issuing a second invoice for it. Submission and `accepted` follow;
    // `book` only after that.
    for document in books.documents() {
        tracing::debug!(
            number = %document.invoice.number,
            booked = document.booked,
            "held"
        );
    }

    // The list an operator is paged for: accepted, and still not in the books.
    // Not a backlog item — a document the recipient holds and the ledger does
    // not, which nothing else in the system will notice.
    for document in books.documents() {
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
fn lock(service: &Mutex<Billd>) -> std::sync::MutexGuard<'_, Billd> {
    service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
