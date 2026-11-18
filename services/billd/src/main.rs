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
//! Configuration only: the receiver of `csmsd`'s deliveries and the calendar
//! are [`billd::daemon::run`].
//!
//! # What is not here
//!
//! The submission. A German public buyer's *Rechnungseingangsplattform* is
//! somebody else's endpoint behind a registration, exactly as `csmsd`'s WebSocket
//! and the Mobilithek subscription are, and CI cannot run it. The bytes it would
//! carry are `emob_billing::en16931::to_en16931`'s, validated in a test.

use anyhow::{Context as _, Result};
use billd::daemon::{Options, run};
use emob_service::{Identity, Shutdown, estate, identity, journal};

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,billd=debug");

    let path = estate::path();
    let estate: billd::daemon::Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;

    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    let running = run(Options {
        http: emob_service::serve::bind_address("billd", "127.0.0.1:9584")?,
        journal: journal::dir().join("billd.ndjson"),
        estate,
        env: estate::process_env(),
        shutdown,
        identity: me,
    })
    .await?;
    running.wait().await;
    Ok(())
}
