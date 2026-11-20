//! Heute: the page, as a pure function of what the daemons said.
//!
//! A queue, not a dashboard. The header is the one figure the estate is judged
//! by — the charging success rate — and everything below it is something a
//! person can act on today, most urgent first: a session the chain refused, a
//! station gone quiet, a record a partner has not taken, an invoice its
//! recipient has not accepted, a duty that starts within ninety days. A group
//! with nothing in it is not shown; a page with nothing in it says so in one
//! sentence. A daemon that did not answer is named, with since when, and its
//! groups say so rather than looking calm (SR-054).
//!
//! Nothing here decides. The rate is `emob-session`'s tally, folded by `csmsd`;
//! the findings and their paragraphs are `emob-eichrecht`'s; the duties are the
//! calendar's. Every value that came from outside this file goes through
//! [`escape`] (SR-051), and every list stops at [`ROWS`] with a count (SR-055).
//!
//! # What it reads is bounded too (spec 007)
//!
//! The daemons serve the newest [`ROWS`] of each list with its total and a
//! cursor; a list with more ends in "Ältere anzeigen", a plain link to the same
//! page at the list's next cursor — no script needed, and stable while new
//! sessions arrive. The cursor is a number the daemon gave and the link is
//! built from that number alone (SR-095). The rate is shown for **today** and
//! **the seven days ending today**, so a bad morning is not hidden by a good
//! year.

use core::fmt::Write as _;

use emob_core::TimeZone;
use emob_core::obligation::{self, Scope};
use emob_service::ui::{self, Lang, escape};
use emob_session::success::{Cause, Rate};

use crate::fetch::{Source, Why};
use crate::model::{Billd, Csmsd, Page, Roamd, Submission, Window};
use crate::why;

/// The most rows one group renders; the rest is a count.
pub const ROWS: usize = 50;

/// How far ahead the calendar is read.
pub const DUTY_HORIZON_DAYS: i64 = 90;

/// Where the page's script is served.
pub const SCRIPT_PATH: &str = "/ui/heute.js";

/// Everything one render reads.
#[derive(Debug)]
pub struct Snapshot {
    /// `csmsd`'s read model.
    pub csmsd: Source<Csmsd>,
    /// `billd`'s.
    pub billd: Source<Billd>,
    /// `roamd`'s.
    pub roamd: Source<Roamd>,
    /// Now.
    pub now: time::OffsetDateTime,
    /// The zone the page tells the time in.
    pub zone: TimeZone,
    /// The cursor each list was opened at; `None` is its newest page.
    pub at: Cursors,
    /// What the witness holds about each daemon's journal (spec 008).
    pub journals: Vec<emob_service::witness::Seen>,
}

/// Where each paged list starts — the `?<list>=<n>` the page was opened with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursors {
    /// Refused sessions older than this position.
    pub refused: Option<u64>,
    /// Silent stations from this offset on.
    pub silent: Option<u64>,
    /// Invoices older than this position.
    pub invoices: Option<u64>,
    /// Consignments older than this position.
    pub consignments: Option<u64>,
}

impl Cursors {
    const fn of(self, list: &str) -> Option<u64> {
        match list.as_bytes() {
            b"refused" => self.refused,
            b"silent" => self.silent,
            b"invoices" => self.invoices,
            b"consignments" => self.consignments,
            _ => None,
        }
    }
}

/// One row of the queue.
struct Row {
    id: String,
    tone: Tone,
    body: String,
}

#[derive(Clone, Copy)]
enum Tone {
    Bad,
    Warn,
    Info,
}

impl Tone {
    const fn edge(self) -> &'static str {
        match self {
            Self::Bad => "edge-bad",
            Self::Warn => "edge-warn",
            Self::Info => "edge-info",
        }
    }

    const fn class(self) -> &'static str {
        match self {
            Self::Bad => "tone-bad",
            Self::Warn => "tone-warn",
            Self::Info => "tone-info",
        }
    }
}

/// A group of rows, or the daemon that should have supplied them.
enum Group {
    Rows {
        total: u64,
        rows: Vec<Row>,
        /// The cursor of the next page, for a list the daemon pages.
        older: Option<u64>,
        /// A sentence above the rows that says what they share, if anything.
        lead: String,
    },
    Down {
        daemon: &'static str,
        since: time::OffsetDateTime,
        why: Why,
    },
}

impl Group {
    const fn is_empty(&self) -> bool {
        matches!(self, Self::Rows { total: 0, .. })
    }

    const fn len(&self) -> u64 {
        match self {
            Self::Rows { total, .. } => *total,
            Self::Down { .. } => 0,
        }
    }
}

/// Render the page.
#[must_use]
pub fn render(snapshot: &Snapshot, lang: Lang) -> String {
    let local = snapshot.zone.local(snapshot.now);
    let today = local.date;
    let link = Links(lang);

    let groups: [(&str, &str, Group); 5] = [
        (
            "refused",
            lang.pick("Abgelehnte Ladevorgänge", "Refused sessions"),
            refused(snapshot, lang, link),
        ),
        (
            "silent",
            lang.pick("Stationen ohne Lebenszeichen", "Stations gone silent"),
            silent(snapshot, lang, link),
        ),
        (
            "consignments",
            lang.pick("Vom Partner nicht angenommen", "Not taken by the partner"),
            consignments(snapshot, lang, link),
        ),
        (
            "invoices",
            lang.pick("Rechnungen ohne Annahme", "Invoices awaiting acceptance"),
            invoices(snapshot, lang, link),
        ),
        (
            "duties",
            lang.pick("Fristen der nächsten 90 Tage", "Duties in the next 90 days"),
            duties(today, lang, link),
        ),
    ];

    let all_up = snapshot.csmsd.up().is_some()
        && snapshot.billd.up().is_some()
        && snapshot.roamd.up().is_some()
        && snapshot.journals.iter().all(|seen| seen.alarm.is_none());
    let waiting: u64 = groups.iter().map(|(_, _, group)| group.len()).sum();

    let mut body = head(snapshot, lang);
    body.push_str(&rate(snapshot, lang));

    let _ = write!(
        body,
        "<section id=\"queue\" aria-labelledby=\"queue-h\"><h2 id=\"queue-h\">{title}{count}</h2>",
        title = lang.pick("Braucht dich heute", "Needs you today"),
        count = if waiting > 0 {
            format!(" <span class=\"count\">{waiting}</span>")
        } else {
            String::new()
        },
    );
    if waiting == 0 && all_up {
        let _ = write!(
            body,
            "<p class=\"calm tone-good\"><span aria-hidden=\"true\">✓</span> {}</p>",
            lang.pick(
                "Heute braucht nichts deine Aufmerksamkeit — jeder Vorgang ist abgerechnet, jede Station meldet sich.",
                "Nothing needs you today — every session billed, every station is talking.",
            )
        );
    }
    for (id, title, group) in &groups {
        if group.is_empty() {
            continue;
        }
        body.push_str(&render_group(id, title, group, lang, snapshot));
    }
    body.push_str("</section>");

    body.push_str(&journals(snapshot, lang));
    body.push_str(&footer(snapshot, lang));

    ui::page(
        &ui::Shell {
            title: lang.pick("Heute", "Today"),
            place: lang.pick("Betrieb", "Operations"),
            lang,
            reading: false,
            script: Some(SCRIPT_PATH),
        },
        &body,
    )
}

/// The page's title, its date and clock, and the other language.
fn head(snapshot: &Snapshot, lang: Lang) -> String {
    let local = snapshot.zone.local(snapshot.now);
    let today = local.date;
    format!(
        "<div class=\"head\"><h1>{h1}</h1><p class=\"when\">{date} · {stand} \
         <time datetime=\"{iso}\">{clock}</time> · <a href=\"{other}\" lang=\"{other_lang}\" \
         hreflang=\"{other_lang}\">{other_name}</a> · <a href=\"/stations\">{stations}</a></p></div>",
        stations = lang.pick("Stationen", "Stations"),
        h1 = lang.pick("Heute", "Today"),
        date = escape(&format!(
            "{}, {}",
            weekday(lang, local.weekday),
            lang.date(today)
        )),
        stand = lang.pick("Stand", "as of"),
        iso = escape(&iso(snapshot.now)),
        clock = clock(local.time),
        other = match lang {
            Lang::De => "?lang=en",
            Lang::En => "?lang=de",
        },
        other_lang = match lang {
            Lang::De => "en",
            Lang::En => "de",
        },
        other_name = match lang {
            Lang::De => "English",
            Lang::En => "Deutsch",
        },
    )
}

fn render_group(id: &str, title: &str, group: &Group, lang: Lang, snapshot: &Snapshot) -> String {
    let mut out = format!(
        "<section class=\"group\" id=\"{id}\" aria-labelledby=\"{id}-h\"><h3 id=\"{id}-h\">{title}",
        title = escape(title),
    );
    match group {
        Group::Down { daemon, since, why } => {
            let _ = write!(
                out,
                "</h3>{}",
                down(daemon, *since, *why, lang, &snapshot.zone)
            );
        }
        Group::Rows {
            total,
            rows,
            older,
            lead,
        } => {
            let _ = write!(
                out,
                " <span class=\"count\">{total}</span></h3>{lead}<ol class=\"rows\">"
            );
            for row in rows {
                let _ = write!(
                    out,
                    "<li id=\"{id}\" class=\"{edge}\">{body}</li>",
                    id = escape(&row.id),
                    edge = row.tone.edge(),
                    body = row.body,
                );
            }
            out.push_str("</ol>");
            out.push_str(&paging(
                id,
                *total,
                rows.len(),
                *older,
                lang,
                snapshot.at.of(id),
            ));
        }
    }
    out.push_str("</section>");
    out
}

/// Under a list: how much of it is shown, and the links to the rest — each a
/// plain link to this page at a cursor the daemon gave, so no script is
/// needed and nothing from the request is echoed (SR-095).
fn paging(
    list: &str,
    total: u64,
    shown: usize,
    older: Option<u64>,
    lang: Lang,
    at: Option<u64>,
) -> String {
    let shown = shown as u64;
    if total <= shown && at.is_none() {
        return String::new();
    }
    let lang_query = match lang {
        Lang::De => "lang=de",
        Lang::En => "lang=en",
    };
    // A station list is not history: "more", not "older".
    let (older_word, newest_word) = if list == "silent" {
        (
            lang.pick("Weitere anzeigen", "Show more"),
            lang.pick("Zum Anfang", "Back to the start"),
        )
    } else {
        (
            lang.pick("Ältere anzeigen", "Show older"),
            lang.pick("Neueste anzeigen", "Show newest"),
        )
    };
    let mut out = format!(
        "<p class=\"more\">{}",
        match lang {
            Lang::De => format!("{shown} von {total} angezeigt"),
            Lang::En => format!("{shown} of {total} shown"),
        }
    );
    if at.is_some() {
        let _ = write!(
            out,
            " · <a class=\"noprint\" href=\"?{lang_query}#{list}\">{newest_word}</a>"
        );
    }
    if let Some(older) = older {
        let _ = write!(
            out,
            " · <a class=\"noprint\" href=\"?{lang_query}&amp;{list}={older}#{list}\">{older_word}</a>"
        );
    }
    out.push_str("</p>");
    out
}

/// A daemon that did not answer, and since when (SR-054). The class only —
/// never the request or the body (SR-053).
fn down(
    daemon: &str,
    since: time::OffsetDateTime,
    why: Why,
    lang: Lang,
    zone: &TimeZone,
) -> String {
    let class = match (why, lang) {
        (Why::NoAnswer, Lang::De) => "keine Antwort".to_owned(),
        (Why::NoAnswer, Lang::En) => "no answer".to_owned(),
        (Why::Refused, Lang::De) => "lässt deskd nicht lesen — Token prüfen".to_owned(),
        (Why::Refused, Lang::En) => "does not let deskd read — check the token".to_owned(),
        (Why::Status(code), Lang::De) => format!("antwortet {code}"),
        (Why::Status(code), Lang::En) => format!("answers {code}"),
        (Why::Unreadable, Lang::De) => "Antwort nicht lesbar".to_owned(),
        (Why::Unreadable, Lang::En) => "answer unreadable".to_owned(),
    };
    let local = zone.local(since);
    format!(
        "<p class=\"down tone-warn\" role=\"status\"><span aria-hidden=\"true\">⚠</span> \
         <strong>{daemon}</strong> {verb} {since_word} <time datetime=\"{iso}\">{at}</time> ({class}).</p>",
        daemon = escape(daemon),
        verb = lang.pick("antwortet nicht —", "is not answering —"),
        since_word = lang.pick("seit", "since"),
        iso = escape(&iso(since)),
        at = clock(local.time),
        class = escape(&class),
    )
}

// ── the header ──────────────────────────────────────────────────────────────

fn rate(snapshot: &Snapshot, lang: Lang) -> String {
    let title = lang.pick("Ladeerfolg", "Charging success");
    let mut out = format!(
        "<section id=\"rate\" class=\"card rate\" aria-labelledby=\"rate-h\"><h2 id=\"rate-h\" class=\"eyebrow\">{title}</h2>"
    );
    let Some(csmsd) = snapshot.csmsd.up() else {
        if let Source::Down { since, why, .. } = &snapshot.csmsd {
            out.push_str(&down("csmsd", *since, *why, lang, &snapshot.zone));
        }
        out.push_str("</section>");
        return out;
    };
    out.push_str(&window(
        lang,
        "today",
        lang.pick("Heute", "Today"),
        &csmsd.success.today,
        lang.pick("Heute noch keine Ladevorgänge.", "No sessions yet today."),
    ));
    out.push_str(&window(
        lang,
        "week",
        lang.pick("7 Tage", "7 days"),
        &csmsd.success.week,
        lang.pick(
            "Keine Ladevorgänge in den letzten 7 Tagen.",
            "No sessions in the last 7 days.",
        ),
    ));
    out.push_str("</section>");
    out
}

/// One window of the rate: the figure, the fraction, why sessions failed and
/// the weakest points — or, on a quiet day, that nobody tried, never a figure
/// from another day.
fn window(lang: Lang, id: &str, title: &str, window: &Window, none: &str) -> String {
    let estate = &window.estate;
    let mut out = format!(
        "<div class=\"window\" aria-labelledby=\"rate-{id}\"><h3 id=\"rate-{id}\" class=\"eyebrow\">{title}</h3>"
    );
    match estate.percent() {
        None => {
            let _ = write!(
                out,
                "<p class=\"value\">—</p><p class=\"muted\">{none}</p></div>"
            );
            return out;
        }
        Some(percent) => {
            let _ = write!(
                out,
                "<p class=\"value\">{value}&#8239;%</p>\
                 <meter min=\"0\" max=\"{attempted}\" value=\"{billed}\" aria-label=\"{label}\">{billed}/{attempted}</meter>\
                 <p class=\"muted num\">{of}</p>",
                value = lang.decimal(percent),
                attempted = estate.attempted,
                billed = estate.billed,
                label = escape(&format!(
                    "{} {title}",
                    lang.pick("Ladeerfolg", "Charging success")
                )),
                of = of(lang, estate),
            );
        }
    }
    if estate.attempted > estate.billed {
        let _ = write!(
            out,
            "<h4 class=\"eyebrow\">{}</h4><ul class=\"chips\">",
            lang.pick("Woran es scheiterte", "Why sessions failed")
        );
        for cause in Cause::ALL {
            let count = estate.failed_by(cause);
            if count == 0 {
                continue;
            }
            let _ = write!(
                out,
                "<li><span class=\"badge {tone}\">{name} <span class=\"num\">{count}</span></span></li>",
                tone = match cause {
                    Cause::Cancelled => "tone-info",
                    _ => "tone-bad",
                },
                name = cause_name(lang, cause),
            );
        }
        out.push_str("</ul>");
        if !window.worst.is_empty() {
            let _ = write!(
                out,
                "<h4 class=\"eyebrow\">{}</h4><ol class=\"worst\">",
                lang.pick("Am schwächsten", "Weakest points")
            );
            for point in &window.worst {
                let _ = write!(
                    out,
                    "<li><a href=\"{href}\">{name}</a><span class=\"num\">{percent}&#8239;% · {of}</span></li>",
                    href = escape(&crate::station::href(&point.point)),
                    name = escape(&point.point),
                    percent = point
                        .rate
                        .percent()
                        .map_or_else(|| "—".to_owned(), |p| lang.decimal(p)),
                    of = of(lang, &point.rate),
                );
            }
            out.push_str("</ol>");
        }
    } else {
        let _ = write!(
            out,
            "<p class=\"muted\">{}</p>",
            lang.pick("Jeder Versuch wurde abgerechnet.", "Every attempt billed.")
        );
    }
    out.push_str("</div>");
    out
}

fn of(lang: Lang, rate: &Rate) -> String {
    match lang {
        Lang::De => format!(
            "{} von {} Ladevorgängen abgerechnet",
            rate.billed, rate.attempted
        ),
        Lang::En => format!("{} of {} sessions billed", rate.billed, rate.attempted),
    }
}

const fn cause_name(lang: Lang, cause: Cause) -> &'static str {
    match cause {
        Cause::Authorisation => lang.pick("Autorisierung", "Authorisation"),
        Cause::Evidence => lang.pick("Nachweis", "Evidence"),
        Cause::StationFault => lang.pick("Stationsfehler", "Station fault"),
        Cause::Cancelled => lang.pick("Vom Fahrer abgebrochen", "Cancelled by driver"),
    }
}

// ── the groups ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Links(Lang);

impl Links {
    /// A stable URL for one row: the page, its language, the row's anchor.
    fn to(self, anchor: &str) -> String {
        let query = match self.0 {
            Lang::De => "",
            Lang::En => "?lang=en",
        };
        format!(
            "<a class=\"act noprint\" href=\"{query}#{anchor}\" aria-label=\"{label}\">{text}</a>",
            anchor = escape(anchor),
            label = self.0.pick(
                "Dauerhafter Link zu diesem Eintrag",
                "Permanent link to this item"
            ),
            text = self.0.pick("Link", "Link"),
        )
    }
}

fn badge(tone: Tone, icon: &str, word: &str) -> String {
    format!(
        "<span class=\"badge {class}\"><span aria-hidden=\"true\">{icon}</span> {word}</span>",
        class = tone.class(),
        word = escape(word),
    )
}

fn bounded(rows: impl Iterator<Item = Row>, total: usize) -> Group {
    Group::Rows {
        total: total as u64,
        rows: rows.take(ROWS).collect(),
        older: None,
        lead: String::new(),
    }
}

/// A page the daemon cut: its rows as they came, its total and its cursor.
fn paged<T>(page: &Page<T>, row: impl Fn(&T) -> Row) -> Group {
    Group::Rows {
        total: page.total,
        rows: page.items.iter().take(ROWS).map(row).collect(),
        older: page.older,
        lead: String::new(),
    }
}

fn refused(snapshot: &Snapshot, lang: Lang, link: Links) -> Group {
    let csmsd = match &snapshot.csmsd {
        Source::Up(csmsd) => csmsd,
        Source::Down { since, why } => {
            return Group::Down {
                daemon: "csmsd",
                since: *since,
                why: *why,
            };
        }
    };
    // Newest first, as the daemon sent them.
    paged(&csmsd.refused, |refusal| {
        let id = format!(
            "s-{}-{}",
            slug(&refusal.identity),
            slug(&refusal.transaction)
        );
        let (headline, cite) = why::headline(lang, &refusal.findings, &refusal.reasons);
        let cite = cite.map_or_else(String::new, |cite| {
            format!(" <span class=\"cite\">{}</span>", escape(cite))
        });
        Row {
            body: format!(
                "<div><p class=\"what\">{badge} <a href=\"{href}\"><strong>{identity}</strong></a> · {tx_word} {transaction}</p>\
                 <p class=\"why\">{headline}{cite}</p></div>{action}{details}",
                badge = badge(Tone::Bad, "✕", lang.pick("Nicht abgerechnet", "Not billed")),
                identity = escape(&refusal.identity),
                href = escape(&crate::station::href(&refusal.identity)),
                tx_word = lang.pick("Vorgang", "transaction"),
                transaction = escape(&refusal.transaction),
                action = link.to(&id),
                details = why::details(lang, &refusal.findings, &refusal.reasons),
            ),
            id,
            tone: Tone::Bad,
        }
    })
}

fn silent(snapshot: &Snapshot, lang: Lang, link: Links) -> Group {
    let csmsd = match &snapshot.csmsd {
        Source::Up(csmsd) => csmsd,
        Source::Down { since, why } => {
            return Group::Down {
                daemon: "csmsd",
                since: *since,
                why: *why,
            };
        }
    };
    // The most recently silent first, as the daemon sent them.
    let today = snapshot.zone.local(snapshot.now).date;
    let mut group = paged(&csmsd.silent, |station| {
        let since = station.silent_since;
        let id = format!("cp-{}", slug(&station.identity));
        let local = snapshot.zone.local(since);
        let at = if local.date == today {
            clock(local.time)
        } else {
            format!("{} {}", lang.date(local.date), clock(local.time))
        };
        Row {
            body: format!(
                "<div><p class=\"what\">{badge} <a href=\"{href}\"><strong>{identity}</strong></a> · {evse}{network}</p>\
                 <p class=\"why\">{says} <time datetime=\"{iso}\">{at}</time> — {beat}</p></div>{action}",
                badge = badge(Tone::Warn, "◌", lang.pick("Still", "Silent")),
                identity = escape(&station.identity),
                href = escape(&crate::station::href(&station.identity)),
                evse = escape(&station.evse),
                says = lang.pick("Kein Lebenszeichen seit", "Nothing heard since"),
                network = station
                    .carrier
                    .as_deref()
                    .or(station.network.as_deref())
                    .map(|network| format!(" · {}", escape(network)))
                    .unwrap_or_default(),
                iso = escape(&iso(since)),
                beat = match lang {
                    Lang::De => format!("Herzschlag alle {} s erwartet", station.heartbeat_secs),
                    Lang::En => format!("a heartbeat is due every {} s", station.heartbeat_secs),
                },
                action = link.to(&id),
            ),
            id,
            tone: Tone::Warn,
        }
    });
    if let Group::Rows { lead, .. } = &mut group {
        *lead = by_network(&csmsd.networks, lang);
    }
    group
}

/// Silent stations that share a mobile network, said once above the list
/// (spec 022): twelve of forty on one carrier is one outage to report, not
/// twelve stations to visit. A network with a single silent station is noise.
fn by_network(networks: &[crate::model::NetworkShare], lang: Lang) -> String {
    let shared: Vec<String> = networks
        .iter()
        .filter(|share| share.silent >= 2)
        .take(3)
        .map(|share| {
            let name = share.carrier.as_deref().unwrap_or(&share.network);
            format!(
                "<a href=\"/stations?q={network}+offline\"><strong>{name}</strong></a> {count}",
                network = escape(&share.network),
                name = escape(name),
                count = match lang {
                    Lang::De => format!("{} von {} still", share.silent, share.stations),
                    Lang::En => format!("{} of {} silent", share.silent, share.stations),
                },
            )
        })
        .collect();
    if shared.is_empty() {
        return String::new();
    }
    let outage = networks.len() == 1 && networks[0].silent >= 3;
    format!(
        "<p class=\"lead\"><span aria-hidden=\"true\">📶</span> {by}: {list}{hint}</p>",
        by = lang.pick("Nach Mobilfunknetz", "By mobile network"),
        list = shared.join(" · "),
        hint = if outage {
            lang.pick(
                " — alle stillen Stationen in einem Netz: eher ein Netzausfall als ein Defekt.",
                " — every silent station on one network: more likely an outage than a fault.",
            )
        } else {
            ""
        },
    )
}

fn consignments(snapshot: &Snapshot, lang: Lang, link: Links) -> Group {
    let roamd = match &snapshot.roamd {
        Source::Up(roamd) => roamd,
        Source::Down { since, why } => {
            return Group::Down {
                daemon: "roamd",
                since: *since,
                why: *why,
            };
        }
    };
    let page = &roamd.consignments;
    let mut open: Vec<_> = page.items.iter().take(ROWS).collect();
    // On each page, a refusal before the pending: it needs a person; a
    // pending one may only need patience.
    open.sort_by_key(|consignment| consignment.delivery != "refused");
    let rows = open.into_iter().map(|consignment| {
        let id = format!("c-{}", slug(&consignment.key));
        let refused = consignment.delivery == "refused";
        let (tone, icon, word) = if refused {
            (Tone::Bad, "✕", lang.pick("Abgelehnt", "Refused"))
        } else {
            (Tone::Warn, "◷", lang.pick("Ausstehend", "Pending"))
        };
        let said = consignment.reason.as_deref().map_or_else(
            || {
                if refused {
                    lang.pick("ohne Begründung", "with no reason given")
                        .to_owned()
                } else {
                    lang.pick("noch nicht angenommen", "not yet taken")
                        .to_owned()
                }
            },
            ToOwned::to_owned,
        );
        Row {
            body: format!(
                "<div><p class=\"what\">{badge} <strong>{key}</strong> → {recipient}</p>\
                 <p class=\"why\">{said}</p></div>{action}",
                badge = badge(tone, icon, word),
                key = escape(&consignment.key),
                recipient = escape(&consignment.recipient),
                said = escape(&said),
                action = link.to(&id),
            ),
            id,
            tone,
        }
    });
    Group::Rows {
        total: page.total,
        rows: rows.collect(),
        older: page.older,
        lead: String::new(),
    }
}

fn invoices(snapshot: &Snapshot, lang: Lang, link: Links) -> Group {
    let billd = match &snapshot.billd {
        Source::Up(billd) => billd,
        Source::Down { since, why } => {
            return Group::Down {
                daemon: "billd",
                since: *since,
                why: *why,
            };
        }
    };
    let page = &billd.invoices;
    let mut waiting: Vec<_> = page.items.iter().take(ROWS).collect();
    // On each page, a rejection before the waiting.
    waiting.sort_by_key(|document| !matches!(document.submission, Submission::Rejected { .. }));
    let rows = waiting.into_iter().map(|document| {
        let id = format!("i-{}", slug(&document.number));
        let (tone, badge_html, said) = match &document.submission {
            Submission::Rejected { reason } => (
                Tone::Bad,
                badge(Tone::Bad, "✕", lang.pick("Zurückgewiesen", "Rejected")),
                reason.clone(),
            ),
            _ => (
                Tone::Warn,
                badge(Tone::Warn, "◷", lang.pick("Wartet", "Waiting")),
                lang.pick(
                    "Ausgestellt, vom Empfänger noch nicht angenommen.",
                    "Issued, not yet accepted by its recipient.",
                )
                .to_owned(),
            ),
        };
        Row {
            body: format!(
                "<div><p class=\"what\">{badge_html} <strong>{number}</strong> · <span class=\"num\">{total}</span></p>\
                 <p class=\"why\">{said}</p></div>{action}",
                number = escape(&document.number),
                total = escape(&document.total),
                said = escape(&said),
                action = link.to(&id),
            ),
            id,
            tone,
        }
    });
    Group::Rows {
        total: page.total,
        rows: rows.collect(),
        older: page.older,
        lead: String::new(),
    }
}

fn duties(today: time::Date, lang: Lang, link: Links) -> Group {
    let until = today + time::Duration::days(DUTY_HORIZON_DAYS);
    let from = today.previous_day().unwrap_or(today);
    let mut upcoming: Vec<_> = obligation::starting_between(from, until).collect();
    upcoming.sort_by_key(|duty| (duty.applies_from, duty.id.to_string()));
    let total = upcoming.len();
    let rows = upcoming.into_iter().map(|duty| {
        let id = format!("d-{}", slug(&duty.id.to_string()));
        let days = (duty.applies_from - today).whole_days();
        let tone = match days {
            ..=14 => Tone::Bad,
            15..=45 => Tone::Warn,
            _ => Tone::Info,
        };
        let when = match (days, lang) {
            (0, _) => lang.pick("heute", "today").to_owned(),
            (1, _) => lang.pick("morgen", "tomorrow").to_owned(),
            (n, Lang::De) => format!("in {n} Tagen"),
            (n, Lang::En) => format!("in {n} days"),
        };
        let exact = lang.date(duty.applies_from);
        Row {
            body: format!(
                "<div><p class=\"what\"><time class=\"badge due {class}\" datetime=\"{iso}\" \
                 tabindex=\"0\" title=\"{exact}\" data-exact=\"{exact}\">{when}</time> \
                 <strong>{title}</strong> <span class=\"cite\">{cite}</span></p>\
                 <p class=\"why muted\">{scope}</p></div>{action}",
                class = tone.class(),
                iso = duty.applies_from,
                exact = escape(&exact),
                when = escape(&when),
                title = escape(lang.pick(duty.title_de, duty.title)),
                cite = escape(duty.citation),
                scope = scope(lang, duty.scope()),
                action = link.to(&id),
            ),
            id,
            tone,
        }
    });
    bounded(rows, total)
}

const fn scope(lang: Lang, scope: Scope) -> &'static str {
    match scope {
        Scope::ChargePoint => lang.pick("Bindet jeden Ladepunkt", "Binds each Ladepunkt"),
        Scope::MobilityServiceProvider => lang.pick(
            "Bindet den Fahrstromanbieter",
            "Binds the mobility service provider",
        ),
        Scope::Undertaking => lang.pick("Bindet das Unternehmen", "Binds the undertaking"),
    }
}

// ── the footer ──────────────────────────────────────────────────────────────

/// "Journale": where the witness last saw each daemon's journal end, and — in
/// red, until somebody moves the witness file aside — a head that went
/// backwards, forked or came under another key (SR-098).
fn journals(snapshot: &Snapshot, lang: Lang) -> String {
    use emob_service::witness::Verdict;
    let say = |de: String, en: String| if matches!(lang, Lang::De) { de } else { en };
    let mut out = format!(
        "<section id=\"journals\" class=\"card\" aria-labelledby=\"journals-h\"><h2 id=\"journals-h\" class=\"eyebrow\">{}</h2>",
        lang.pick("Journale", "Journals"),
    );
    if snapshot.journals.is_empty() {
        let _ = write!(
            out,
            "<p>{}</p></section>",
            lang.pick(
                "Noch nicht bezeugt — der Zeuge liest die Köpfe gleich.",
                "Not witnessed yet — the witness reads the heads shortly.",
            )
        );
        return out;
    }
    out.push_str("<ul class=\"sources\">");
    for seen in &snapshot.journals {
        let name = escape(&seen.daemon);
        let at = clock(snapshot.zone.local(seen.at).time);
        let key = if seen.key_from_estate {
            lang.pick("Schlüssel aus dem Estate", "key from the estate")
        } else {
            lang.pick(
                "Schlüssel beim ersten Mal gemerkt",
                "key pinned at first sight",
            )
        };
        let line = match &seen.alarm {
            None => format!(
                "{} <b>{name}</b> {} {} · {at} · {key}",
                badge(Tone::Info, "✓", lang.pick("bezeugt", "witnessed")),
                lang.pick("bei", "at"),
                seen.position,
            ),
            Some(Verdict::Regressed { witnessed }) => format!(
                "{} <b>{name}</b> — {}",
                badge(
                    Tone::Bad,
                    "✕",
                    lang.pick("zurückgegangen", "went backwards")
                ),
                say(
                    format!(
                        "bezeugt bei {witnessed}, danach bei {} gesehen: Einträge wurden entfernt oder eine ältere Kopie eingespielt",
                        seen.last
                    ),
                    format!(
                        "witnessed at {witnessed}, then seen at {}: records were removed or an older copy restored",
                        seen.last
                    ),
                ),
            ),
            Some(Verdict::Forked) => format!(
                "{} <b>{name}</b> — {}",
                badge(Tone::Bad, "✕", lang.pick("verzweigt", "forked")),
                say(
                    format!(
                        "bei {} ein anderer Eintrag als bezeugt: die Geschichte wurde umgeschrieben",
                        seen.last
                    ),
                    format!(
                        "another record at {} than the one witnessed: the history was rewritten",
                        seen.last
                    ),
                ),
            ),
            Some(_) => format!(
                "{} <b>{name}</b> — {}",
                badge(
                    Tone::Bad,
                    "✕",
                    lang.pick("anderer Schlüssel", "another key")
                ),
                lang.pick(
                    "der Kopf ist mit einem anderen Schlüssel signiert als dem gemerkten",
                    "the head is signed by another key than the one held",
                ),
            ),
        };
        let _ = write!(out, "<li>{line}</li>");
    }
    out.push_str("</ul></section>");
    out
}

fn footer(snapshot: &Snapshot, lang: Lang) -> String {
    let state = |up: bool| {
        if up {
            format!(
                "<span class=\"badge tone-good\"><span aria-hidden=\"true\">✓</span> {}</span>",
                lang.pick("antwortet", "answering")
            )
        } else {
            format!(
                "<span class=\"badge tone-warn\"><span aria-hidden=\"true\">⚠</span> {}</span>",
                lang.pick("antwortet nicht", "not answering")
            )
        }
    };
    format!(
        "<footer class=\"foot\"><p>{sources}</p><ul class=\"sources\">\
         <li>csmsd {a}</li><li>billd {b}</li><li>roamd {c}</li></ul>\
         <p class=\"noprint\">{refresh}</p></footer>",
        sources = lang.pick("Gelesen aus", "Read from"),
        a = state(snapshot.csmsd.up().is_some()),
        b = state(snapshot.billd.up().is_some()),
        c = state(snapshot.roamd.up().is_some()),
        refresh = lang.pick(
            "Die Seite aktualisiert sich alle 15 Sekunden, solange sie sichtbar ist.",
            "This page refreshes every 15 seconds while it is visible.",
        ),
    )
}

// ── small words ─────────────────────────────────────────────────────────────

/// An anchor-safe form of an identifier: what a stable URL can carry.
fn slug(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn clock(at: time::Time) -> String {
    format!("{:02}:{:02}", at.hour(), at.minute())
}

fn iso(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

const fn weekday(lang: Lang, day: time::Weekday) -> &'static str {
    use time::Weekday as W;
    match day {
        W::Monday => lang.pick("Montag", "Monday"),
        W::Tuesday => lang.pick("Dienstag", "Tuesday"),
        W::Wednesday => lang.pick("Mittwoch", "Wednesday"),
        W::Thursday => lang.pick("Donnerstag", "Thursday"),
        W::Friday => lang.pick("Freitag", "Friday"),
        W::Saturday => lang.pick("Samstag", "Saturday"),
        W::Sunday => lang.pick("Sonntag", "Sunday"),
    }
}

#[cfg(test)]
mod tests;
