//! `check-actions` — every reference the build pulls from somebody else is
//! pinned to something that cannot move.
//!
//! A tag is a pointer its owner can repoint. A release workflow that says
//! `actions/checkout@v4` runs whatever `v4` points at on the day it runs —
//! including the day somebody else's account is compromised — and the release
//! it builds carries a provenance attestation vouching for it. The same holds
//! for `FROM rust:1.96-bookworm`. A commit and a digest cannot move; the tag
//! stays beside them as a comment, so a reader still sees which release it is.
//!
//! The third check is the blast radius: a workflow without a top-level
//! `permissions:` runs every job with the repository's default token scopes,
//! so one compromised step in a lint job can write.
//!
//! There is no update bot. A pin moves by hand, in a reviewed diff, and this
//! guard is what keeps the hand from writing a tag.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub fn check_actions(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let mut actions = 0usize;
    let mut images = 0usize;

    let dir = root.join(".github/workflows");
    let mut workflows: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml"))
                .collect()
        })
        .unwrap_or_default();
    workflows.sort();
    for file in &workflows {
        let text = std::fs::read_to_string(file)?;
        let shown = file.strip_prefix(root).unwrap_or(file).display();
        if !text.lines().any(|l| l.starts_with("permissions:")) {
            problems.push(format!(
                "{shown}: no top-level `permissions:` — every job gets the default token scopes"
            ));
        }
        for (n, line) in text.lines().enumerate() {
            if let Some(target) = action_reference(line) {
                if action_is_pinned(target) {
                    actions += 1;
                } else {
                    problems.push(format!(
                        "{shown}:{}: `{target}` is a movable ref — pin the commit SHA, tag as a comment",
                        n + 1
                    ));
                }
            }
        }
    }

    let dockerfile = root.join("Dockerfile");
    if let Ok(text) = std::fs::read_to_string(&dockerfile) {
        let mut stages = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let Some((image, stage)) = from_line(line) else {
                continue;
            };
            if stages
                .iter()
                .any(|s: &String| s.eq_ignore_ascii_case(image))
            {
                // `FROM build` names an earlier stage, not an image.
            } else if image.contains("@sha256:") {
                images += 1;
            } else {
                problems.push(format!(
                    "Dockerfile:{}: `{image}` has no `@sha256:` digest — a tag is a pointer its owner can move",
                    n + 1
                ));
            }
            if let Some(stage) = stage {
                stages.push(stage.to_string());
            }
        }
    }

    if !problems.is_empty() {
        eprintln!("❌ check-actions: references that can move under a release:");
        for p in &problems {
            eprintln!("   {p}");
        }
        bail!("{} unpinned reference(s)", problems.len());
    }
    println!(
        "🔗 check-actions: {actions} action reference(s) across {} workflow(s) and {images} base \
         image(s), every one pinned",
        workflows.len()
    );
    Ok(())
}

/// The target of a `uses:` key on this line, without a trailing comment.
fn action_reference(line: &str) -> Option<&str> {
    let trimmed = line.trim_start().trim_start_matches("- ").trim_start();
    let rest = trimmed.strip_prefix("uses:")?;
    let target = rest
        .split('#')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(['"', '\'']);
    (!target.is_empty()).then_some(target)
}

/// Pinned means a local action, a container digest, or `owner/repo@<40 hex>`.
fn action_is_pinned(target: &str) -> bool {
    if target.starts_with("./") {
        return true;
    }
    if target.starts_with("docker://") {
        return target.contains("@sha256:");
    }
    target.rsplit_once('@').is_some_and(|(_, r)| {
        r.len() == 40
            && r.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// `FROM [--platform=…] <image> [AS <stage>]` → the image and the stage name.
fn from_line(line: &str) -> Option<(&str, Option<&str>)> {
    let mut words = line.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("FROM") {
        return None;
    }
    let image = words.find(|w| !w.starts_with("--"))?;
    let stage = match (words.next(), words.next()) {
        (Some(kw), Some(name)) if kw.eq_ignore_ascii_case("AS") => Some(name),
        _ => None,
    };
    Some((image, stage))
}

#[cfg(test)]
mod tests {
    use super::*;

    // SR-080: a tag, a branch or a short SHA is not a pin; a full commit is.
    #[test]
    fn a_tag_or_a_branch_is_not_a_pin_and_a_commit_is() {
        assert!(!action_is_pinned("actions/checkout@v4"));
        assert!(!action_is_pinned("dtolnay/rust-toolchain@stable"));
        assert!(!action_is_pinned("actions/checkout@3d3c42e5"));
        assert!(!action_is_pinned("docker://alpine:3"));
        assert!(action_is_pinned(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
        ));
        assert!(action_is_pinned("./.github/actions/local"));
    }

    #[test]
    fn the_reference_is_read_past_the_list_dash_and_the_tag_comment() {
        assert_eq!(
            action_reference(
                "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1"
            ),
            Some("actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1")
        );
        assert_eq!(action_reference("        uses: \"a/b@v1\""), Some("a/b@v1"));
        assert_eq!(action_reference("      - run: cargo test"), None);
    }

    // SR-081: `FROM` names an image or an earlier stage.
    #[test]
    fn a_from_line_yields_its_image_and_stage() {
        assert_eq!(
            from_line("FROM rust:1.96-bookworm@sha256:ab AS build"),
            Some(("rust:1.96-bookworm@sha256:ab", Some("build")))
        );
        assert_eq!(
            from_line("FROM --platform=$BUILDPLATFORM build"),
            Some(("build", None))
        );
        assert_eq!(from_line("# FROM nothing"), None);
    }

    /// A throwaway workspace with one workflow and one Dockerfile.
    fn workspace(name: &str, workflow: &str, dockerfile: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("emob-check-actions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        std::fs::write(root.join(".github/workflows/ci.yml"), workflow).unwrap();
        std::fs::write(root.join("Dockerfile"), dockerfile).unwrap();
        root
    }

    const PINNED: &str = "permissions:\n  contents: read\njobs:\n  a:\n    steps:\n      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n";
    const DOCKER: &str = "FROM rust:1@sha256:00 AS build\nFROM build\n";

    // SR-080, SR-081, SR-086: the guard passes a pinned tree and fails, once
    // each, on a tag, a digest-less base image and a missing permissions block.
    #[test]
    fn the_guard_fails_on_a_tag_a_bare_base_image_and_missing_permissions() {
        let ok = workspace("ok", PINNED, DOCKER);
        assert!(check_actions(&ok).is_ok());

        let tag = workspace(
            "tag",
            &PINNED.replace("3d3c42e5aac5ba805825da76410c181273ba90b1", "v4"),
            DOCKER,
        );
        let err = check_actions(&tag).unwrap_err().to_string();
        assert_eq!(err, "1 unpinned reference(s)");

        let image = workspace(
            "image",
            PINNED,
            "FROM rust:1.96-bookworm AS build\nFROM build\n",
        );
        assert!(check_actions(&image).is_err());

        let perms = workspace(
            "perms",
            &PINNED.replace("permissions:\n  contents: read\n", ""),
            DOCKER,
        );
        assert!(check_actions(&perms).is_err());

        for root in [ok, tag, image, perms] {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
