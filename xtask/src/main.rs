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
        Some("check-concepts") => check_concepts(&root),
        Some("check-all") => {
            no_floats(&root)?;
            check_citations(&root)?;
            check_manifests(&root)?;
            check_wire(&root)?;
            check_concepts(&root)?;
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
  check-concepts    every count concepts/ states about itself is the count it
                    holds: the decision log's own total, and the numbered list
                    of rules its README says how many of there are
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
/// So the two claims are checked, and the numbering is checked with them: a
/// stable identifier that skips or repeats is a citation from code that quietly
/// means something else.
fn check_concepts(root: &Path) -> Result<()> {
    let concepts = root.join("concepts");
    if !concepts.is_dir() {
        println!("check-concepts: concepts/ is absent; skipping");
        return Ok(());
    }

    let mut problems: Vec<String> = Vec::new();

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
        (None, _) => {
            problems.push("concepts/README.md has no '## <spelled-out> rules …' heading".to_owned())
        }
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

    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("❌ {problem}");
        }
        bail!(
            "{} claim{} in concepts/ that the documents do not hold",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
    }

    println!(
        "✅ check-concepts: {held} decisions, {} rules, each the number stated",
        items.len()
    );
    Ok(())
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
    ("[OCPP ", "the OCPP specifications", "ocpp-kit/specs"),
    ("[BGB ", "Bürgerliches Gesetzbuch", "bgb.pdf"),
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
    let index_path = root.join("specs/README.md");
    if !index_path.exists() {
        println!("check-citations: specs/README.md is absent (it is gitignored); skipping");
        return Ok(());
    }
    let index = std::fs::read_to_string(&index_path)?;

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
                    if !index.contains(needle) {
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
            "   add the document to specs/README.md and its prefix to CITATION_SOURCES, \n                or the citation is a claim nobody can follow"
        );
        bail!("{} unrecognised citation(s)", unknown.len());
    }
    println!(
        "📚 check-citations: {} document families cited across {scanned} files, every one indexed in specs/README.md",
        seen.len()
    );
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
                    "{crate_name} depends on `{dependency}` — {why}. A crate that promises to be                      replayable cannot carry one, whatever it happens to call: see D181"
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
}
