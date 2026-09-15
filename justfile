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
    pure="emob-core emob-eichrecht emob-session emob-cdr emob-billing emob-tariff emob-thg emob-ocpp emob-poi emob-roam emob-sim emob-smart emob-eebus emob-pnc"
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
# something calls, documented APIs the code still has, constructors the wire
# goes through, clean graphs
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
        -p emob-sim -p emob-smart -p emob-eebus -p emob-pnc
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

# The certificates `emob-pnc`'s tests validate paths against. Minted by OpenSSL
# rather than by anything here, because a path validation checked only against
# DER this repository produced is checked against its own opinions — and with
# absolute validity windows, so nothing rots.
#
# 🔑 Re-mint the Plug & Charge test PKI
pnc-fixtures:
    cd crates/emob-pnc && ./scripts/make-test-pki.sh
    cargo fmt -p emob-pnc

# The eight daemons, from one Dockerfile. `docker-bake.hcl` carries the list, so
# `just images -- --print` is the answer to *what does this workspace ship*.
#
# `-f` is not tidiness. `bake` reads `compose.yaml` as a bake definition when it
# is not told otherwise, and that file requires `EMPD_TOKEN_KEY` — correctly, so
# that a missing secret refuses the estate rather than crash-looping one
# container. Building an **image** needs no secret, and a build that demanded
# one would teach an operator to set a throwaway value for the run that matters.
#
# 📦 Build every daemon image
images *ARGS:
    docker buildx bake -f docker-bake.hcl {{ ARGS }}

# The estate on one machine, with a volume per journal. A daemon whose journal
# is not on a volume is one whose ledger dies with the container, which is the
# whole failure the journal exists to prevent.
#
# 🚀 Bring the estate up locally
#
# `EMPD_TOKEN_KEY` has no default and compose refuses without it: a token
# reference is a keyed digest of an RFID uid, and a key everybody has is a store
# whose digests anybody can recompute.
up:
    #!/usr/bin/env bash
    set -uo pipefail
    if [ -z "${EMPD_TOKEN_KEY:-}" ]; then
        echo "❌ EMPD_TOKEN_KEY is unset — the token store is keyed, and a default key is a store whose digests anybody can recompute." >&2
        echo "   for a local run:  export EMPD_TOKEN_KEY=\$(openssl rand -hex 32)" >&2
        exit 1
    fi
    docker compose up --build -d
    echo "🚀 up — probes on :8080…:8087, OCPP on :9000"

# 🛑 Take it down, keeping the journals
down:
    docker compose down

# 🌐 Serve the documentation site locally
site:
    cd site && zola serve

# 🌐 Build the documentation site
site-build:
    cd site && zola build
