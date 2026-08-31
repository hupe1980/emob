# emob — task runner (https://just.systems)
#
# `just` on its own lists every recipe.

set shell := ["bash", "-uc"]

# Keep in sync with rust-toolchain.toml and `rust-version` in Cargo.toml.
msrv := "1.94"

# The one version every publishable crate carries, from `[workspace.package]`.
version := `sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml | sed -n 's/^version *= *"\(.*\)"/\1/p' | head -1`

# 📋 List all recipes
default:
    @just --list

# ✅ Everything CI runs, in CI order
ci: fmt-check lint purity test guards deny doc
    @echo "✅ all checks passed"

# 🎨 Format the workspace
fmt:
    cargo fmt --all

# 🎨 Fail if anything is unformatted
fmt-check:
    cargo fmt --all --check

# 🔍 Clippy, warnings as errors
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# A dispute about a session from two years ago is answered by replaying the
# check exactly as it ran. That is only possible while the verification path
# takes its time and its keys as arguments — the moment one of them reads a
# clock or opens a socket, the replay stops being a replay.
#
# 🧊 Enforce the "no I/O, no clock" promise of the domain crates
purity:
    #!/usr/bin/env bash
    set -uo pipefail
    pure="emob-core emob-eichrecht emob-session emob-cdr emob-tariff"
    fail=0
    for crate in $pure; do
        [ -d "crates/$crate/src" ] || continue
        hits="$(grep -rn --include='*.rs' -E \
            'SystemTime::now|Instant::now|OffsetDateTime::now|std::(fs|env|net|process)|\bunsafe\b' \
            "crates/$crate/src" 2>/dev/null | grep -vE ':[[:space:]]*(///|//!|//)' || true)"
        if [ -n "$hits" ]; then
            echo "❌ $crate reached for ambient state:" >&2
            echo "$hits" >&2
            fail=1
        fi
    done
    [ "$fail" -eq 0 ] && echo "🧊 pure: no clock, no I/O, no unsafe in the domain crates"
    exit "$fail"

# 🧪 Every test
test:
    cargo test --workspace --all-features

# 🧪 One crate's tests
test-crate crate:
    cargo test -p {{ crate }} --all-features

# 🛡️ Workspace guards: no floats, citations, publishable manifests
guards:
    cargo run -q -p xtask -- check-all

# 📜 Licences and advisories
deny:
    cargo deny check

# 📚 Documentation, warnings as errors
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# 🔒 Minimum supported Rust version
msrv:
    cargo +{{ msrv }} check --workspace --all-features

# `cargo publish` cannot be undone, so the dry run is the cheap half of the
# decision: it packages the publishable crates in dependency order and verifies
# each builds from its own tarball.
#
# 🚢 Everything the release workflow checks, before the tag exists
release-check:
    cargo publish --workspace --locked --dry-run
    @echo "🚢 verified — tag it with: git tag v{{ version }} && git push origin v{{ version }}"

# 🌐 Serve the documentation site locally
site:
    cd site && zola serve

# 🌐 Build the documentation site
site-build:
    cd site && zola build
