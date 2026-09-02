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
        Some("check-all") => {
            no_floats(&root)?;
            check_citations(&root)?;
            check_manifests(&root)
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
  check-all         all of the above
"
    );
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
    fn comments_are_prose_not_code() {
        assert_eq!(code_part("// an f64 would round here"), "");
        assert_eq!(code_part("    /// f64 is forbidden"), "");
        assert_eq!(code_part("let x = 1; // f64"), "let x = 1; ");
        assert_eq!(code_part("let x: u8 = 1;"), "let x: u8 = 1;");
    }
}
