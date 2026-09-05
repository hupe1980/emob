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
# This greps *our* source. The other half — that no dependency drags a clock, a
# socket or a database in behind it — is `cargo xtask check-graph`, under
# `just guards`, because a dependency is not our source and `emob-billing` once
# carried `uuid`/`v7` and therefore `SystemTime::now` (D181).
#
# 🧊 Enforce the "no I/O, no clock" promise of the domain crates
purity:
    #!/usr/bin/env bash
    set -uo pipefail
    pure="emob-core emob-eichrecht emob-session emob-cdr emob-billing emob-tariff emob-thg emob-ocpp emob-poi emob-roam emob-sim"
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

# 🛡️ Workspace guards: no floats, citations, publishable manifests, wire
# spellings, unbroken sentences, self-consistent documents, public functions
# something calls, constructors the wire goes through, clean graphs
guards:
    cargo run -q -p xtask -- check-all

# 📜 Licences and advisories
deny:
    cargo deny check

# 📚 Documentation, warnings as errors
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# The MSRV is a promise to the crates downstream, and it is not one number any
# more: `emob-roam` speaks to `ocpi-kit`, which asks for 1.96, and the crates
# `mako` and `hems` consume still promise 1.94. So this checks that the promise
# holds where it is made rather than asserting one floor for everything.
#
# 🔒 Minimum supported Rust version, where it is promised
msrv:
    cargo +{{ msrv }} check --all-features \
        -p emob-core -p emob-eichrecht -p emob-session -p emob-cdr \
        -p emob-tariff -p emob-ocpp -p emob-poi -p emob-billing -p emob-thg -p emob-service \
        -p emob-sim
    @echo "🔒 the crates that promise {{ msrv }} build on {{ msrv }}"

# `cargo publish` cannot be undone, so the dry run is the cheap half of the
# decision: it packages the publishable crates in dependency order and verifies
# each builds from its own tarball.
#
# 🚢 Everything the release workflow checks, before the tag exists
release-check:
    cargo publish --workspace --locked --dry-run
    @echo "🚢 verified — tag it with: git tag v{{ version }} && git push origin v{{ version }}"

# The social card every link to the site renders as. The SVG is the source —
# a card is text and rules, and a binary is not reviewable — and Open Graph
# consumers take no SVG, so the PNG beside it is the deliverable.
#
# 🖼️  Re-render the site's social preview image
og:
    rsvg-convert -w 1200 -h 630 -f png -o site/static/og.png site/og.svg
    @echo "🖼️  site/static/og.png re-rendered from site/og.svg"

# 🌐 Serve the documentation site locally
site:
    cd site && zola serve

# 🌐 Build the documentation site
site-build:
    cd site && zola build
