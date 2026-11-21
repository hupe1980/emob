//! The page as structure: what it shows, in which order, in which language,
//! and what it refuses to let text become.

use super::*;
use crate::model::{Consignment, Document, Point, Refusal, Station, Windows};
use emob_eichrecht::{Disqualifies, Finding, FindingKind};
use emob_session::success::{Ending, Tally};
use time::macros::datetime;

const NOW: time::OffsetDateTime = datetime!(2026-11-18 09:42 UTC);

fn zone() -> TimeZone {
    TimeZone::new("Europe/Berlin").unwrap()
}

fn tampered(identity: &str, transaction: &str) -> Refusal {
    Refusal {
        identity: identity.into(),
        transaction: transaction.into(),
        reasons: vec!["record 2: the signature does not verify".into()],
        findings: vec![Finding {
            kind: FindingKind::Signature,
            takes: Disqualifies::Both,
            detail: "record 2: the signature does not verify".into(),
        }],
    }
}

fn station(identity: &str, silent_since: time::OffsetDateTime) -> Station {
    Station {
        identity: identity.into(),
        evse: format!("DE*ABC*E{identity}"),
        heartbeat_secs: 300,
        silent_since,
        network: Some("26201".into()),
        carrier: Some("Telekom Deutschland".into()),
        note: Some("Zugang über <b>Tor 3</b>".into()),
        reserved_until: Some("14:30".into()),
    }
}

fn page<T>(items: Vec<T>) -> Page<T> {
    Page {
        total: items.len() as u64,
        items,
        older: None,
    }
}

/// A window as `csmsd` serves it: the estate and its weakest three.
fn window(tally: &Tally) -> Window {
    Window {
        estate: tally.estate.clone(),
        worst: tally
            .worst(3)
            .into_iter()
            .map(|(point, rate)| Point {
                point: point.to_owned(),
                rate: rate.clone(),
            })
            .collect(),
    }
}

fn tally() -> Tally {
    let mut tally = Tally::default();
    tally.record("CP-1", Ending::Billed);
    tally.record("CP-1", Ending::Failed(Cause::Evidence));
    tally.record("CP-2", Ending::Failed(Cause::Authorisation));
    tally
}

fn week() -> Tally {
    let mut week = tally();
    for _ in 0..9 {
        week.record("CP-1", Ending::Billed);
    }
    week
}

fn busy() -> Snapshot {
    Snapshot {
        csmsd: Source::Up(Csmsd {
            success: Windows {
                today: window(&tally()),
                week: window(&week()),
            },
            refused: page(vec![tampered("CP-1", "2")]),
            silent: page(vec![station("CP-2", datetime!(2026-11-18 08:58 UTC))]),
            networks: Vec::new(),
        }),
        billd: Source::Up(Billd {
            invoices: page(vec![Document {
                number: "R-2026-11-0001".into(),
                total: "6.56 EUR".into(),
                submission: Submission::Issued,
            }]),
        }),
        roamd: Source::Up(Roamd {
            consignments: page(vec![Consignment {
                key: "DE*ABC/CP-1-1".into(),
                recipient: "DE*MSP".into(),
                delivery: "refused".into(),
                reason: Some("unknown token".into()),
            }]),
        }),
        now: NOW,
        zone: zone(),
        at: Cursors::default(),
        journals: Vec::new(),
    }
}

fn quiet() -> Snapshot {
    let mut tally = Tally::default();
    tally.record("CP-1", Ending::Billed);
    Snapshot {
        csmsd: Source::Up(Csmsd {
            success: Windows {
                today: window(&tally),
                week: window(&tally),
            },
            ..Csmsd::default()
        }),
        billd: Source::Up(Billd::default()),
        roamd: Source::Up(Roamd::default()),
        // A day with no duty within ninety days of it does not exist in this
        // calendar, so the calm day is far past its last entry.
        now: datetime!(2040-06-01 09:00 UTC),
        zone: zone(),
        at: Cursors::default(),
        journals: Vec::new(),
    }
}

fn position(page: &str, needle: &str) -> usize {
    page.find(needle)
        .unwrap_or_else(|| panic!("{needle} is not on the page"))
}

#[test]
fn one_h1_a_language_and_every_group_labelled() {
    let page = render(&busy(), Lang::De);
    assert_eq!(page.matches("<h1").count(), 1);
    assert!(page.contains("<html lang=\"de\">"));
    assert!(page.contains("<h1>Heute</h1>"));
    for id in [
        "rate",
        "queue",
        "refused",
        "silent",
        "consignments",
        "invoices",
        "duties",
    ] {
        assert!(page.contains(&format!("id=\"{id}\"")), "{id}");
        assert!(
            page.contains(&format!("aria-labelledby=\"{id}-h\"")),
            "{id} is labelled"
        );
    }
    let en = render(&busy(), Lang::En);
    assert!(en.contains("<html lang=\"en\">") && en.contains("<h1>Today</h1>"));
    assert!(en.contains("Needs you today") && !en.contains("Braucht dich"));
}

#[test]
fn the_queue_is_in_the_order_a_person_works_it() {
    let page = render(&busy(), Lang::De);
    let order = [
        "id=\"refused\"",
        "id=\"silent\"",
        "id=\"consignments\"",
        "id=\"invoices\"",
        "id=\"duties\"",
    ];
    let at: Vec<usize> = order.iter().map(|id| position(&page, id)).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    // The rate heads the page.
    assert!(position(&page, "id=\"rate\"") < position(&page, "id=\"queue\""));
}

#[test]
fn a_refused_session_says_why_with_its_paragraph_and_a_stable_link() {
    let page = render(&busy(), Lang::De);
    assert!(page.contains("id=\"s-CP-1-2\""));
    assert!(page.contains("href=\"#s-CP-1-2\""));
    assert!(page.contains("Eine Signatur passt nicht"));
    assert!(page.contains("<span class=\"cite\">[MessEG §33]</span>"));
    assert!(page.contains("<summary>Warum?</summary>"));
    assert!(page.contains("Abrechenbar, wenn"));
    // The English link carries its language.
    assert!(render(&busy(), Lang::En).contains("href=\"?lang=en#s-CP-1-2\""));
}

#[test]
fn the_rate_shows_the_figure_the_fraction_and_the_causes() {
    let page = render(&busy(), Lang::De);
    assert!(page.contains("33,3&#8239;%"), "{page}");
    assert!(page.contains("1 von 3 Ladevorgängen abgerechnet"));
    assert!(page.contains("<meter min=\"0\" max=\"3\" value=\"1\""));
    assert!(page.contains("Autorisierung <span class=\"num\">1</span>"));
    assert!(page.contains("Nachweis <span class=\"num\">1</span>"));
    assert!(
        !page.contains("Stationsfehler"),
        "a cause with none is not listed"
    );
    // The weakest point first.
    assert!(position(&page, "/station/CP-2") < position(&page, "/station/CP-1"));
    assert!(render(&busy(), Lang::En).contains("33.3&#8239;%"));
}

#[test]
fn us2_the_rate_is_today_and_the_seven_days_beside_it() {
    let page = render(&busy(), Lang::De);
    let today = position(&page, "<h3 id=\"rate-today\" class=\"eyebrow\">Heute</h3>");
    let week = position(&page, "<h3 id=\"rate-week\" class=\"eyebrow\">7 Tage</h3>");
    assert!(today < week);
    assert!(page[week..].contains("10 von 12 Ladevorgängen abgerechnet"));
    assert!(page[today..week].contains("1 von 3 Ladevorgängen abgerechnet"));
    let en = render(&busy(), Lang::En);
    assert!(en.contains(">Today</h3>") && en.contains(">7 days</h3>"));
}

#[test]
fn us2_a_quiet_morning_says_so_rather_than_showing_yesterday() {
    let mut snapshot = busy();
    let Source::Up(csmsd) = &mut snapshot.csmsd else {
        unreachable!()
    };
    csmsd.success.today = Window::default();
    let page = render(&snapshot, Lang::De);
    let today = &page[position(&page, "rate-today")..position(&page, "rate-week")];
    assert!(today.contains("Heute noch keine Ladevorgänge."), "{today}");
    assert!(!today.contains("<meter"));
    assert!(page.contains("10 von 12 Ladevorgängen abgerechnet"));
}

#[test]
fn a_silent_station_an_unaccepted_record_and_a_waiting_invoice_are_rows() {
    let page = render(&busy(), Lang::De);
    assert!(page.contains("id=\"cp-CP-2\""));
    assert!(
        page.contains(
            "Kein Lebenszeichen seit <time datetime=\"2026-11-18T08:58:00Z\">09:58</time>"
        )
    );
    assert!(page.contains("unknown token"));
    assert!(page.contains("R-2026-11-0001"));
}

#[test]
fn a_duty_says_how_far_off_it_is_and_gives_the_exact_date_on_focus() {
    let page = render(&busy(), Lang::De);
    // 18.11.2026 → 01.01.2027 is 44 days.
    assert!(page.contains("in 44 Tagen"), "{page}");
    assert!(page.contains(
        "datetime=\"2027-01-01\" tabindex=\"0\" title=\"01.01.2027\" data-exact=\"01.01.2027\""
    ));
    assert!(render(&busy(), Lang::En).contains("in 44 days"));
}

#[test]
fn nothing_to_do_is_one_sentence_and_no_empty_list() {
    let page = render(&quiet(), Lang::De);
    assert!(page.contains("class=\"calm tone-good\""));
    assert!(page.contains("Heute braucht nichts deine Aufmerksamkeit"));
    assert!(!page.contains("<ol class=\"rows\">"), "no empty table");
    assert!(page.contains("Jeder Versuch wurde abgerechnet."));
}

#[test]
fn sr_054_a_daemon_that_is_down_is_named_with_since_when_and_the_rest_renders() {
    let mut snapshot = busy();
    snapshot.billd = Source::Down {
        since: datetime!(2026-11-18 09:41 UTC),
        why: Why::NoAnswer,
    };
    let page = render(&snapshot, Lang::De);
    let invoices = &page[position(&page, "id=\"invoices\"")..position(&page, "id=\"duties\"")];
    assert!(
        invoices.contains("<strong>billd</strong> antwortet nicht — seit"),
        "{invoices}"
    );
    assert!(invoices.contains("10:41"));
    assert!(
        page.contains("id=\"s-CP-1-2\""),
        "the refused session still renders"
    );
    assert!(
        !page.contains("class=\"calm"),
        "a page with a daemon down is not calm"
    );

    let mut quiet = quiet();
    quiet.roamd = Source::Down {
        since: NOW,
        why: Why::Refused,
    };
    let page = render(&quiet, Lang::En);
    assert!(
        !page.contains("class=\"calm"),
        "SR-054: calm only when every source answered"
    );
    assert!(page.contains("check the token"));
}

#[test]
fn sr_051_text_from_a_read_model_never_becomes_markup() {
    let mut snapshot = busy();
    snapshot.csmsd = Source::Up(Csmsd {
        refused: page(vec![Refusal {
            identity: "<script>alert(1)</script>".into(),
            transaction: "\"><img src=x onerror=alert(2)>".into(),
            reasons: vec!["<b>bold</b>".into()],
            findings: Vec::new(),
        }]),
        silent: page(vec![station("<i>x</i>", NOW)]),
        ..Csmsd::default()
    });
    snapshot.roamd = Source::Up(Roamd {
        consignments: page(vec![Consignment {
            key: "k".into(),
            recipient: "<svg onload=alert(3)>".into(),
            delivery: "refused".into(),
            reason: Some("</li><script>alert(4)</script>".into()),
        }]),
    });
    let page = render(&snapshot, Lang::De);
    assert_eq!(
        page.matches("<script").count(),
        1,
        "only the page's own script tag"
    );
    assert!(
        !page.contains("<img")
            && !page.contains("<svg")
            && !page.contains("<b>")
            && !page.contains("<i>")
    );
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!page.contains("onerror=alert(2)>"));
}

/// What `csmsd` serves for 10 000 refusals: the newest page and the count.
fn ten_thousand_refused(older_than: u64) -> Page<Refusal> {
    let items: Vec<Refusal> = (0..older_than)
        .rev()
        .take(ROWS)
        .map(|n| tampered("CP-1", &n.to_string()))
        .collect();
    let oldest = older_than.saturating_sub(items.len() as u64);
    Page {
        total: 10_000,
        items,
        older: (oldest > 0).then_some(oldest),
    }
}

#[test]
fn sr_055_ten_thousand_refusals_render_fifty_newest_first_with_the_count() {
    let mut snapshot = busy();
    snapshot.csmsd = Source::Up(Csmsd {
        refused: ten_thousand_refused(10_000),
        ..Csmsd::default()
    });
    let page = render(&snapshot, Lang::De);
    let refused = &page[position(&page, "id=\"refused\"")..position(&page, "id=\"invoices\"")];
    assert_eq!(refused.matches("<li id=\"s-").count(), ROWS);
    assert!(refused.contains("<span class=\"count\">10000</span>"));
    assert!(refused.contains("50 von 10000 angezeigt"));
    assert!(
        position(refused, "s-CP-1-9999") < position(refused, "s-CP-1-9998"),
        "newest first"
    );
    // SC-002: under 30 kB compressed; the markup repeats, so the raw bound is
    // a generous stand-in.
    assert!(page.len() < 120 * 1024, "{} bytes", page.len());
}

#[test]
fn us3_sr_095_older_is_one_plain_link_built_from_the_cursor_alone() {
    let mut snapshot = busy();
    snapshot.csmsd = Source::Up(Csmsd {
        refused: ten_thousand_refused(10_000),
        ..Csmsd::default()
    });
    let page = render(&snapshot, Lang::De);
    assert!(
        page.contains(
            "<a class=\"noprint\" href=\"?lang=de&amp;refused=9950#refused\">Ältere anzeigen</a>"
        ),
        "{page}"
    );
    assert!(
        !page.contains("Neueste anzeigen"),
        "the newest page needs no way back"
    );
    // Opened at that cursor: the next page, a way back, and the next cursor.
    snapshot.csmsd = Source::Up(Csmsd {
        refused: ten_thousand_refused(9_950),
        ..Csmsd::default()
    });
    snapshot.at.refused = Some(9_950);
    let page = render(&snapshot, Lang::En);
    assert!(page.contains("id=\"s-CP-1-9949\""));
    assert!(page.contains("href=\"?lang=en#refused\">Show newest</a>"));
    assert!(page.contains("href=\"?lang=en&amp;refused=9900#refused\">Show older</a>"));
    // A station list says "more": it is not history.
    snapshot.csmsd = Source::Up(Csmsd {
        silent: Page {
            total: 120,
            items: (0..50).map(|n| station(&format!("CP-{n}"), NOW)).collect(),
            older: Some(50),
        },
        ..Csmsd::default()
    });
    let page = render(&snapshot, Lang::De);
    assert!(page.contains("href=\"?lang=de&amp;silent=50#silent\">Weitere anzeigen</a>"));
}

#[test]
fn sc_001_heute_renders_from_bounded_inputs_in_time() {
    // The largest input a bounded view can give: 200 rows in every list.
    let rows = 200;
    let snapshot = Snapshot {
        csmsd: Source::Up(Csmsd {
            success: Windows {
                today: window(&tally()),
                week: window(&week()),
            },
            refused: Page {
                total: 50_000,
                items: (0..rows)
                    .map(|n| tampered("CP-1", &n.to_string()))
                    .collect(),
                older: Some(1),
            },
            silent: Page {
                total: 10_000,
                items: (0..rows)
                    .map(|n| station(&format!("CP-{n}"), NOW))
                    .collect(),
                older: Some(200),
            },
            networks: Vec::new(),
        }),
        ..busy()
    };
    let started = std::time::Instant::now();
    let page = render(&snapshot, Lang::De);
    let took = started.elapsed();
    println!("Heute from 200-row inputs: {took:?}, {} bytes", page.len());
    // At most ROWS rows per list, whatever the daemon sent.
    assert_eq!(page.matches("<li id=\"s-").count(), ROWS);
    // SC-001: under 100 ms in release; a debug build gets ten times that.
    let budget = if cfg!(debug_assertions) { 1_000 } else { 100 };
    assert!(took.as_millis() < budget, "Heute rendered in {took:?}");
}

#[test]
fn sr_053_sr_057_the_page_names_no_token_and_no_card() {
    let page = render(&busy(), Lang::De);
    for absent in ["Bearer", "operator-token", "04A1B2C3D4E5F6", "DE-MSP-C"] {
        assert!(!page.contains(absent), "{absent}");
    }
}

#[test]
fn the_shell_is_the_shared_one_and_loads_only_its_own_script() {
    let page = render(&busy(), Lang::De);
    assert!(page.contains("href=\"/ui/emob.css\""));
    assert!(page.contains("<script type=\"module\" src=\"/ui/heute.js\"></script>"));
    assert!(!page.contains("http://") && !page.contains("https://"));
    assert!(
        !page.contains("style=\""),
        "no inline style: the policy forbids it"
    );
}

#[test]
fn sr_098_a_head_that_went_backwards_is_red_on_heute_and_breaks_the_calm() {
    use emob_service::witness::{Seen, Verdict};
    let seen = |daemon: &str, alarm: Option<Verdict>| Seen {
        daemon: daemon.to_owned(),
        position: 57,
        hash: "ab".repeat(32),
        at: NOW,
        key: "emob/x+00000000+AQ==".to_owned(),
        key_from_estate: false,
        last: 41,
        alarm,
    };
    let mut snapshot = quiet();
    snapshot.journals = vec![seen("billd", None)];
    let calm = render(&snapshot, Lang::De);
    assert!(calm.contains("Journale"), "{calm}");
    assert!(calm.contains("<b>billd</b> bei 57"), "{calm}");
    assert!(calm.contains("beim ersten Mal gemerkt"), "{calm}");

    snapshot.journals = vec![
        seen("billd", None),
        seen("csmsd", Some(Verdict::Regressed { witnessed: 57 })),
    ];
    let alarmed = render(&snapshot, Lang::De);
    assert!(alarmed.contains("zurückgegangen"), "{alarmed}");
    assert!(
        alarmed.contains("bezeugt bei 57, danach bei 41"),
        "{alarmed}"
    );
    assert!(
        !alarmed.contains("Heute braucht nichts deine Aufmerksamkeit"),
        "a lost tail is not a calm morning"
    );
    let english = render(&snapshot, Lang::En);
    assert!(
        english.contains("witnessed at 57, then seen at 41"),
        "{english}"
    );
}

#[test]
fn an_estate_not_witnessed_yet_says_so() {
    let page = render(&quiet(), Lang::De);
    assert!(page.contains("Noch nicht bezeugt"), "{page}");
}

#[test]
fn silent_stations_on_one_network_are_said_once_as_a_likely_outage() {
    use crate::model::NetworkShare;
    let mut snapshot = busy();
    if let Source::Up(csmsd) = &mut snapshot.csmsd {
        csmsd.silent = page(vec![
            station("CP-2", NOW),
            station("CP-3", NOW),
            station("CP-4", NOW),
        ]);
        csmsd.networks = vec![NetworkShare {
            network: "26201".into(),
            carrier: Some("Telekom Deutschland".into()),
            silent: 3,
            stations: 4,
        }];
    }
    let page = render(&snapshot, Lang::De);
    assert!(
        page.contains("Telekom Deutschland</strong></a> 3 von 4 still"),
        "{page}"
    );
    assert!(page.contains("/stations?q=26201+offline"), "{page}");
    assert!(page.contains("eher ein Netzausfall"), "{page}");

    // One silent station per network is not a pattern, and is not said.
    if let Source::Up(csmsd) = &mut snapshot.csmsd {
        csmsd.networks[0].silent = 1;
    }
    assert!(!render(&snapshot, Lang::De).contains("Nach Mobilfunknetz"));
}

#[test]
fn sr_155_a_silent_row_shows_its_newest_note_as_text() {
    let page = render(&busy(), Lang::De);
    assert!(
        page.contains("Zugang über &lt;b&gt;Tor 3&lt;/b&gt;"),
        "{page}"
    );
    assert!(!page.contains("<b>Tor 3</b>"));
    assert!(
        page.contains("Reserviert bis 14:30"),
        "a driver is on the way to a silent station"
    );
}
