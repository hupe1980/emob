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
  check-citations   every regulatory citation in the code names a document that
                    specs/README.md actually indexes
  check-manifests   every publishable crate can be packaged: the files its
                    manifest promises exist
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
        collect(&root.join(top), &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
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
    ("[MessEG ", "MessEG", "messeg.pdf"),
    ("[MessEV", "MessEV", "messev.pdf"),
    ("[PTB-A ", "PTB-A 50.7", "ptb-a-50.7"),
    ("[REA ", "REA Dokument 6-A", "rea-dokument-6-a"),
    ("[38k", "38. BImSchV", "bimschv-38"),
    ("[UStG ", "UStG", "ustg.pdf"),
    ("[PAngV", "PAngV", "pangv.pdf"),
    ("[OCMF ", "OCMF", "ocmf-master.zip"),
    ("[NIS2", "NIS2", "nis2-2022-2555"),
    ("[CRA", "Cyber Resilience Act", "cra-2024-2847"),
    // The NZR-EMob corpus lives in the sibling `mako` workspace, which
    // specs/README.md points at rather than duplicating.
    ("[A6 ", "BK6-20-160 Anlage 6", "mako/regulatories"),
    ("[M2 ", "BDEW AWH „Zum Modell 2\"", "mako/regulatories"),
];

/// Every regulatory claim in emob cites its source in the form `[AFIR Art. 5(1)]`
/// or `[OCMF Tab. 7]`. This checks that the documents those refer to are indexed
/// in `specs/README.md`, so a citation can always be followed to a file and a
/// retrieval URL.
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

    for file in rust_sources(root)? {
        if file.ends_with("xtask/src/main.rs") {
            continue;
        }
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
        }
    }

    if !missing.is_empty() {
        eprintln!("❌ citations without an indexed source:");
        for m in &missing {
            eprintln!("   {m}");
        }
        bail!("{} unindexed citation(s)", missing.len());
    }
    println!(
        "📚 check-citations: {} document families cited, every one indexed in specs/README.md",
        seen.len()
    );
    Ok(())
}

// ── check-manifests ─────────────────────────────────────────────────────────

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
        // …but not a substring of a longer identifier.
        assert!(find_type_token("let sf64x = 1;", "f64").is_none());
        assert!(find_type_token("my_f64_helper()", "f64").is_none());
        assert!(find_type_token("let float64 = 1;", "f64").is_none());
    }

    #[test]
    fn comments_are_prose_not_code() {
        assert_eq!(code_part("// an f64 would round here"), "");
        assert_eq!(code_part("    /// f64 is forbidden"), "");
        assert_eq!(code_part("let x = 1; // f64"), "let x = 1; ");
        assert_eq!(code_part("let x: u8 = 1;"), "let x: u8 = 1;");
    }
}
