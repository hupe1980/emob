//! Spec 007: the operator view on day 400 of a busy estate.
//!
//! A `csmsd` state holding 1 000 000 settled sessions and 50 000 refusals over
//! 400 days, built in memory rather than through the network. The refusals go
//! through `journal::apply`, which is what builds the index the view reads;
//! the settled sessions are written into the same fields `apply` writes —
//! their outcome, their day in the tally and their receipt's index entry —
//! without a `Cdr` each, whose memory is the benchmark milestone's question
//! and not one the read path asks.
//!
//! The view is then read the way `deskd` reads it — serialised, parsed into
//! its own model — and Heute is rendered from it (FR-004). The size of the
//! view, the receipt lookup and the SC-001 time budgets (50 ms for the view,
//! 100 ms for the page) are asserted on every run, debug or release. Run with
//! `--release -- --nocapture` for the numbers.

use std::time::Instant;

use csmsd::Outcome;
use csmsd::journal::{Event, State, apply};
use csmsd::read::Today;
use emob_core::TimeZone;
use emob_service::page::Page;
use emob_session::success::Ending;

const SETTLED: u64 = 1_000_000;
const REFUSED: u64 = 50_000;
const STATIONS: u64 = 500;
const DAYS: u64 = 400;

fn estate() -> (State, time::OffsetDateTime) {
    let mut state = State::new(TimeZone::new("Europe/Berlin").unwrap());
    let first = time::macros::datetime!(2025-10-15 06:00 UTC);
    let sessions = SETTLED + REFUSED;
    let per_day = sessions / DAYS;
    let mut at = first;
    for n in 0..sessions {
        // Spread over the days, a few seconds apart within one.
        at = first
            + time::Duration::days(i64::try_from(n / per_day).unwrap())
            + time::Duration::seconds(i64::try_from(n % per_day).unwrap() * 30);
        let station = format!("CP-{}", n % STATIONS);
        let transaction = n.to_string();
        if n % (sessions / REFUSED) == 0 {
            apply(
                &mut state,
                Event::Noted {
                    closes: Some((station.clone(), transaction.clone())),
                    outcome: Outcome::Refused {
                        identity: station,
                        transaction,
                        reasons: vec!["record 2: the signature does not verify".to_owned()],
                        findings: Vec::new(),
                    },
                    at,
                },
            );
        } else {
            let key = format!("DE*ABC/{station}-{transaction}");
            state
                .success
                .record(state.zone.local(at).date, &station, Ending::Billed);
            state
                .receipt_for
                .insert(key.clone(), (station, transaction));
            state.outcomes.push(Outcome::Settled {
                key,
                energy: "11.250 kWh".to_owned(),
                retries: 0,
                gap: Vec::new(),
            });
        }
    }
    (state, at)
}

#[test]
fn sc_001_the_bounded_view_is_small_and_fast_at_a_million_sessions() {
    let started = Instant::now();
    let (state, now) = estate();
    let built_state = started.elapsed();
    assert_eq!(state.refusals.len() as u64, REFUSED);
    assert_eq!(state.outcomes.len() as u64, SETTLED + REFUSED);
    assert!(
        state.success.len() <= 401,
        "{} days held",
        state.success.len()
    );

    let mut slowest = std::time::Duration::ZERO;
    let mut view = None;
    for _ in 0..5 {
        let started = Instant::now();
        let today = Today::from_state(
            &state,
            now,
            50,
            None,
            1,
            Page::from_offset(std::iter::empty(), 0, 0, 50),
        );
        slowest = slowest.max(started.elapsed());
        view = Some(today);
    }
    let view = view.unwrap();
    let body = serde_json::to_vec(&view).unwrap();

    // The view's size is bounded by the page, not by the history.
    assert_eq!(view.refused.total, REFUSED);
    assert_eq!(view.refused.items.len(), 50);
    assert!(view.refused.older.is_some());
    assert!(view.success.today.estate.attempted > 0);
    assert!(view.success.week.estate.attempted > view.success.today.estate.attempted);
    assert!(body.len() < 256 * 1024, "{} bytes", body.len());

    // The page after "show older" starts strictly before the cursor, and the
    // largest page is still small.
    let older = Today::from_state(
        &state,
        now,
        200,
        view.refused.older,
        1,
        Page::from_offset(std::iter::empty(), 0, 0, 200),
    );
    assert_eq!(older.refused.items.len(), 200);
    assert!(serde_json::to_vec(&older).unwrap().len() < 256 * 1024);

    // FR-003: a record's receipt is found by its key.
    let started = Instant::now();
    let found = state.receipt_for.get("DE*ABC/CP-1-1");
    let lookup = started.elapsed();
    assert_eq!(found, Some(&("CP-1".to_owned(), "1".to_owned())));

    // FR-004: the page, from exactly what deskd would have read.
    let read: deskd::model::Csmsd = serde_json::from_slice(&body).unwrap();
    let snapshot = deskd::heute::Snapshot {
        accounting: false,
        access: emob_service::authority::OperatorAccess::default(),
        csmsd: deskd::fetch::Source::Up(read),
        billd: deskd::fetch::Source::Up(deskd::model::Billd::default()),
        roamd: deskd::fetch::Source::Up(deskd::model::Roamd::default()),
        now,
        zone: TimeZone::new("Europe/Berlin").unwrap(),
        at: deskd::heute::Cursors::default(),
        journals: Vec::new(),
        publication: None,
    };
    let started = Instant::now();
    let page = deskd::heute::render(&snapshot, emob_service::ui::Lang::De);
    let rendered = started.elapsed();
    assert!(
        page.contains("50 von 50000 angezeigt"),
        "the count is the history's"
    );
    assert!(page.contains("Ältere anzeigen"));
    assert!(page.contains(&format!(
        "{} von {} Ladevorgängen abgerechnet",
        view.success.today.estate.billed, view.success.today.estate.attempted
    )));
    assert!(rendered.as_millis() < 100, "Heute rendered in {rendered:?}");

    println!(
        "heute at scale: {SETTLED} settled + {REFUSED} refused over {DAYS} days, state built in \
         {built_state:?}; bounded view built in {slowest:?} (slowest of 5), {} bytes; at limit \
         200, {} bytes; receipt lookup {lookup:?}; Heute rendered in {rendered:?}, {} bytes",
        body.len(),
        serde_json::to_vec(&older).unwrap().len(),
        page.len(),
    );
    // SC-001: 50 ms. Measured at about 2 ms in a debug build, so the same
    // budget holds there with room for a loaded machine — and a read path that
    // walked the million outcomes would not.
    let budget = 50;
    assert!(
        slowest.as_millis() < budget,
        "the bounded view took {slowest:?}, over {budget} ms"
    );
}
