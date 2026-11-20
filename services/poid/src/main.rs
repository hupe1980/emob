//! `poid` — the daemon that publishes an operator's charge points.
//!
//! Sockets, a clock and a cadence. The documents are [`emob_poi`]'s and the
//! decisions about what may be said are its; what this process adds is *when* a
//! snapshot goes out and *whether the access point took it* — see [`poid`].
//!
//! # Two rhythms, not one
//!
//! `[AFIR Art. 20(2)]` splits the duty in two, and so does the profile: the
//! **table** is the estate as it stands and goes out on a cadence, while a
//! **status** message is a change and goes out when there is one. They are not
//! interchangeable — a status message references a facility at the version the
//! table published it at, so a status that outran its own table is a document
//! every consumer drops without a word.

use std::net::SocketAddr;

use anyhow::{Context as _, Result};
use emob_poi::datex::Publisher;
use emob_poi::site::Facility;
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::Journal as _;
use emob_service::{Identity, Readiness, Server, Shutdown, estate, identity};
use poid::{Ledger, Poid};

/// What the daemon waits for before it takes traffic.
const INVENTORY: &str = "inventory";

/// How often the estate is republished, and how long a feed may go unrefreshed
/// before an operator is told.
///
/// A feed nobody refreshed is one route planners read as current, and nothing
/// about it errors.
const CADENCE: time::Duration = time::Duration::hours(1);

#[tokio::main]
async fn main() -> Result<()> {
    let me: Identity = identity!();
    emob_service::telemetry::init(me, "info,poid=debug");

    let http: SocketAddr = emob_service::serve::bind_address("poid", "127.0.0.1:9582")?;

    let readiness = Readiness::new().expecting(INVENTORY);
    let shutdown = Shutdown::new();

    // A real deployment loads the estate from the register and the prices from
    // what `tarifd` published. Both are arguments here for the reason every
    // instant in this workspace is one: a snapshot replayed two years later has
    // to produce the same bytes.
    // Which snapshot the access point actually holds is a fact about the past,
    // so it comes back from the journal rather than starting empty — a service
    // that forgot it answers *"no snapshot has ever been accepted"*, which is
    // safe (`status` refuses) and still wrong: it republishes the whole estate
    // after every restart, which is what an access point rate-limits.
    //
    // Its head is signed after every append, so a copy cut short — a restored
    // backup, an edited file — refuses to start rather than republishing as if
    // the access point held an older snapshot (SR-096).
    let file = emob_service::journal::for_daemon("poid")
        .context("opening poid's journal — see EMOB_JOURNAL_DIR")?;
    let journal_path = file.path().to_owned();
    tracing::info!(path = %journal_path.display(), "folding the journal");
    let journal = Signing::open(
        file,
        SigningKey::for_daemon("poid", &estate::process_env())?,
        head::beside(&journal_path),
    )
    .context("opening poid's journal against its signed head")?;
    let latest = journal.latest();
    let ledger = Ledger::open(journal).context("folding poid's journal")?;
    let held = ledger
        .last_accepted()
        .context("reading what the access point last took")?;

    let path = estate::path();
    let estate: poid::http::Estate =
        estate::load(&path).with_context(|| format!("reading the estate at {}", path.display()))?;
    let operators = estate::operator_tokens(&estate.operators, &estate::process_env())?;
    let sites = poid::http::sites(&estate)?;
    let view = poid::http::View::of(&sites, held.clone());

    let no_rates = |_: &emob_poi::site::ChargingPoint| None;
    let mut service = Poid::new(
        Publisher {
            country: estate.parties.operator.country_code().to_owned(),
            // The identifier the national access point issued (spec 012):
            // refused when absent rather than published as a placeholder
            // nobody at the Mobilithek ever issued.
            national_identifier: std::env::var("POID_NAP_IDENTIFIER")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .context(
                    "POID_NAP_IDENTIFIER is not set: the identifier the national access point \
                     issued this operator — `just dev-keys` sets DE-NAP-DEV for one machine",
                )?,
            language: "de".to_owned(),
        },
        Facility::new(format!("{}-TABLE", estate.parties.operator)),
        sites,
        &no_rates,
    );
    if let Some(accepted) = &held {
        service.restore(accepted.clone());
        tracing::info!(
            published_at = %accepted.published_at,
            accepted_at = %accepted.at,
            head = ledger.journal().head().map(|head| head.position).unwrap_or_default(),
            "the access point holds this snapshot, and a status message may address its versions"
        );
    } else {
        tracing::warn!(
            "no snapshot has ever been accepted: every point this operator runs is absent from \
             the feed `[AFIR Art. 20(2)]`"
        );
    }
    readiness.up(INVENTORY);

    let now = time::OffsetDateTime::now_utc();
    if let Some(stale) = service.stale(now, CADENCE) {
        tracing::warn!(feed = %stale, "the national access point is behind");
    }
    // The push itself is a socket the deployment supplies: `snapshotPush` to
    // the Mobilithek, with the operator's credentials — which is the one leg of
    // M12 that is a subscription rather than a client. Only a push that came
    // back accepted is recorded, which is why `Ledger::accepted` is a separate
    // call and why it writes the record **after** the answer.
    let _ = (&service, &ledger);

    let signal = shutdown.clone();
    tokio::spawn(async move { signal.on_signal().await });

    Server::new(me, readiness, shutdown)
        .with(poid::http::router(view, operators.clone()).merge(
            emob_service::serve::operator_only(head::router("poid", latest), operators),
        ))
        .draining_for(std::time::Duration::from_secs(3))
        .listen(http)
        .await?;
    Ok(())
}
