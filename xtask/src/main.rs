//! Workspace guards.
//!
//! Checks that are cheap to run and expensive to skip. Each one exists because
//! the failure it catches is silent: nothing crashes, nothing logs, the system
//! is simply wrong in a way that surfaces on an invoice months later.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let root = workspace_root()?;
    match std::env::args().nth(1).as_deref() {
        Some("no-floats") => no_floats(&root),
        Some("check-citations") => check_citations(&root),
        Some("check-manifests") => check_manifests(&root),
        Some("check-graph") => check_graph(&root),
        Some("check-wire") => check_wire(&root),
        Some("check-prose") => check_prose(&root),
        Some("check-concepts") => check_concepts(&root),
        Some("check-reach") => check_reach(&root),
        Some("check-constructors") => check_constructors(&root),
        Some("check-all") => {
            no_floats(&root)?;
            check_citations(&root)?;
            check_manifests(&root)?;
            check_wire(&root)?;
            check_prose(&root)?;
            check_concepts(&root)?;
            check_reach(&root)?;
            check_constructors(&root)?;
            check_graph(&root)
        }
        Some("help" | "--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => {
            print_help();
            bail!("unknown task: {other}")
        }
    }
}

fn print_help() {
    println!(
        "\
cargo xtask <task>

  no-floats         no f32/f64 anywhere in the workspace: every quantity here
                    either is money or becomes money, and a binary float
                    cannot represent 0.10
  check-citations   every regulatory citation in the code *and the docs* names a
                    document that specs/README.md actually indexes
  check-manifests   every publishable crate can be packaged *and accepted*: the
                    files its manifest promises exist, and its keywords and
                    categories are within what the registry takes
  check-graph       no domain crate has a clock, a socket or a database in its
                    dependency graph — the half of `just purity` that greps
                    cannot see, because a dependency is not our source
  check-wire        every date, time and instant in a serialisable type names
                    the spelling it crosses the wire in: `time`'s own derived
                    form is a nine-element array a partner cannot read
  check-prose       no string a user reads carries a lost line continuation:
                    a run of spaces inside a literal is a `\\` somebody
                    dropped, and the sentence reaches an operator with a hole
                    in it
  check-concepts    every count the documentation states about itself is the
                    count something holds: the decision log's own total, the
                    numbered list of rules concepts/README.md says how many of
                    there are, the size of the test suite four documents open
                    by quoting, the count the root README opens by stating,
                    and the per-crate column beside the suite total —
                    because a guard on a sum is not a guard on its terms. Also
                    that every Markdown table row has its header's columns:
                    a surplus cell is dropped and a missing one is blanked,
                    silently, so a sentence moves rows and disappears
  check-reach       every public function in a crate's source is named
                    somewhere other than its own definition: `pub` tells the
                    compiler a downstream may call it, and in this workspace
                    the downstream is this workspace — a rule with no caller
                    is a rule that is not enforced
  check-constructors
                    no type with a fallible `new` derives `Deserialize`: a
                    constructor that states a rule has to be the door the wire
                    comes through, or the rule is enforced everywhere except
                    where the values arrive from
  check-all         all of the above
"
    );
}

/// Every count `concepts/` states about itself is the count it holds.
///
/// # Why a prose document needs a guard at all
///
/// The rest of this workspace keeps its concrete claims in code, where a test
/// can hold them. Two of them are not in code and cannot be: how many decisions
/// the log records, and how many rules the design keeps re-learning. Both are
/// *stated in words* in the document that holds them, and prose has no test —
/// which is the argument D168 made for keeping such claims few, and is exactly
/// why the two that exist drifted.
///
/// They drifted in the way an unchecked claim always does: one-directionally and
/// invisibly. `DECISIONS.md` said "a hundred and seventy-four things" over two
/// hundred and thirteen entries, and `README.md` headed **nine** rules over a
/// list of ten whose ninth and tenth were in the wrong order (D211–D213). No
/// link broke, no reference dangled, and nothing anywhere disagreed — because
/// nothing anywhere was looking.
///
/// So those claims are checked, and the numbering is checked with them: a
/// stable identifier that skips or repeats is a citation from code that quietly
/// means something else.
///
/// # …and a fourth, which is why a guard on a total is not enough
///
/// The size of the test suite joined them (D213), and being derived from
/// `cargo test -- --list` it has been right ever since. The **per-crate column**
/// stating the same suite row by row was not derived from anything, drifted into
/// three counting conventions with two wrong figures in them, and sat on the same
/// page as the guarded figure — where it read as guarded (D225). It is checked
/// now, and so is the completeness of the table, because a crate with no row is a
/// crate no reviewer is looking at. See [`check_test_count`].
///
/// # What runs without `concepts/`, and why it has to
///
/// `concepts/` is **gitignored** — internal design notes, not part of the
/// published repository — so a fresh clone does not have it, and CI runs on a
/// fresh clone. A guard that returns at its first line when the directory is
/// missing is inert in the only place it was ever going to catch anything
/// (D228). The counts it holds are not all in `concepts/`: the test total is
/// stated in the root `README.md` and on two site pages, all three of them
/// tracked, and the table-shape check reads every prose file in the workspace.
///
/// So the checks are split by what they read. The decision log, the rule list
/// and the per-crate column are `concepts/`' own and skip with it, **named** in
/// the summary line rather than in silence; the test counts across the tracked
/// documents and the shape of every table run always. A guard that skips has to
/// say which half it skipped, or it reads exactly like a guard that passed.
fn check_concepts(root: &Path) -> Result<()> {
    let concepts = root.join("concepts");
    let internal = concepts.is_dir();

    let mut problems: Vec<String> = Vec::new();
    let mut decisions_held = 0;
    let mut rules_held = 0;

    if internal {
        // ── The decision log ────────────────────────────────────────────────────
        let decisions = std::fs::read_to_string(concepts.join("DECISIONS.md"))
            .context("reading concepts/DECISIONS.md")?;
        let numbers = decision_numbers(&decisions);
        if numbers.is_empty() {
            bail!("concepts/DECISIONS.md holds no `**D<n> — …**` entries at all");
        }
        let held = numbers.len();
        match stated_count(&decisions) {
            Some(stated) if stated == held => {}
            Some(stated) => problems.push(format!(
                "concepts/DECISIONS.md opens by claiming {stated} entries and holds {held}"
            )),
            None => problems.push(
                "concepts/DECISIONS.md does not open with a spelled-out count of its entries"
                    .to_owned(),
            ),
        }
        // `D` numbers are cited from code comments and from the other documents, so
        // a repeat is two entries answering to one citation and a gap is a citation
        // with nothing behind it.
        //
        // **Not** file order. The log is grouped by topic — "Exactness and
        // representation", "Cryptography and identifiers" — so D3 sits after D27
        // and always has. What has to hold is that the identifiers are unique and
        // the range is dense, which is a property of the set rather than of the
        // sequence; checking the sequence instead reports twelve findings about the
        // document's shape and none about its content.
        let mut sorted = numbers.clone();
        sorted.sort_unstable();
        for pair in sorted.windows(2) {
            if pair[1] == pair[0] {
                problems.push(format!(
                    "concepts/DECISIONS.md records D{} twice: two entries answer to one citation",
                    pair[0]
                ));
            } else if pair[1] != pair[0] + 1 {
                problems.push(format!(
                    "concepts/DECISIONS.md has no D{}..=D{}: a `D` number is a stable identifier \
                 and a gap is a citation with nothing behind it",
                    pair[0] + 1,
                    pair[1] - 1
                ));
            }
        }
        if sorted.first() != Some(&1) {
            problems.push(format!(
                "concepts/DECISIONS.md starts at D{:?} rather than D1",
                sorted.first()
            ));
        }

        // ── The rules the design keeps re-learning ──────────────────────────────
        let readme = std::fs::read_to_string(concepts.join("README.md"))
            .context("reading concepts/README.md")?;
        let (heading, items) = numbered_rules(&readme);
        match (heading, items.len()) {
            (Some(stated), held) if stated == held => {}
            (Some(stated), held) => problems.push(format!(
                "concepts/README.md heads {stated} rules and lists {held}"
            )),
            (None, _) => problems
                .push("concepts/README.md has no '## <spelled-out> rules …' heading".to_owned()),
        }
        for (index, number) in items.iter().enumerate() {
            let expected = index + 1;
            if *number != expected {
                problems.push(format!(
                    "concepts/README.md's rule list reaches {number} where {expected} was due: a \
                 reader counting down the list finds a different rule than a reference to it does"
                ));
            }
        }
        decisions_held = held;
        rules_held = items.len();
    }

    // ── The count the root README opens with ────────────────────────────────
    let properties = check_property_count(root, &mut problems)?;

    // ── The tables, which lose content without failing anything ─────────────
    let tables = check_table_shapes(root, &mut problems)?;

    // ── The suite every document opens by counting ──────────────────────────
    let suite = check_test_count(root, &mut problems, internal)?;

    // ── …and the calendar, which is the same drift one document further in ──
    let duties = check_duty_count(root, &mut problems, internal)?;

    // ── …and the five numbers on the landing page, which is further out still
    check_site_stats(root, &mut problems, duties, suite)?;

    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("❌ {problem}");
        }
        bail!(
            "{} claim{} the documents do not hold",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
    }

    if internal {
        println!(
            "✅ check-concepts: {decisions_held} decisions, {rules_held} rules, {suite} tests, \
             {duties} duties, {properties} properties, {tables} tables, each the number stated"
        );
    } else {
        // Named rather than silent: this is the shape the whole guard had, and
        // it read as a pass (D228).
        println!(
            "✅ check-concepts: {suite} tests, {properties} properties, {tables} tables, each \
             the number stated (concepts/ is absent, so the decision log, the rule list and the \
             per-crate column were not read)"
        );
    }
    Ok(())
}

/// The root README opens by counting the properties it then lists, and nothing
/// held it to the number.
///
/// # Why this one is worth a guard on its own
///
/// It is the first sentence of the first document anybody reads, it is
/// **tracked** — unlike `concepts/`, so it is the count a stranger checks — and
/// it drifts one-directionally, because a property is added by writing a section
/// and a section is written a long way below the heading. It was two out.
///
/// A property is a `### ` heading under the `## The <n> properties …` one. A
/// heading opening with `…` continues the property above it — *"…and the law
/// binds a third subject nobody models"* is the second half of a sentence, not a
/// second property — and is not counted, which is exactly how a reader counts
/// them.
fn check_property_count(root: &Path, problems: &mut Vec<String>) -> Result<usize> {
    let readme = std::fs::read_to_string(root.join("README.md")).context("reading README.md")?;
    let (stated, held) = property_count(&readme);
    match stated {
        Some(stated) if stated == held => {}
        Some(stated) => problems.push(format!(
            "README.md opens by counting {stated} properties and lists {held}: the first sentence \
             of the first document anybody reads"
        )),
        None => problems.push(
            "README.md has no '## The <spelled-out> properties that decide quality' heading"
                .to_owned(),
        ),
    }
    Ok(held)
}

/// The landing page's five headline numbers, against the things that produced
/// them.
///
/// # Why the site needed its own guard
///
/// `check-concepts` holds every *prose* claim: the suite total four documents
/// open by quoting, the decision log's own count, the size of the calendar. The
/// landing page states five numbers and states them in **TOML**, as
/// `stat_tests = "662"` rather than as `**662 tests**` — so every one of those
/// checks walked straight past them, and three of the five were wrong for four
/// milestones: forty duties against forty-seven, six hundred and sixty-two tests
/// against nine hundred, nine crates against twelve.
///
/// They are the first thing a visitor sees, above the fold, before a word of
/// prose. The config's own comment said *"a number here that no longer matches
/// its source is worse than no number"*, and nothing enforced it (D288).
///
/// Two of the five are not checked here and say so: `stat_floats` is `no-floats`'
/// own answer, which is a guard rather than a count, and `stat_curves` is a fact
/// about the `ocmf` crate's pure-Rust backend rather than about this workspace.
fn check_site_stats(
    root: &Path,
    problems: &mut Vec<String>,
    duties: usize,
    tests: usize,
) -> Result<()> {
    let path = root.join("site/config.toml");
    let config = std::fs::read_to_string(&path).context("reading site/config.toml")?;

    // One entry per publishable crate, which is what the landing page counts.
    let crates = std::fs::read_dir(root.join("crates"))
        .context("reading crates/")?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().join("Cargo.toml").is_file())
        .count();

    // Every page's `description` is its meta description, and a search engine
    // truncates one past about 160 characters — so the half a reader is meant to
    // be persuaded by is the half that gets cut. It is also the on-page lead
    // paragraph, where 400 characters is a wall rather than a summary. Nine of
    // the ten were over, one of them at 424 (D288).
    for entry in std::fs::read_dir(root.join("site/content"))?
        .chain(std::fs::read_dir(root.join("site/content/docs"))?)
    {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let Some(description) = front_matter_description(&text) else {
            problems.push(format!(
                "{} states no description, so it inherits the site's and shares a snippet with \
                 every other page",
                path.strip_prefix(root).unwrap_or(&path).display()
            ));
            continue;
        };
        if description.chars().count() > SERP_DESCRIPTION_LIMIT {
            problems.push(format!(
                "{} has a {}-character description: a search engine shows about {}",
                path.strip_prefix(root).unwrap_or(&path).display(),
                description.chars().count(),
                SERP_DESCRIPTION_LIMIT
            ));
        }
    }

    for (key, held) in [
        ("stat_obligations", duties),
        ("stat_tests", tests),
        ("stat_crates", crates),
    ] {
        match stated_stat(&config, key) {
            Some(stated) if stated == held => {}
            Some(stated) => problems.push(format!(
                "site/config.toml states {key} = \"{stated}\" and the workspace holds {held}: it \
                 is the first number a visitor reads"
            )),
            None => problems.push(format!("site/config.toml states no {key}")),
        }
    }
    Ok(())
}

/// What a search engine shows of a meta description before it truncates.
///
/// Google's is closer to 155 on a phone and a little more on a desktop; 160 is
/// the figure every style guide settles on, and the point is that a description
/// written past it has a tail nobody reads.
const SERP_DESCRIPTION_LIMIT: usize = 160;

/// The `description = "…"` of a page's TOML front matter.
///
/// Read from the front matter only — a `description = ` inside the body is prose
/// about the field rather than the field.
fn front_matter_description(page: &str) -> Option<String> {
    let body = page.strip_prefix("+++\n")?;
    let (front, _) = body.split_once("\n+++")?;
    front.lines().find_map(|line| {
        line.trim()
            .strip_prefix("description")?
            .trim_start()
            .strip_prefix('=')
            .map(|rest| rest.trim().trim_matches('"').to_owned())
    })
}

/// The value of a `stat_* = "<n>"` line in the site configuration.
fn stated_stat(config: &str, key: &str) -> Option<usize> {
    config.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(key)?;
        rest.trim_start()
            .strip_prefix('=')?
            .trim()
            .trim_matches('"')
            .parse()
            .ok()
    })
}

/// The obligation calendar's size, against the figure `COMPLIANCE.md` states.
///
/// # Why this is the same guard as the test count and not a new idea
///
/// `check-concepts` exists because prose has no test, and the figure a document
/// opens with is the sentence a reader trusts furthest and nothing checks. The
/// suite total was one such figure (D225). The calendar's is another, and it is
/// the one a *regulator* would read: `COMPLIANCE.md` opens its table with "Forty
/// duties", and adding a duty to `CALENDAR` moved nothing in that sentence.
///
/// Counted off the source rather than off the built binary, for the reason the
/// test count is: this guard is a text check over documents, and running the
/// calendar would make it a dependency of the crate it is auditing.
fn check_duty_count(root: &Path, problems: &mut Vec<String>, internal: bool) -> Result<usize> {
    let calendar = std::fs::read_to_string(root.join("crates/emob-core/src/obligation.rs"))
        .context("reading crates/emob-core/src/obligation.rs")?;
    // Sliced to the `CALENDAR` literal rather than grepped over the file: the
    // test module builds an `Obligation` of its own to exercise `applies_until`
    // with, and counting that would report forty-six duties in a calendar of
    // forty-five — a guard about a number being wrong by one.
    let Some(from) = calendar.find("pub const CALENDAR:") else {
        bail!("crates/emob-core/src/obligation.rs declares no `pub const CALENDAR`");
    };
    let body = &calendar[from..];
    let end = body.find("\n];").unwrap_or(body.len());
    let held = body[..end]
        .lines()
        .filter(|line| line.trim_start().starts_with("id: ObligationId::"))
        .count();
    if held == 0 {
        bail!("crates/emob-core/src/obligation.rs holds no `id: ObligationId::…` calendar entries");
    }

    // Wherever the figure is written it is written **bold**, the convention the
    // test count already uses — and it is checked in Rust doc comments as well
    // as in prose, because the sentence that had drifted was `agentd`'s module
    // documentation rather than a design note. A specialist telling a reader it
    // sweeps forty duties while sweeping forty-four is the same untruth in a
    // place nobody thinks to re-read.
    let mut sources = prose_sources(root)?;
    sources.extend(rust_sources(root)?);
    for file in sources {
        if file.ends_with("xtask/src/main.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        for stated in stated_duty_counts(&text) {
            if stated != held {
                problems.push(format!(
                    "{} states **{stated} duties** and `obligation::CALENDAR` holds {held}",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                ));
            }
        }
    }

    // …and `COMPLIANCE.md` is the document that has to state it at all. A count
    // nobody writes down cannot drift, and a calendar whose own design note
    // does not say how big it is has stopped being a map.
    if internal {
        let compliance = std::fs::read_to_string(root.join("concepts/COMPLIANCE.md"))
            .context("reading concepts/COMPLIANCE.md")?;
        if stated_duty_counts(&compliance).is_empty() {
            problems.push(
                "concepts/COMPLIANCE.md states no **<n> duties** count for the calendar".to_owned(),
            );
        }
    }
    Ok(held)
}

/// Every `**<n> duties**` a document states, spelled in words or in digits.
///
/// Both spellings, because prose here writes "**Forty-four duties**" at the head
/// of a table and "the **44 duties**" inside a sentence, and a guard that read
/// one of them would pass the other by.
fn stated_duty_counts(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("**") {
        rest = &rest[index + 2..];
        let Some(end) = rest.find("**") else { break };
        let phrase = &rest[..end];
        let Some(head) = phrase.strip_suffix(" duties") else {
            continue;
        };
        let head = head.trim();
        if let Ok(count) = head.parse() {
            out.push(count);
        } else if let Some(count) = words_to_number(head) {
            out.push(count);
        }
    }
    out
}

/// The heading's spelled-out count, and the properties the table under it holds.
///
/// # Why it counts rows and no longer sections
///
/// The list used to be twenty-seven `###` sections with their own code — fifteen
/// hundred lines, four fifths of a README nobody scrolls, restating what the
/// documentation site already says at more length and better. It is now an index:
/// one numbered row per property, each linking to the page that explains it, so
/// the front door is scannable and the depth lives in one place (D289).
///
/// The guard is the same guard. A document that states how many of something it
/// holds is one that can be wrong about it, and this one was.
fn property_count(readme: &str) -> (Option<usize>, usize) {
    let Some(start) = readme.find(" properties that decide quality") else {
        return (None, 0);
    };
    let line_start = readme[..start].rfind('\n').map_or(0, |i| i + 1);
    let stated = words_to_number(&readme[line_start..start]);

    let body = &readme[start..];
    let end = body[1..].find("\n## ").map_or(body.len(), |i| i + 1);
    // A numbered row: `| 12 | … | … |`. The header and the delimiter row are not
    // numbered, so nothing else in the table counts.
    let held = body[..end]
        .lines()
        .filter(|line| {
            line.strip_prefix('|')
                .map(str::trim_start)
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .count();
    (stated, held)
}

/// Every row of every Markdown table has the columns its header does.
///
/// # Why a shape is worth a guard
///
/// A row with one cell too many does not fail: Markdown renders the header's
/// number of columns and **drops the rest**, silently. A row with one too few
/// renders a blank. So a cell can move from the row it belongs to into its
/// neighbour and the document still builds, still validates, still looks like a
/// table — and the sentence is gone.
///
/// That is what happened to `DECISIONS.md`'s own summary of passes: the "shape
/// of the error" for D211–D220, a paragraph naming seven separate defects, sat
/// as a fourth cell on the D221–D224 row. The row above it rendered empty and
/// the row below it rendered short. Nothing anywhere disagreed, which is the
/// same sentence [`check_concepts`] opens with.
///
/// A table is a run of lines opening with `|` whose second line is the delimiter
/// row; anything else is prose that happens to start with a pipe. Cells are split
/// on pipes outside code spans and outside `\|`, because a table cell in this
/// workspace routinely carries `` `a|b` ``.
fn check_table_shapes(root: &Path, problems: &mut Vec<String>) -> Result<usize> {
    let mut tables = 0;
    for file in prose_sources(root)? {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let shown = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();

        let lines: Vec<&str> = text.lines().collect();
        let mut fenced = false;
        let mut index = 0;
        while index < lines.len() {
            if lines[index].trim_start().starts_with("```") {
                fenced = !fenced;
                index += 1;
                continue;
            }
            if fenced || !lines[index].trim_start().starts_with('|') {
                index += 1;
                continue;
            }
            // A header, then a delimiter row, then the body.
            let width = table_cells(lines[index]).len();
            let is_delimiter = lines.get(index + 1).is_some_and(|line| {
                let trimmed = line.trim();
                trimmed.starts_with('|')
                    && trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
            });
            if !is_delimiter {
                index += 1;
                continue;
            }
            tables += 1;
            let mut row = index + 2;
            while row < lines.len() && lines[row].trim_start().starts_with('|') {
                let held = table_cells(lines[row]).len();
                if held != width {
                    problems.push(format!(
                        "{shown}:{}: this table row has {held} cell(s) and its header has \
                         {width}: Markdown drops the surplus and blanks the shortfall, so the \
                         difference is a sentence nobody will see",
                        row + 1
                    ));
                }
                row += 1;
            }
            index = row;
        }
    }
    Ok(tables)
}

/// One table row's cells.
///
/// The outer pipes delimit rather than separate, so they contribute no cell. A
/// pipe inside a code span or escaped as `\|` is content.
fn table_cells(line: &str) -> Vec<&str> {
    let line = line.trim();
    let mut cells = Vec::new();
    let mut start = 0;
    let mut in_code = false;
    let mut escaped = false;
    for (at, ch) in line.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            // A backslash escapes the next character — but **not** inside a code
            // span, where Markdown takes it literally. Honouring it there
            // swallows the closing backtick of `` `\` ``, leaves the parser
            // inside a code span for the rest of the line, and reports the row
            // as short: a guard finding its own false positive on the first row
            // that mentioned an escape.
            '\\' if !in_code => escaped = true,
            '`' => in_code = !in_code,
            '|' if !in_code => {
                cells.push(&line[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    cells.push(&line[start..]);
    if cells.first().is_some_and(|c| c.trim().is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|c| c.trim().is_empty()) {
        cells.pop();
    }
    cells
}

/// Every count a prose file in this workspace states about the test suite is a
/// count the suite holds — the workspace total, and the per-package column
/// beside it.
///
/// The total appears in four documents — the root README, `concepts/OVERVIEW.md`
/// and two site pages — and it is the first figure a reader checks, because it
/// is the one that says whether any of the rest is real.
///
/// # The total was guarded and the terms were not
///
/// D213 put this guard on `**N tests**` and it has been green ever since. The
/// column beside it in `OVERVIEW.md` states the same suite crate by crate, was
/// governed by nothing, and had drifted in two directions at once: three
/// counting conventions, and two figures simply wrong. The total stayed right
/// through all of it, because a total that comes from `cargo test -- --list` is
/// not the sum of anything a human typed. **A guard on the sum is not a guard on
/// the terms** (D225).
///
/// So the column is checked too, one convention: the tests in a package's own
/// targets, doc tests excluded — those are in the total and in no row, because a
/// row is about a crate and a doc test is about a sentence. Every workspace
/// member has to have a row, so a new crate cannot arrive unstated.
///
/// # One invocation
///
/// `cargo test -- --list` enumerates without running. `just ci` runs `test`
/// before `guards`, so the binaries this asks are already built and the guard
/// costs a few hundred milliseconds; on its own it pays for a compile, which is
/// the honest price of a claim about a test suite.
///
/// Attributing a test to a package needs the `Running …` lines, and those go to
/// **stderr** while the test names go to stdout. Captured as two pipes they
/// cannot be interleaved, so the command is run through a shell that merges them
/// into one: `cargo` prints `Running X`, waits for `X` to exit, then prints the
/// next, so a single stream is in order by construction. Running `cargo test -p`
/// per package instead would be seventeen invocations *and* seventeen different
/// feature unifications, which is a slower way to answer a different question.
fn check_test_count(root: &Path, problems: &mut Vec<String>, internal: bool) -> Result<usize> {
    let suite = enumerate_suite(root)?;

    for file in prose_sources(root)? {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        for stated in stated_test_counts(&text) {
            if stated != suite.total {
                problems.push(format!(
                    "{} states **{stated} tests** and the workspace holds {}",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                    suite.total
                ));
            }
        }
    }

    // The per-crate column lives in `concepts/OVERVIEW.md`, which a clone does
    // not have. The total above does not: it is stated in tracked documents and
    // is checked either way.
    if internal {
        check_package_counts(root, &suite, problems)?;
    }
    Ok(suite.total)
}

/// The workspace's tests, as the total four documents quote and as the figure
/// each package's row in `concepts/OVERVIEW.md` states.
struct Suite {
    /// Every listed test, doc tests included.
    total: usize,
    /// One entry per workspace member, in manifest order, doc tests excluded.
    per_package: Vec<(String, usize)>,
}

/// Enumerate the suite once and attribute every test to the package that owns
/// it.
fn enumerate_suite(root: &Path) -> Result<Suite> {
    let members = workspace_members(root)?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = std::process::Command::new("sh")
        .current_dir(root)
        .arg("-c")
        // The merge is the point — see the function documentation above.
        .arg(format!(
            "{cargo} test --workspace --all-features -- --list 2>&1"
        ))
        .output()
        .context("running `cargo test -- --list`")?;
    let listing = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        bail!("`cargo test -- --list` failed:\n{listing}");
    }

    let mut per_package: Vec<(String, usize)> =
        members.iter().map(|m| (m.name.clone(), 0)).collect();
    let mut total = 0;
    // `None` while the listing is inside a doc-test section, which belongs to no
    // row, or before the first `Running` line.
    let mut current: Option<usize> = None;

    for line in listing.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            let target = running_target(rest).with_context(|| {
                format!("reading the test binary out of `cargo test`'s line: {line}")
            })?;
            current = Some(owner_of(&target, &members).with_context(|| {
                format!("attributing the test target `{target}` to a workspace member")
            })?);
        } else if trimmed.starts_with("Doc-tests ") {
            current = None;
        } else if line.ends_with(": test") {
            // One line per test. Benchmarks end in `: bench`.
            total += 1;
            if let Some(index) = current {
                per_package[index].1 += 1;
            }
        }
    }

    Ok(Suite { total, per_package })
}

/// The test target's name, out of `unittests src/lib.rs (target/…/name-hash)`.
fn running_target(rest: &str) -> Option<String> {
    let path = rest.rsplit_once('(')?.1.trim_end_matches(')');
    let file = path.rsplit('/').next()?;
    // `<target>-<hash>`, and a target name may itself contain `-`.
    Some(file.rsplit_once('-')?.0.to_owned())
}

/// The workspace member a test target belongs to, as an index into `members`.
///
/// A `lib`/`bin` target is named for its package, with `-` spelled `_`; an
/// integration target is named for its file under the package's `tests/`. A
/// target neither rule places is an error rather than a silent zero, because a
/// guard that undercounts reports every row as wrong or none.
fn owner_of(target: &str, members: &[Member]) -> Option<usize> {
    let as_package = target.replace('_', "-");
    members
        .iter()
        .position(|m| m.name == as_package)
        .or_else(|| {
            members
                .iter()
                .position(|m| m.dir.join("tests").join(format!("{target}.rs")).exists())
        })
}

/// A workspace member: the name `cargo` knows it by, and where its manifest is.
struct Member {
    name: String,
    dir: PathBuf,
}

/// The `[workspace] members` list, in the order the manifest states it.
///
/// The directory's last segment is the package name throughout this workspace,
/// and `check-manifests` is what would notice if that stopped being true.
fn workspace_members(root: &Path) -> Result<Vec<Member>> {
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).context("reading Cargo.toml")?;
    let list = manifest
        .split_once("members")
        .and_then(|(_, rest)| rest.split_once('['))
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(list, _)| list)
        .context("Cargo.toml has no `[workspace] members` list")?;

    let members: Vec<Member> = list
        .split(',')
        .filter_map(|entry| {
            let path = entry.trim().trim_matches('"');
            if path.is_empty() {
                return None;
            }
            Some(Member {
                name: path.rsplit('/').next()?.to_owned(),
                dir: root.join(path),
            })
        })
        .collect();
    if members.is_empty() {
        bail!("Cargo.toml's `members` list is empty");
    }
    Ok(members)
}

/// Every workspace member has a row in `concepts/OVERVIEW.md`, and the row opens
/// with the number of tests that member holds.
///
/// The row's verdict cell reads `✅ <n> tests …`, and only that leading figure is
/// checked: the prose after it says how the tests divide, which is worth reading
/// and is not a claim a guard can hold.
fn check_package_counts(root: &Path, suite: &Suite, problems: &mut Vec<String>) -> Result<()> {
    let path = root.join("concepts/OVERVIEW.md");
    let text = std::fs::read_to_string(&path).context("reading concepts/OVERVIEW.md")?;

    let mut seen: Vec<&str> = Vec::new();
    for line in text.lines() {
        if !line.starts_with("| ") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').collect();
        if cells.len() < 3 {
            continue;
        }
        let Some(name) = backticked(cells[0]) else {
            continue;
        };
        let Some((_, held)) = suite.per_package.iter().find(|(pkg, _)| pkg == name) else {
            continue;
        };
        seen.push(name);

        let verdict = cells[cells.len() - 1].trim();
        match verdict.strip_prefix("✅ ").and_then(stated_row_count) {
            Some(stated) if stated == *held => {}
            Some(stated) => problems.push(format!(
                "concepts/OVERVIEW.md gives `{name}` {stated} tests and it holds {held}"
            )),
            None => problems.push(format!(
                "concepts/OVERVIEW.md's row for `{name}` does not open `✅ <n> tests`, so the \
                 figure beside the guarded total is a claim nothing holds"
            )),
        }
    }

    for (name, held) in &suite.per_package {
        if !seen.contains(&name.as_str()) {
            problems.push(format!(
                "concepts/OVERVIEW.md has no row for `{name}`, which holds {held} tests: a crate \
                 the map does not carry is one nobody reviews"
            ));
        }
    }
    Ok(())
}

/// The first backticked token in a table cell — the crate a row is about.
fn backticked(cell: &str) -> Option<&str> {
    let (_, rest) = cell.split_once('`')?;
    rest.split_once('`').map(|(name, _)| name)
}

/// `123 tests …` as `123`, and anything else as `None`.
fn stated_row_count(verdict: &str) -> Option<usize> {
    let digits: String = verdict.chars().take_while(char::is_ascii_digit).collect();
    verdict[digits.len()..]
        .starts_with(" tests")
        .then(|| digits.parse().ok())
        .flatten()
}

/// Every `**N tests**` a document states, in digits.
fn stated_test_counts(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("**") {
        rest = &rest[index + 2..];
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ' ' || *c == '\u{202f}')
            .filter(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            continue;
        }
        let after = rest
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_digit() || *c == ' ' || *c == '\u{202f}'))
            .map_or("", |(i, _)| &rest[i..]);
        if after.starts_with("tests**")
            && let Ok(count) = digits.parse()
        {
            out.push(count);
        }
    }
    out
}

/// The `D` numbers of the log's entries, in the order they appear.
///
/// An entry opens a line as `**D<n> — `. A *section* intro opens as
/// `**D<n>–D<m> came from …**` and is not one, which is why the em dash after
/// the number is part of the pattern.
fn decision_numbers(text: &str) -> Vec<usize> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("**D")?;
            let end = rest.find(|c: char| !c.is_ascii_digit())?;
            if !rest[end..].starts_with(" — ") {
                return None;
            }
            rest[..end].parse().ok()
        })
        .collect()
}

/// The count a document opens by claiming, written out in words.
///
/// Spelled out rather than in digits because that is how the document reads, and
/// a guard that only understood `213` would be satisfied by prose nobody writes.
fn stated_count(text: &str) -> Option<usize> {
    // The opening sentence, which is the only place the total is claimed.
    let head: String = text.lines().take(12).collect::<Vec<_>>().join(" ");
    words_to_number(&head)
}

/// The heading's spelled-out count, and the numbers the list under it actually
/// reaches.
fn numbered_rules(text: &str) -> (Option<usize>, Vec<usize>) {
    let Some(start) = text.find(" rules this design keeps re-learning") else {
        return (None, Vec::new());
    };
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let heading = words_to_number(&text[line_start..start]);

    let body = &text[start..];
    let end = body[1..].find("\n## ").map_or(body.len(), |i| i + 1);
    let items = body[..end]
        .lines()
        .filter_map(|line| {
            let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() || !line[digits.len()..].starts_with(". **") {
                return None;
            }
            digits.parse().ok()
        })
        .collect();
    (heading, items)
}

/// A count written in English words, up to the low thousands — enough for a
/// decision log and a list of rules, and deliberately not a general parser.
fn words_to_number(text: &str) -> Option<usize> {
    const UNITS: [(&str, usize); 21] = [
        ("zero", 0),
        ("one", 1),
        ("two", 2),
        ("three", 3),
        ("four", 4),
        ("five", 5),
        ("six", 6),
        ("seven", 7),
        ("eight", 8),
        ("nine", 9),
        ("ten", 10),
        ("eleven", 11),
        ("twelve", 12),
        ("thirteen", 13),
        ("fourteen", 14),
        ("fifteen", 15),
        ("sixteen", 16),
        ("seventeen", 17),
        ("eighteen", 18),
        ("nineteen", 19),
        ("twenty", 20),
    ];
    const TENS: [(&str, usize); 8] = [
        ("twenty", 20),
        ("thirty", 30),
        ("forty", 40),
        ("fifty", 50),
        ("sixty", 60),
        ("seventy", 70),
        ("eighty", 80),
        ("ninety", 90),
    ];

    let lower = text.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|t| !t.is_empty())
        .collect();

    let mut total = 0usize;
    let mut current = 0usize;
    let mut seen = false;
    for token in tokens {
        let word = token.trim_end_matches('s');
        if word == "hundred" || word == "thousand" {
            // English writes "a hundred and seventy-four", so the article is
            // the multiplier. Without this the commonest spelling of the very
            // claim being checked parses as "no number here at all", and the
            // guard reports the wrong reason for a real finding.
            let scale = if word == "hundred" { 100 } else { 1000 };
            current = current.max(1) * scale;
            seen = true;
            continue;
        }
        if word == "and" || word == "a" || word == "an" {
            continue;
        }
        if let Some((_, tens)) = TENS.iter().find(|(w, _)| *w == word) {
            current += tens;
            seen = true;
            continue;
        }
        if let Some((_, unit)) = UNITS.iter().find(|(w, _)| *w == word) {
            current += unit;
            seen = true;
            continue;
        }
        // Hyphenated forms — "seventy-four" — arrive as two tokens above; any
        // other word ends the number.
        if seen {
            total += current;
            return Some(total);
        }
    }
    seen.then_some(total + current)
}

/// Every date, time and instant in a serialisable type pins the spelling it
/// crosses the wire in.
///
/// # Why this is a guard and not a review note
///
/// `time`'s derived `Serialize` writes its internal representation: an
/// `OffsetDateTime` becomes a nine-element array, a `Date` becomes a year and an
/// *ordinal day*. All of it round-trips through this codebase perfectly, which
/// is exactly why nothing notices — `from_str(to_string(x)) == x` holds for any
/// encoding, including one no partner can read (D85).
///
/// That was found once by reading, fixed across the workspace, and **came back
/// on the next timestamp added to a serialisable type** — `EvidenceRef`'s list
/// of signed tariff-change instants, which went out as a list of arrays for
/// exactly as long as it took to write this check (D209). A defect that returns
/// the moment attention moves is a defect that needs a build failure rather than
/// a convention.
///
/// The rule: a field whose type mentions a `time` type, inside a struct that
/// derives `Serialize`, must carry `#[serde(with = …)]`. What that module is —
/// `time::serde::rfc3339`, `emob_core::wire::date`, one of this workspace's own
/// — is the author's call; *saying* is not.
fn check_wire(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let mut checked = 0usize;

    for file in rust_sources(root)? {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        // Everything from the first `#[cfg(test)]` is a fixture, and a fixture
        // does not cross a wire.
        let body = source
            .split_once("#[cfg(test)]")
            .map_or(source.as_str(), |(before, _)| before);

        for (name, attrs, fields) in serialisable_structs(body) {
            for (field, ty, field_attrs) in struct_fields(&fields) {
                if !mentions_a_time_type(&ty) || field_attrs.contains("serde(with") {
                    continue;
                }
                checked += 1;
                problems.push(format!("  {}: {name}.{field}: {ty}", file.display()));
            }
            let _ = attrs;
        }
    }

    if !problems.is_empty() {
        bail!(
            "these fields cross a wire in `time`'s own derived form, which is a nine-element \
             array no partner can read — name the spelling with `#[serde(with = …)]`:\n{}",
            problems.join("\n")
        );
    }
    let _ = checked;
    println!(
        "🕰️  check-wire: every date and instant in a serialisable type names its wire spelling"
    );
    Ok(())
}

/// Struct declarations that derive `Serialize`, as `(name, attributes, body)`.
fn serialisable_structs(source: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (index, _) in source.match_indices("struct ") {
        // The attributes are whatever precedes the declaration back to the
        // previous blank line — enough to see a `derive` or a `cfg_attr`.
        let head = &source[..index];
        let attrs_start = head.rfind("\n\n").map_or(0, |at| at + 2);
        let attrs = &head[attrs_start..];
        if !attrs.contains("Serialize") {
            continue;
        }
        let rest = &source[index + "struct ".len()..];
        let Some(brace) = rest.find('{') else {
            continue;
        };
        let name = rest[..brace].trim().to_owned();
        // A tuple struct or a generic bound is not a field list this reads.
        if name.contains('(') || name.is_empty() {
            continue;
        }
        let Some(end) = rest[brace..].find("\n}") else {
            continue;
        };
        out.push((
            name,
            attrs.to_owned(),
            rest[brace + 1..brace + end].to_owned(),
        ));
    }
    out
}

/// The public fields of a struct body, as `(name, type, its attributes)`.
fn struct_fields(body: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut attrs = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[") {
            attrs.push_str(trimmed);
            continue;
        }
        if trimmed.starts_with("///") || trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("pub ")
            && let Some((name, ty)) = rest.split_once(':')
            && !name.contains('(')
        {
            out.push((
                name.trim().to_owned(),
                ty.trim().trim_end_matches(',').to_owned(),
                std::mem::take(&mut attrs),
            ));
            continue;
        }
        attrs.clear();
    }
    out
}

/// Whether a field's type mentions something `time` serialises structurally.
fn mentions_a_time_type(ty: &str) -> bool {
    [
        "OffsetDateTime",
        "time::Date",
        "time::Time",
        "time::Duration",
        "Weekday",
    ]
    .iter()
    .any(|needle| ty.contains(needle))
}

fn workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("no workspace root above the current directory");
        }
    }
}

/// Every `.rs` file under `crates/`, `services/` and `xtask/`.
fn rust_sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for top in ["crates", "services", "xtask"] {
        collect(&root.join(top), "rs", &mut files)?;
    }
    files.sort();
    Ok(files)
}

/// Every prose file that makes regulatory claims: the crate and service
/// READMEs, the site, and the architecture notes when they are present.
///
/// A citation in a document is the same promise as a citation in a comment —
/// "this rule comes from that paragraph of that text" — and a reader who cannot
/// follow it is in exactly the position the guard exists to prevent. `concepts/`
/// and `specs/` are gitignored, so this finds them on a working copy and skips
/// them on a fresh clone.
fn prose_sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let readme = root.join("README.md");
    if readme.exists() {
        files.push(readme);
    }
    for top in ["crates", "services", "site/content", "concepts"] {
        collect(&root.join(top), "md", &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect(&path, extension, out)?;
        } else if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        }
    }
    Ok(())
}

/// Strip a line comment, so a `//` mentioning `f64` is prose rather than code.
fn code_part(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("*") {
        return "";
    }
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

// ── no-floats ───────────────────────────────────────────────────────────────

/// Every energy and money value in this workspace either *is* money or
/// *becomes* money. OCMF defines a session's energy as a subtraction of two
/// register readings, and in `f64` `10.1 - 0.1` is `10.000000000000002`.
///
/// The failure it prevents: a workspace that is exact everywhere it was
/// reviewed and approximate in the one helper nobody looked at, reconciling
/// against nothing and losing the dispute.
///
/// Two shapes are checked, because one of them a token search cannot see:
/// the tokens `f32`/`f64` themselves, and **`Decimal::try_from`**, which
/// accepts an `f64` and therefore launders a dependency's float field into an
/// exact type without the word appearing anywhere here.
fn no_floats(root: &Path) -> Result<()> {
    let mut offenders = Vec::new();

    for file in rust_sources(root)? {
        // The guard itself has to name what it forbids.
        if file.ends_with("xtask/src/main.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file)?;
        let mut in_test_module = false;
        let mut test_depth: i32 = 0;
        let mut depth: i32 = 0;

        for (n, line) in text.lines().enumerate() {
            let code = code_part(line);

            // Tests may use floats freely: they are allowed to construct the
            // very values the domain refuses, in order to prove it refuses them.
            if code.contains("#[cfg(test)]") {
                in_test_module = true;
                test_depth = depth;
            }
            depth += code.matches('{').count() as i32;
            depth -= code.matches('}').count() as i32;
            if in_test_module && depth <= test_depth {
                in_test_module = false;
            }
            if in_test_module {
                continue;
            }

            for needle in ["f32", "f64"] {
                if let Some(col) = find_type_token(code, needle) {
                    offenders.push(format!(
                        "{}:{}:{} — {}",
                        file.strip_prefix(root).unwrap_or(&file).display(),
                        n + 1,
                        col + 1,
                        line.trim()
                    ));
                }
            }

            // The hole a token search cannot see. `Decimal::try_from` accepts an
            // `f64`, so a value laundered out of a dependency's float field —
            // `Decimal::try_from(event.meter_wh?)` — reaches an exact type
            // without the word `f64` appearing anywhere in this workspace. The
            // guard would pass and the invoice would be wrong.
            //
            // Every other `Decimal::try_from` conversion has an infallible
            // spelling (`Decimal::from` for the integers, `from_str_exact` for
            // text), so refusing the whole name costs nothing and closes the
            // one path a token search misses.
            if let Some(col) = code.find("Decimal::try_from") {
                offenders.push(format!(
                    "{}:{}:{} — {} (use Decimal::from or from_str_exact: try_from accepts an f64)",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                    n + 1,
                    col + 1,
                    line.trim()
                ));
            }
        }
    }

    if !offenders.is_empty() {
        eprintln!("❌ binary floats found — every quantity here becomes money:");
        for o in &offenders {
            eprintln!("   {o}");
        }
        bail!("{} float(s) in the workspace", offenders.len());
    }
    println!("💶 no-floats: no f32/f64 outside tests — every quantity is exact");
    Ok(())
}

/// Find `f32`/`f64` where it means a binary float.
///
/// A match counts when it is not glued to an identifier: `let x: f64`,
/// `(f64, f64)` — and `Decimal::from_f64(x)`, deliberately. That last one is
/// the boundary conversion that matters most: turning an `f64` into a `Decimal`
/// preserves whatever error the float already had, so the exactness downstream
/// is a formality. An underscore before the match is therefore *not* an escape
/// hatch; only a letter or digit is, so `sf64x` and `my_f64_helper` stay quiet.
fn find_type_token(line: &str, needle: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find(needle) {
        let at = from + rel;
        let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let after = at + needle.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The bracketed phrases in a line that look like regulatory citations.
///
/// A citation here is `[` … `]` with no nesting, starting with an upper-case
/// letter or a digit, containing one of [`CITATION_MARKERS`], and holding no
/// backtick — which is what separates `[MessEG §33]` from a Rust doc link like
/// ``[`Self::foo`]`` and from a markdown reference.
fn citation_phrases(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let phrase = &after[..close];
        rest = &after[close + 1..];

        let starts_right = phrase
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let looks_cited = CITATION_MARKERS.iter().any(|m| phrase.contains(m));
        if starts_right && looks_cited && !phrase.contains('`') && !phrase.contains('[') {
            found.push(phrase.to_owned());
        }
    }
    found
}

// ── check-citations ─────────────────────────────────────────────────────────

/// Which document each citation prefix belongs to, and a string that must
/// appear in `specs/README.md` for that document to count as indexed.
///
/// A family is added here only once the document is actually indexed. An entry
/// whose needle is broad enough to match anything is worse than no entry: it
/// reports a citation as checked when nothing checked it.
const CITATION_SOURCES: &[(&str, &str, &str)] = &[
    ("[AFIR ", "Regulation (EU) 2023/1804", "afir-2023-1804"),
    (
        "[DA-656",
        "Delegated Regulation (EU) 2025/656",
        "afir-da-2025-656",
    ),
    ("[LSV26", "Ladesäulenverordnung 2026", "lsv-2026"),
    (
        "[DATEX-II-Profil",
        "the Mobilithek AFIR DATEX II Recharging profile",
        "AFIR-DATEX-II-Recharging-Profil",
    ),
    ("[MessEG ", "MessEG", "messeg.pdf"),
    ("[MessEV", "MessEV", "messev.pdf"),
    ("[PTB-A ", "PTB-A 50.7", "ptb-a-50.7"),
    ("[REA ", "REA Dokument 6-A", "rea-dokument-6-a"),
    ("[38k", "38. BImSchV", "bimschv-38"),
    ("[UStG ", "UStG", "ustg.pdf"),
    ("[PAngV", "PAngV", "pangv.pdf"),
    ("[OCMF ", "OCMF", "ocmf-master.zip"),
    (
        "[OCA SMV",
        "the OCA application note on signed meter values",
        "oca-signed-meter-values",
    ),
    ("[NIS2", "NIS2", "nis2-2022-2555"),
    ("[CRA", "Cyber Resilience Act", "cra-2024-2847"),
    // The NZR-EMob corpus lives in the sibling `mako` workspace, which
    // specs/README.md points at rather than duplicating.
    ("[A6 ", "BK6-20-160 Anlage 6", "mako/regulatories"),
    ("[M2 ", "BDEW AWH „Zum Modell 2\"", "mako/regulatories"),
    // The protocol corpora live in the sibling kits, which specs/README.md
    // points at rather than duplicating.
    ("[OCPI ", "the OCPI specifications", "ocpi-kit/specs"),
    ("[OICP ", "the OICP specifications", "oicp-kit/specs"),
    ("[OCPP ", "the OCPP specifications", "ocpp-kit/specs"),
    ("[BGB ", "Bürgerliches Gesetzbuch", "bgb.pdf"),
    // § 14a's dimmable consumer devices are the reason a charge point can be
    // held at zero by its operator, which is a fact about *money* here and a
    // fact about the grid there. The statute lives in the sibling `hems`
    // workspace, which specs/README.md points at rather than duplicating.
    (
        "[EnWG ",
        "Energiewirtschaftsgesetz",
        "hems/specs/law/enwg.pdf",
    ),
];

/// The markers that make a bracketed phrase a citation rather than a doc link,
/// an array type or a markdown reference.
///
/// Deliberately narrow. A citation this misses is one the prefix table still
/// checks; a false positive here would fail the build over a Rust doc link,
/// which is how a guard gets switched off.
const CITATION_MARKERS: &[&str] = &["§", "Art. ", "Tab. ", "Anh. "];

/// Every regulatory claim in emob cites its source in the form `[AFIR Art. 5(1)]`
/// or `[OCMF Tab. 7]`. This checks that the documents those refer to are indexed
/// in `specs/README.md`, so a citation can always be followed to a file and a
/// retrieval URL — in the **documentation** as well as the code, because a
/// README that cites a paragraph is making the same promise a comment does and
/// is read by more people.
///
/// The failure it prevents: a rule that cites a Verordnung nobody can produce,
/// which is indistinguishable from a rule somebody invented.
fn check_citations(root: &Path) -> Result<()> {
    // `specs/` is gitignored — the documents are third-party and copyrighted —
    // so a clone has no index, and CI builds a clone. Returning here was the
    // whole guard skipping in the one environment that gates a merge (D228).
    //
    // Only *half* of it needs the index. "Does `specs/README.md` list this
    // document" cannot be asked without the file; "is this citation a form this
    // workspace recognises at all" is asked of `CITATION_SOURCES`, a table
    // compiled into this binary — and that is the half D65 added, because a
    // citation whose prefix is unknown used to be checked by nothing and report
    // success. So the second half runs everywhere and the first says when it did
    // not run.
    let index_path = root.join("specs/README.md");
    let index = match std::fs::read_to_string(&index_path) {
        Ok(index) => Some(index),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("reading specs/README.md"),
    };

    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut missing = Vec::new();
    let mut unknown = Vec::new();

    // Code and prose alike: a citation in a README is the same promise as one
    // in a comment, and a site page nobody can follow to a document is the
    // failure this guard is named after.
    let mut sources = rust_sources(root)?;
    sources.extend(prose_sources(root)?);
    let mut scanned = 0;

    for file in sources {
        if file.ends_with("xtask/src/main.rs") || file.ends_with("specs/README.md") {
            continue;
        }
        scanned += 1;
        let text = std::fs::read_to_string(&file)?;
        for (n, line) in text.lines().enumerate() {
            for (prefix, document, needle) in CITATION_SOURCES {
                if line.contains(prefix) {
                    seen.insert(needle);
                    if index.as_ref().is_some_and(|index| !index.contains(needle)) {
                        missing.push(format!(
                            "{}:{} cites {document}, which specs/README.md does not index (looking for {needle:?})",
                            file.strip_prefix(root).unwrap_or(&file).display(),
                            n + 1,
                        ));
                    }
                }
            }

            // …and the other half of the promise. Until now a citation whose
            // prefix was not in the table above was not checked *at all*: the
            // guard reported success because it had nothing to say, which is
            // the failure it exists to prevent, wearing its own uniform.
            for phrase in citation_phrases(line) {
                if !CITATION_SOURCES
                    .iter()
                    .any(|(prefix, _, _)| phrase.starts_with(prefix.trim_start_matches('[')))
                {
                    unknown.push(format!(
                        "{}:{} cites [{phrase}], whose document is not in the source table",
                        file.strip_prefix(root).unwrap_or(&file).display(),
                        n + 1,
                    ));
                }
            }
        }
    }

    if !missing.is_empty() {
        eprintln!("❌ citations without an indexed source:");
        for m in &missing {
            eprintln!("   {m}");
        }
        bail!("{} unindexed citation(s)", missing.len());
    }
    if !unknown.is_empty() {
        unknown.sort();
        unknown.dedup();
        eprintln!("❌ citations to documents this guard does not know:");
        for u in &unknown {
            eprintln!("   {u}");
        }
        eprintln!(
            "   add the document to specs/README.md and its prefix to CITATION_SOURCES, or the \
             citation is a claim nobody can follow"
        );
        bail!("{} unrecognised citation(s)", unknown.len());
    }
    match index {
        Some(_) => println!(
            "📚 check-citations: {} document families cited across {scanned} files, every one \
             recognised and indexed in specs/README.md",
            seen.len()
        ),
        // Named rather than silent, for the reason `check_concepts` says at
        // length: a guard that skips without saying so reads as one that passed.
        None => println!(
            "📚 check-citations: {} document families cited across {scanned} files, every one a \
             form this guard recognises (specs/README.md is absent, so whether each is *indexed* \
             was not asked)",
            seen.len()
        ),
    }
    Ok(())
}

// ── check-manifests ─────────────────────────────────────────────────────────

/// What crates.io accepts. Enforced server-side, on upload, one crate at a time.
const MAX_KEYWORDS: usize = 5;
const MAX_KEYWORD_CHARS: usize = 20;
const MAX_CATEGORIES: usize = 5;

/// The string items of a TOML array field, whether it is written on one line or
/// several. Deliberately not a TOML parse: this guard reads the manifest as text
/// so that it keeps working on a field `cargo metadata` does not surface.
fn array_field(text: &str, field: &str) -> Option<Vec<String>> {
    let start = text
        .lines()
        .position(|l| l.trim_start().starts_with(field) && l.contains('='))?;
    let mut body = String::new();
    for line in text.lines().skip(start) {
        body.push_str(line);
        body.push('\n');
        if line.contains(']') {
            break;
        }
    }
    let inner = body.split_once('[')?.1;
    let inner = inner.rsplit_once(']')?.0;
    Some(
        inner
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_owned())
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

/// `cargo publish` cannot be undone, and it fails on a `readme` that is not
/// there — after the version has already been consumed on the registry.
/// The crates that promise to read no clock, open no socket and touch no
/// filesystem — the same list `just purity` greps.
const PURE_CRATES: [&str; 11] = [
    "emob-core",
    "emob-eichrecht",
    "emob-session",
    "emob-cdr",
    "emob-billing",
    "emob-tariff",
    "emob-thg",
    "emob-ocpp",
    "emob-poi",
    "emob-roam",
    "emob-sim",
];

/// Crates whose presence in a graph means an ambient-state path was activated.
///
/// Each entry is here because reaching it is the *only* thing it is for: an
/// async runtime drives sockets, a driver talks to a database, a v7 or v4
/// identifier reads the clock or the OS random source, and a system time-zone
/// lookup reads `TZ` and `/etc/localtime`. None of them is a crate a domain
/// crate can hold and still be replayable.
///
/// Deliberately not a list of everything that *could* do I/O. `libc` is in every
/// graph, `time` can format an instant it was handed, and `rand_core` is a trait
/// crate the elliptic-curve stack needs to *verify* a signature it was given.
/// A guard that failed on those would be turned off within a week, which is the
/// failure mode of every over-broad check.
const AMBIENT: [(&str, &str); 16] = [
    (
        "tokio",
        "an async runtime, which exists to drive sockets and files",
    ),
    ("async-std", "an async runtime"),
    ("smol", "an async runtime"),
    ("mio", "the event loop under a socket"),
    ("socket2", "sockets"),
    ("reqwest", "an HTTP client"),
    ("hyper", "an HTTP implementation"),
    ("tungstenite", "a WebSocket implementation"),
    ("axum", "an HTTP server"),
    ("sqlx", "a database driver"),
    ("diesel", "a database driver"),
    ("rusqlite", "a database driver"),
    ("tokio-postgres", "a database driver"),
    (
        "uuid",
        "v4 reads the OS random source and v7 reads the clock",
    ),
    ("iana-time-zone", "reads `TZ` and the system zone file"),
    ("notify", "watches the filesystem"),
];

/// No domain crate carries a clock, a socket or a database in its dependency
/// graph.
///
/// # Why this is a separate guard from `just purity`
///
/// `just purity` greps *this workspace's* source for `SystemTime::now`, a
/// socket and a filesystem call. It cannot see into a dependency, and a
/// dependency is not our source — so the guarantee that a two-year-old dispute
/// replays to the same answer rests on the feature sets the manifests declare
/// as much as on the code the crates call. That half was reviewable and is now
/// checked (D181): `emob-billing` carried `uuid`/`v7`, and therefore
/// `SystemTime::now`, for sixty lines of adapter that belonged in a daemon.
///
/// Run with `--all-features`, because a feature is exactly how such a
/// dependency comes back.
/// No string a user reads carries a lost line continuation.
///
/// # The defect this is named after
///
/// Rust joins a `\`-continued string literal with no separator, so a long
/// diagnostic is written as
///
/// ```text
/// "the record is refused because \
///  the evidence does not verify"
/// ```
///
/// and the indentation of the second line is *outside* the literal. Delete the
/// backslash — or let an editor reflow the line — and the indentation moves
/// **inside** it: the message still compiles, still passes every test that
/// checks it `contains` a phrase, and reaches an operator as "because
/// ⟨eleven spaces⟩ the evidence".
///
/// Nothing catches it. `clippy` has no lint, `rustfmt` does not touch literal
/// contents, and the tests that read these strings all match on substrings that
/// do not span the join. Six of them had accumulated across five crates before
/// anything looked, in exactly the places a defect hurts most: the text of a
/// refusal, which is the only thing a person sees when the platform says no
/// (D234).
///
/// # The rule, and why it has no false positives
///
/// A run of **three or more** spaces, **interior** to a string literal — between
/// two non-space characters — in a line that is not a comment. Interior is what
/// makes it precise: a literal that *starts* with spaces is testing
/// indentation, and there is one of those in this file. Comments are excluded
/// because a doc example's alignment is code formatting rather than prose.
fn check_prose(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let mut scanned = 0;

    for file in rust_sources(root)? {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        scanned += 1;
        // The literals this is about are the ones a **person** reads — a
        // refusal, a note, a diagnostic — and those live in a crate's own
        // source. A test's literals are fixtures: `"   "` is the blank id a
        // constructor has to refuse, not a sentence with a hole in it, and
        // failing the build over one teaches a reader to reach for the allow.
        // `check-reach` and `check-constructors` cut at the same line, for the
        // same reason.
        let text = text
            .split_once("\n#[cfg(test)]")
            .map_or(text.as_str(), |(head, _)| head);
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for literal in string_literals(line) {
                if let Some(run) = lost_continuation(literal) {
                    problems.push(format!(
                        "{}:{}: a string literal reads {run:?} — {} spaces inside a sentence is a \
                         `\\` line continuation somebody dropped, and the message reaches a \
                         reader with a hole in it",
                        file.strip_prefix(root).unwrap_or(&file).display(),
                        number + 1,
                        run.len(),
                    ));
                    break;
                }
            }
        }
    }

    if !problems.is_empty() {
        eprintln!("❌ strings with a lost line continuation:");
        for problem in &problems {
            eprintln!("   {problem}");
        }
        bail!("{} broken string(s)", problems.len());
    }
    println!("✍️  check-prose: {scanned} files, every sentence a person reads in one piece");
    Ok(())
}

/// The contents of each double-quoted literal on a line.
///
/// Deliberately not a Rust lexer: it handles the escape that matters (`\"`) and
/// treats anything else as content, which is enough to find a run of spaces and
/// cannot produce a false positive on its own — the worst it does on an oddity
/// like a raw string is split one literal into two, and a run of spaces is still
/// a run of spaces in whichever half it lands.
fn string_literals(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j] != b'"' {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        if j > bytes.len() {
            break;
        }
        out.push(&line[start..j.min(line.len())]);
        i = j + 1;
    }
    out
}

/// The run of spaces a dropped continuation left, if there is one.
fn lost_continuation(literal: &str) -> Option<&str> {
    let bytes = literal.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b' ' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        // Interior: something before it and something after it. A leading run is
        // a literal that is *about* indentation, and a trailing one is a
        // deliberate separator.
        if i - start >= 3 && start > 0 && i < bytes.len() {
            return Some(&literal[start..i]);
        }
    }
    None
}

/// Names this guard does not ask about, because the answer is never
/// interesting.
///
/// Trait methods reached through a trait — `Display::fmt`, `FromStr::from_str`,
/// `Default::default` — and the constructors every type has. Each is called
/// through the trait or through inference, so the textual search below cannot
/// see the caller and the finding would be noise a reader learns to skip.
const REACHED_BY_INFERENCE: &[&str] = &[
    "new",
    "default",
    "fmt",
    "from",
    "from_str",
    "clone",
    "eq",
    "cmp",
    "hash",
    "next",
    "serialize",
    "deserialize",
    "main",
    "drop",
    "into",
    "try_from",
    "as_ref",
    "add",
    "sub",
    "sum",
    "extend",
];

/// Every `pub fn` a domain crate exports is named somewhere other than its own
/// definition.
///
/// # The half of rule 1 nothing looked for
///
/// *"A field that is parsed and never consulted is a rule that is not
/// enforced."* Three separate audit passes in this workspace each found the same
/// shape — a specification semantic modelled faithfully and no caller — and each
/// found it by reading. `cargo tree --invert` finds an unused crate and `dead_code`
/// finds an unused private item; **neither sees a `pub fn`**, because `pub` is a
/// promise to a downstream that may not exist yet, and the compiler takes the
/// promise at face value.
///
/// Here it should not be taken at face value. Every crate in this workspace is
/// consumed inside it, and a method nothing calls is a rule nothing enforces
/// however carefully its documentation states the rule. `Chargeable::untransferred_seconds`
/// was the finding that motivated this: it named a real distinction — OCPI defines
/// `total_parking_time` on energy transfer and the priced `PARKING_TIME` dimension
/// on the vehicle's demand — carried the sentence that says so, and had no caller
/// anywhere, while `emob-roam` computed the same figure from the same predicate
/// in its own loop. Two spellings of one rule, one of them enforced (D261).
///
/// A textual search rather than a call graph: it over-approximates reachability,
/// so it can only ever *miss* a dead function, never invent one. Doc comments and
/// Markdown count as references, deliberately — a function named only in prose is
/// documented API, which is a different conversation from a function named
/// nowhere at all.
///
/// The prose it reads is the **tracked** prose. `concepts/` is gitignored and CI
/// builds a clone, so reading it would make this guard stricter where a change
/// merges than where it was written — D228's failure with the sign flipped, and
/// just as hard to act on.
fn check_reach(root: &Path) -> Result<()> {
    let sources = rust_sources(root)?;
    let mut haystack = String::new();
    for file in &sources {
        haystack.push_str(&code_and_quoted(&std::fs::read_to_string(file)?));
    }
    // The **tracked** prose only, and only the part of it that is quoting code.
    //
    // Tracked, because `concepts/` is gitignored and CI builds a clone: reading
    // it would make the guard stricter where a change merges than where it was
    // written — D228's failure with the sign flipped.
    //
    // Backticked, because a function name is also an English word. The partner
    // registry's version setter had no caller anywhere and passed this check on
    // an unrelated doc comment that happened to use its name as a verb — so a
    // registry recorded which OCPI version a peer speaks and nothing read it
    // (D265). A reference to an API in this workspace's prose is written in
    // backticks; a bare word is prose.
    for file in prose_sources(root)? {
        if file.starts_with(root.join("concepts")) {
            continue;
        }
        haystack.push_str(&quoted_spans(&std::fs::read_to_string(&file)?));
    }

    let mut unreached = Vec::new();
    let mut checked = 0usize;
    for file in &sources {
        let relative = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string();
        if !relative.contains("/src/") {
            continue;
        }
        let text = std::fs::read_to_string(file)?;
        // The definitions inside a `#[cfg(test)]` module are the tests' own, and
        // a test helper nothing else calls is exactly what a test helper is.
        let body = text
            .split_once("\n#[cfg(test)]")
            .map_or(text.as_str(), |(head, _)| head);
        for (number, line) in body.lines().enumerate() {
            let Some(name) = public_fn_name(line) else {
                continue;
            };
            if REACHED_BY_INFERENCE.contains(&name) {
                continue;
            }
            checked += 1;
            if mentions(&haystack, name) <= 1 {
                unreached.push(format!(
                    "{relative}:{}: `{name}` is public and named nowhere but its own definition — \
                     a rule with no caller is a rule that is not enforced. Give it the caller it \
                     was written for, or delete it",
                    number + 1,
                ));
            }
        }
    }

    if !unreached.is_empty() {
        eprintln!("❌ public functions nothing reaches:");
        for problem in &unreached {
            eprintln!("   {problem}");
        }
        bail!("{} unreached public function(s)", unreached.len());
    }
    println!("🔗 check-reach: {checked} public functions, every one of them called");
    Ok(())
}

/// Rust source with its comments reduced to the parts that quote code.
///
/// A name in running prose is an English word; a name in backticks or an
/// intra-doc link is a reference. The partner registry's version setter passed
/// this guard on an unrelated doc comment that used its name as an ordinary
/// verb, while having no caller at all (D265).
fn code_and_quoted(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push_str(&quoted_spans(line));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Only what a text puts in backticks.
fn quoted_spans(text: &str) -> String {
    let mut out = String::new();
    for quoted in text.split('`').skip(1).step_by(2) {
        out.push_str(quoted);
        out.push('\n');
    }
    out
}

/// The name a line defines, when the line defines a public function.
fn public_fn_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("pub ")?;
    let rest = rest.strip_prefix("const ").unwrap_or(rest);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("unsafe ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let end = rest.find(|c: char| !c.is_alphanumeric() && c != '_')?;
    Some(&rest[..end])
}

/// How many times a name appears as a whole word.
fn mentions(haystack: &str, name: &str) -> usize {
    let mut count = 0;
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(at) = haystack[from..].find(name) {
        let start = from + at;
        let end = start + name.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            count += 1;
        }
        from = end;
    }
    count
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The constructors that state a rule are the constructors the **wire** goes
/// through.
///
/// # A validating constructor beside a derived `Deserialize` is a rule with a
/// door around it
///
/// `Energy::from_kwh` refuses a negative, because a magnitude and a `Direction`
/// are two fields precisely so a V2G discharge can never cancel a draw.
/// `TokenRef::new` refuses anything that is not a keyed digest, because storing
/// a raw RFID UID is a privacy incident. `PartyId::new` upper-cases, because
/// `DE*ABC` and `deabc` becoming two entries in one routing table is the failure
/// its own documentation names. Every one of them was derived past: a
/// `#[serde(transparent)]` or a field-by-field derive restores the value without
/// asking the type anything, and **that is the path values actually arrive by**
/// — a store, an outbox, a partner's document.
///
/// A charge detail record carrying a period of `-10.000` kWh deserialised,
/// conserved, and validated as settleable (D264). The workspace had already
/// learned this three times — `Currency`, `ClockResolution` and `QuarterHour`
/// each carry a hand-written pair with a comment saying why — and had not
/// generalised it.
///
/// So: a type with a fallible `new`/`parse` may not **derive** `Deserialize`.
/// It may still derive `Serialize`, which asserts nothing.
///
/// The search is textual and deliberately narrow — a derive attribute
/// immediately above a `pub struct`/`pub enum`, and a brace-matched `impl` block
/// for that name holding a `pub fn new(..) -> Result`. It cannot invent a
/// finding: both halves have to be there in the same file.
fn check_constructors(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let mut checked = 0usize;

    for file in rust_sources(root)? {
        let relative = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();
        if !relative.contains("/src/") {
            continue;
        }
        let text = std::fs::read_to_string(&file)?;
        let head = text
            .split_once("\n#[cfg(test)]")
            .map_or(text.as_str(), |(head, _)| head);
        for name in derived_deserialize_types(head) {
            checked += 1;
            // A type whose fields are all public can be built field by field
            // anyway, so a validating deserialiser would be a check the caller
            // walks around — which is not a check (rule 3). The rule is about
            // the types whose constructor is the **only** door.
            if has_private_field(head, &name) && has_fallible_constructor(head, &name) {
                problems.push(format!(
                    "{relative}: `{name}` has a fallible constructor and **derives** \
                     `Deserialize`, so the rule the constructor states is not asked of a value \
                     that arrives from a store, an outbox or a partner. Write the impl and go \
                     through the constructor, as `Currency` and `ClockResolution` do"
                ));
            }
        }
    }

    if !problems.is_empty() {
        eprintln!("❌ constructors the wire walks past:");
        for problem in &problems {
            eprintln!("   {problem}");
        }
        bail!("{} derived deserialiser(s) past a rule", problems.len());
    }
    println!("🚪 check-constructors: {checked} derived deserialiser(s), none of them past a rule");
    Ok(())
}

/// Every type whose `Deserialize` is derived rather than written.
///
/// A derive attribute and the item it applies to, with only further attributes
/// and comments between them — anything else means the attribute belongs to
/// something the search is not looking at.
fn derived_deserialize_types(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (index, line) in src.lines().enumerate() {
        if !line.contains("derive(") || !line.contains("Deserialize") {
            continue;
        }
        for following in src.lines().skip(index + 1) {
            let trimmed = following.trim_start();
            if trimmed.starts_with("#[") || trimmed.starts_with("//") || trimmed.is_empty() {
                continue;
            }
            if let Some(rest) = trimmed
                .strip_prefix("pub struct ")
                .or_else(|| trimmed.strip_prefix("pub enum "))
            {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.push(name);
                }
            }
            break;
        }
    }
    out
}

/// Whether a type declares a field the outside cannot set.
///
/// The test that separates a value with an invariant from a bag of fields. A
/// tuple struct's field is private unless it says otherwise; a named field is
/// private unless it opens with `pub`. Where every field is public the
/// constructor is one door among many, and a rule enforced at one of several
/// doors is not enforced.
fn has_private_field(src: &str, name: &str) -> bool {
    for opener in [
        format!("pub struct {name} {{"),
        format!("pub struct {name}("),
    ] {
        let Some(at) = src.find(&opener) else {
            continue;
        };
        let body_start = at + opener.len();
        let close = if opener.ends_with('(') { ')' } else { '}' };
        let Some(end) = src[body_start..].find(close) else {
            continue;
        };
        let body = &src[body_start..body_start + end];
        if opener.ends_with('(') {
            return !body.trim_start().starts_with("pub ");
        }
        return body
            .lines()
            .map(str::trim_start)
            .filter(|line| {
                !line.is_empty()
                    && !line.starts_with("//")
                    && !line.starts_with("#[")
                    && !line.starts_with("///")
            })
            .any(|line| !line.starts_with("pub "));
    }
    false
}

/// Whether a type's own `impl` block states a constructor that can refuse.
///
/// A **constructor** is an associated function that does not take `self`; one
/// that can **refuse** returns `Result`. Named by shape rather than by a list of
/// names, because the list is what a guard gets wrong: `Energy`'s is
/// `from_kwh`, `ClockResolution`'s is `stated`, and a search for `new` and
/// `parse` walks past both — which it did, on the type this whole check was
/// written for.
fn has_fallible_constructor(src: &str, name: &str) -> bool {
    for body in impl_bodies(src, name) {
        for (at, _) in body.match_indices("pub ") {
            let rest = &body[at..];
            let Some(after_fn) = signature_after_fn(rest) else {
                continue;
            };
            let Some(open) = after_fn.find('(') else {
                continue;
            };
            // A method, not a constructor.
            if after_fn[open + 1..].trim_start().starts_with("self")
                || after_fn[open + 1..].trim_start().starts_with("&self")
                || after_fn[open + 1..].trim_start().starts_with("&mut self")
                || after_fn[open + 1..].trim_start().starts_with("mut self")
            {
                continue;
            }
            // …and one that can refuse says so in the arrow, which may sit a few
            // lines down on a wrapped signature.
            let head: String = after_fn.lines().take(8).collect::<Vec<_>>().join(" ");
            let Some(arrow) = head.find("->") else {
                continue;
            };
            if head[arrow..].trim_start().starts_with("-> Result") {
                return true;
            }
        }
    }
    false
}

/// The text of every `impl <name>` block in a file, brace-matched.
fn impl_bodies<'a>(src: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let needle = format!("impl {name} ");
    let mut from = 0;
    while let Some(at) = src[from..].find(&needle) {
        let start = from + at;
        let Some(open) = src[start..].find('{') else {
            break;
        };
        let body_start = start + open + 1;
        let mut depth = 1usize;
        let mut end = body_start;
        for byte in src[body_start..].bytes() {
            if byte == b'{' {
                depth += 1;
            } else if byte == b'}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            end += 1;
        }
        out.push(&src[body_start..end.min(src.len())]);
        from = end.max(start + needle.len());
    }
    out
}

/// What follows `fn` in a `pub [const] [async] fn …` signature, if this is one.
fn signature_after_fn(rest: &str) -> Option<&str> {
    let rest = rest.strip_prefix("pub ")?;
    let rest = rest.strip_prefix("const ").unwrap_or(rest);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    rest.strip_prefix("fn ")
}

fn check_graph(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let mut checked = 0;

    for crate_name in PURE_CRATES {
        if !root.join("crates").join(crate_name).exists() {
            continue;
        }
        let output =
            std::process::Command::new(std::env::var("CARGO").as_deref().unwrap_or("cargo"))
                .current_dir(root)
                .args([
                    "tree",
                    "-p",
                    crate_name,
                    "--edges",
                    "normal",
                    "--all-features",
                    "--prefix",
                    "none",
                ])
                .output()
                .with_context(|| format!("running `cargo tree` for {crate_name}"))?;
        if !output.status.success() {
            bail!(
                "`cargo tree -p {crate_name}` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        checked += 1;

        let tree = String::from_utf8_lossy(&output.stdout).into_owned();
        let names: BTreeSet<&str> = tree
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        for (dependency, why) in AMBIENT {
            if names.contains(dependency) {
                problems.push(format!(
                    "{crate_name} depends on `{dependency}` — {why}. A crate that promises to be \
                     replayable cannot carry one, whatever it happens to call: see D181"
                ));
            }
        }
    }

    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("  {problem}");
        }
        bail!(
            "{} domain crate dependency(s) reach ambient state",
            problems.len()
        );
    }
    println!(
        "🧊 check-graph: {checked} domain crate(s) carry no clock, socket or database in their dependency graphs"
    );
    Ok(())
}

fn check_manifests(root: &Path) -> Result<()> {
    let crates_dir = root.join("crates");
    if !crates_dir.exists() {
        return Ok(());
    }
    let mut problems = Vec::new();
    let mut checked = 0;

    for entry in std::fs::read_dir(&crates_dir)? {
        let dir = entry?.path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.exists() {
            continue;
        }
        checked += 1;
        let text = std::fs::read_to_string(&manifest)?;

        for field in ["readme", "description", "license", "repository", "keywords"] {
            let declared = text
                .lines()
                .any(|l| l.trim_start().starts_with(field) && l.contains('='));
            if !declared {
                problems.push(format!(
                    "{}: no `{field}`",
                    manifest.strip_prefix(root).unwrap_or(&manifest).display()
                ));
            }
        }

        if let Some(line) = text.lines().find(|l| l.trim_start().starts_with("readme"))
            && let Some(name) = line.split('=').nth(1)
        {
            {
                let name = name.trim().trim_matches('"');
                if !dir.join(name).exists() {
                    problems.push(format!(
                        "{}: readme = {name:?} does not exist",
                        manifest.strip_prefix(root).unwrap_or(&manifest).display()
                    ));
                }
            }
        }

        // The registry's own limits on `keywords` and `categories`. A field
        // that merely *exists* still fails the upload if its contents break a
        // rule, and it fails at the far end — after the version has been spent
        // on every crate published ahead of this one in the same run.
        let where_ = manifest.strip_prefix(root).unwrap_or(&manifest).display();
        if let Some(keywords) = array_field(&text, "keywords") {
            if keywords.len() > MAX_KEYWORDS {
                problems.push(format!(
                    "{where_}: {} keywords, and crates.io takes at most {MAX_KEYWORDS}",
                    keywords.len()
                ));
            }
            for keyword in &keywords {
                if keyword.chars().count() > MAX_KEYWORD_CHARS {
                    problems.push(format!(
                        "{where_}: keyword {keyword:?} is {} characters, and crates.io takes at most {MAX_KEYWORD_CHARS}",
                        keyword.chars().count()
                    ));
                }
                let shape = keyword
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                    && keyword
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'));
                if !shape {
                    problems.push(format!(
                        "{where_}: keyword {keyword:?} must begin with a letter or digit and hold only letters, digits, `_`, `-` or `+`"
                    ));
                }
            }
        }
        if let Some(categories) = array_field(&text, "categories")
            && categories.len() > MAX_CATEGORIES
        {
            problems.push(format!(
                "{where_}: {} categories, and crates.io takes at most {MAX_CATEGORIES}",
                categories.len()
            ));
        }
    }

    if !problems.is_empty() {
        eprintln!("❌ manifests that cannot be published:");
        for p in &problems {
            eprintln!("   {p}");
        }
        bail!("{} manifest problem(s)", problems.len());
    }
    println!("📦 check-manifests: {checked} crate(s) can be packaged");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_detection_is_token_aware() {
        assert!(find_type_token("let x: f64 = 1.0;", "f64").is_some());
        assert!(find_type_token("(f64, f64)", "f64").is_some());
        assert!(find_type_token("Decimal::from_f64(x)", "f64").is_some());

        // …and the shape the token search cannot see at all: `try_from` takes
        // an `f64`, so a value laundered out of a dependency's float field
        // reaches an exact type without the word appearing in this workspace.
        // Checked by name in `no_floats` rather than here, because there is no
        // type to look at.
        assert!(find_type_token("Decimal::try_from(x)", "f64").is_none());
        // …but not a substring of a longer identifier.
        assert!(find_type_token("let sf64x = 1;", "f64").is_none());
        assert!(find_type_token("my_f64_helper()", "f64").is_none());
        assert!(find_type_token("let float64 = 1;", "f64").is_none());
    }

    #[test]
    fn a_manifest_field_that_exists_can_still_be_rejected_on_upload() {
        // The limits are the registry's, enforced server-side, one crate at a
        // time — so a keyword three characters too long fails the *upload*,
        // after every crate published ahead of it has already spent its version.
        assert_eq!(
            array_field(r#"keywords = ["ev-charging", "afir"]"#, "keywords"),
            Some(vec!["ev-charging".to_owned(), "afir".to_owned()])
        );
        // …written across several lines, which is how a long list is kept
        // readable and is exactly where a text guard usually stops looking.
        assert_eq!(
            array_field(
                "keywords = [\n  \"ocmf\",\n  \"eichrecht\",\n]\n",
                "keywords"
            ),
            Some(vec!["ocmf".to_owned(), "eichrecht".to_owned()])
        );
        assert_eq!(array_field("description = \"x\"", "keywords"), None);

        // The one that got through: 23 characters against a limit of 20.
        assert!("charging-infrastructure".chars().count() > MAX_KEYWORD_CHARS);
    }

    #[test]
    fn a_citation_is_told_apart_from_a_doc_link_and_an_array() {
        // The guard's second half only works if this is tight: a false
        // positive fails the build over a Rust doc link, which is how a guard
        // gets switched off rather than fixed.
        assert_eq!(
            citation_phrases("see [MessEG §33] for this"),
            ["MessEG §33"]
        );
        assert_eq!(citation_phrases("`[OCMF Tab. 25]`"), ["OCMF Tab. 25"]);
        assert_eq!(
            citation_phrases("[DA-656 Anh. 2.1.1] and [AFIR Art. 5(1)]"),
            ["DA-656 Anh. 2.1.1", "AFIR Art. 5(1)"]
        );

        for not_a_citation in [
            "[`Self::foo`] links to a method",
            "let bytes: [u8; 32] = digest;",
            "[a markdown link](https://example.com)",
            "[lower case prose]",
            "an unclosed [bracket",
            "[MessEG] with no section marker",
        ] {
            assert!(
                citation_phrases(not_a_citation).is_empty(),
                "{not_a_citation:?} is not a citation"
            );
        }
    }

    #[test]
    fn a_count_written_in_words_is_read_as_a_number() {
        // The spellings the documents actually use, including the article that
        // stands in for the multiplier.
        assert_eq!(
            words_to_number("A hundred and seventy-four things"),
            Some(174)
        );
        assert_eq!(
            words_to_number("Two hundred and thirteen things"),
            Some(213)
        );
        assert_eq!(words_to_number("## Ten rules this design"), Some(10));
        assert_eq!(words_to_number("## Nine rules"), Some(9));
        assert_eq!(words_to_number("Twenty-one entries"), Some(21));
        // …and prose with no count in it is not a count of zero.
        assert_eq!(
            words_to_number("Each is now a test rather than a paragraph."),
            None
        );
    }

    #[test]
    fn a_section_intro_is_not_an_entry() {
        // `**D34–D40 came from …**` introduces a run; `**D34 — …**` is one of
        // them. Counting the first as an entry would report the log holding
        // more than it does, and the em dash after the number is the difference.
        let text = "\
**D34–D40 came from running a record this workspace did not write.** Every test
**D34 — secp192r1 is not a legacy curiosity.** The reference
**D35 — Something else.** More
not a decision at all
";
        assert_eq!(decision_numbers(text), vec![34, 35]);
    }

    #[test]
    fn the_rule_list_is_read_with_its_heading() {
        let text = "\
## Ten rules this design keeps re-learning

Some prose.

1. **First.** Body
   continued.
2. **Second.** Body

## Feedback to sibling crates
3. **Not a rule — this is past the section.**
";
        let (heading, items) = numbered_rules(text);
        assert_eq!(heading, Some(10));
        assert_eq!(items, vec![1, 2], "the list stops at the next heading");
    }

    #[test]
    fn comments_are_prose_not_code() {
        assert_eq!(code_part("// an f64 would round here"), "");
        assert_eq!(code_part("    /// f64 is forbidden"), "");
        assert_eq!(code_part("let x = 1; // f64"), "let x = 1; ");
        assert_eq!(code_part("let x: u8 = 1;"), "let x: u8 = 1;");
    }

    #[test]
    fn only_the_numbered_rows_of_the_property_index_are_counted() {
        // The index is a table now, so the header and the delimiter row are the
        // shapes that must not count — and neither must a table further down the
        // document. A row counts when it opens with its own number, which is how
        // a reader counts the list and therefore how the guard has to.
        let readme = "\n## The three properties that decide quality\n\n\
                      | # | Property | Where |\n\
                      |---|---|---|\n\
                      | 1 | One | [Eichrecht](x) |\n\
                      | 2 | Two | [Tariffs](y) |\n\
                      | 3 | Three | [Roaming](z) |\n\
                      \n## Layout\n| 9 | Not counted | here |\n";
        assert_eq!(property_count(readme), (Some(3), 3));
        assert_eq!(property_count("no heading here"), (None, 0));
    }

    #[test]
    fn a_constructor_is_told_apart_from_a_method_by_its_first_parameter() {
        // The distinction the whole check turns on: a function that takes
        // `self` cannot be the door a value arrives through, and one that
        // returns `Result` without taking `self` is exactly that door — whatever
        // it is called. Naming it by shape rather than by a list of names is why
        // this finds `Energy::from_kwh` and `ClockResolution::stated`, neither
        // of which is called `new`.
        let src = "\
impl Energy {
    pub fn from_kwh(kwh: Decimal) -> Result<Self, QuantityError> { }
}
impl Money {
    pub fn checked_add(self, rhs: Self) -> Result<Self, QuantityError> { }
}
impl Plain {
    pub fn new(v: u8) -> Self { }
}
";
        assert!(has_fallible_constructor(src, "Energy"));
        assert!(
            !has_fallible_constructor(src, "Money"),
            "a method that can fail is not a constructor"
        );
        assert!(
            !has_fallible_constructor(src, "Plain"),
            "a constructor that cannot refuse states no rule"
        );
    }

    #[test]
    fn a_bag_of_public_fields_has_no_door_to_guard() {
        // A type anybody can build field by field gains nothing from a
        // validating deserialiser, because the caller walks around it — which is
        // not a check (rule 3). So the guard asks only about types whose
        // constructor is the only way in.
        let src = "\
pub struct Energy(Decimal);
pub struct Wrapper(pub Decimal);
pub struct Hidden {
    /// A doc comment.
    #[serde(default)]
    inner: u8,
    pub shown: u8,
}
pub struct Open {
    pub a: u8,
    pub b: u8,
}
";
        assert!(has_private_field(src, "Energy"));
        assert!(!has_private_field(src, "Wrapper"));
        assert!(has_private_field(src, "Hidden"));
        assert!(!has_private_field(src, "Open"));
    }

    #[test]
    fn a_derive_names_the_item_it_sits_above() {
        let src = "\
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = \"snake_case\")]
pub enum Kind { A }

#[derive(Debug, Clone)]
pub struct NotDeserialised;

#[derive(serde::Deserialize)]
fn not_an_item() {}
";
        assert_eq!(derived_deserialize_types(src), vec!["Kind".to_owned()]);
    }

    #[test]
    fn a_public_function_is_recognised_however_it_is_qualified() {
        assert_eq!(
            public_fn_name("    pub fn split(&self) -> u8 {"),
            Some("split")
        );
        assert_eq!(
            public_fn_name("    pub const fn is_zero(self) -> bool {"),
            Some("is_zero")
        );
        assert_eq!(public_fn_name("pub async fn serve() {"), Some("serve"));
        // Not a definition, not a `pub fn`, and not this guard's business.
        assert_eq!(public_fn_name("    fn seconds_where(&self) -> u64 {"), None);
        assert_eq!(public_fn_name("    pub(crate) fn hidden() {"), None);
        assert_eq!(public_fn_name("    pub struct Thing {"), None);
        assert_eq!(public_fn_name("// pub fn in a comment"), None);
    }

    #[test]
    fn a_name_is_reached_only_as_a_whole_word() {
        // The failure a substring search would produce is the wrong direction:
        // `split` inside `splitter` would report a caller that is not one, and
        // the guard would pass over a function nothing calls.
        assert_eq!(mentions("fn split(); split(); splitter();", "split"), 2);
        assert_eq!(mentions("resplit(); split_at();", "split"), 0);
        assert_eq!(mentions("`split`", "split"), 1, "prose counts");
        assert_eq!(mentions("", "split"), 0);
    }

    #[test]
    fn a_test_binary_names_the_target_it_was_built_from() {
        // The hash is the last `-` segment and a target name may hold its own,
        // which is why this splits from the right rather than the left.
        assert_eq!(
            running_target("unittests src/lib.rs (target/debug/deps/emob_core-7edb93fa697813a7)")
                .as_deref(),
            Some("emob_core")
        );
        assert_eq!(
            running_target("tests/the_other_hat.rs (target/debug/deps/the_other_hat-cc9b7c27c3e4)")
                .as_deref(),
            Some("the_other_hat")
        );
        // A doc-test section carries no parenthesised path, and belongs to no
        // row: `enumerate_suite` recognises it by its own prefix and this by
        // returning nothing rather than a wrong owner.
        assert_eq!(running_target("Doc-tests emob_core"), None);
    }

    #[test]
    fn a_rows_verdict_is_read_only_where_it_opens_with_a_count() {
        // The prose after the figure says how the tests divide, which is worth
        // reading and is not a claim a guard can hold — so only the leading
        // count is taken, and a row that does not state one is a finding rather
        // than a silent pass.
        assert_eq!(
            stated_row_count("127 tests — 124 in the crate, 3 more"),
            Some(127)
        );
        assert_eq!(stated_row_count("20 tests over 2,594 lines"), Some(20));
        assert_eq!(stated_row_count("39 tests"), Some(39));
        assert_eq!(stated_row_count("124 + 3 agreeing with the kit"), None);
        assert_eq!(stated_row_count("published"), None);
    }

    #[test]
    fn a_table_row_is_split_on_the_pipes_that_separate_it() {
        // The outer pipes delimit rather than separate, so a three-column row
        // is three cells and not five.
        assert_eq!(table_cells("| a | b | c |").len(), 3);
        assert_eq!(table_cells("|---|---|---|").len(), 3);
        // A cell in this workspace routinely carries a pipe inside a code span
        // — `f32`/`f64`, `a|b` — and one escaped in prose.
        assert_eq!(table_cells("| `a|b` | c |").len(), 2);
        assert_eq!(table_cells(r"| a \| b | c |").len(), 2);
        // …and a backslash **inside** a code span is content rather than an
        // escape: Markdown does not process escapes there, so honouring one
        // swallows the closing backtick, leaves the parser inside a code span
        // for the rest of the line and reports the row as short. The guard
        // found that on itself, on the first table row that mentioned an
        // escape.
        assert_eq!(table_cells(r"| `\` | the escape | a cell |").len(), 3);
        // And the shape the guard exists for: a cell that belongs to the row
        // above, which Markdown renders by dropping it.
        assert_eq!(table_cells("| a | b | c | d |").len(), 4);
    }

    #[test]
    fn a_rows_subject_is_the_first_crate_it_names() {
        // The service rows carry their state marker in the same cell — `` `csmsd`
        // ✅ `` — so the name is the backticked token and not the whole cell.
        assert_eq!(backticked(" `emob-core` "), Some("emob-core"));
        assert_eq!(backticked(" `csmsd` ✅ "), Some("csmsd"));
        assert_eq!(backticked(" Crate "), None);
    }
}
