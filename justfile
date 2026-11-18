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

# The receipt page's verifier: `emob-verify` for `wasm32-unknown-unknown`,
# compiled into `csmsd` with `include_bytes!`. Paths are remapped so the module
# is the same bytes on every machine, and CI rebuilds it and fails on a diff.
#
# 🔏 Build the in-browser verifier and measure it
wasm:
    #!/usr/bin/env bash
    # Built in the Dockerfile's pinned Rust image, at /emob, as linux/amd64, so
    # every machine produces the same bytes: cargo folds a path crate's absolute
    # location into its symbol hashes, and a different checkout path reorders
    # the code. CI rebuilds it the same way and fails on any difference.
    set -euo pipefail
    image="$(sed -n 's/^FROM \(rust:[^ ]*\) AS build$/\1/p' Dockerfile)"
    docker run --rm --platform linux/amd64 -v "$PWD":/emob -w /emob \
        -e CARGO_TARGET_DIR=/emob/target/wasm-build "$image" sh -ec '
            rustup target add wasm32-unknown-unknown >/dev/null
            commit="$(rustc -vV | sed -n "s/commit-hash: //p")"
            sysroot="$(rustc --print sysroot)"
            RUSTFLAGS="--remap-path-prefix=/usr/local/cargo=/cargo --remap-path-prefix=/emob=/emob --remap-path-prefix=$sysroot/lib/rustlib/src/rust=/rustc/$commit" \
                cargo build -q --locked -p emob-verify --target wasm32-unknown-unknown --profile wasm
            mkdir -p services/csmsd/assets
            cp target/wasm-build/wasm32-unknown-unknown/wasm/emob_verify.wasm services/csmsd/assets/emob_verify.wasm'
    raw=$(wc -c < services/csmsd/assets/emob_verify.wasm | tr -d ' ')
    gz=$(gzip -9c services/csmsd/assets/emob_verify.wasm | wc -c | tr -d ' ')
    echo "🔏 services/csmsd/assets/emob_verify.wasm: ${raw} bytes, ${gz} gzipped (budget 200000)"
    [ "$gz" -le 200000 ] || { echo "❌ over the 200 kB budget" >&2; exit 1; }

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

# The nine daemons and the `emob` CLI, from one Dockerfile. `docker-bake.hcl`
# carries the list, so `just images -- --print` is the answer to *what does
# this workspace ship*.
#
# `-f` is not tidiness. `bake` reads `compose.yaml` as a bake definition when it
# is not told otherwise, and that file requires the data keys — correctly, so
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
# The data and signing keys have no default and compose refuses without them:
# a key everybody has seals nothing and vouches for nothing. The listener's certificate is minted on the
# spot when there is none, because csmsd refuses a public bind without TLS.
#
# 🚀 Bring the estate up locally
up:
    #!/usr/bin/env bash
    set -uo pipefail
    for key in CSMSD_DATA_KEY ROAMD_DATA_KEY EMPD_DATA_KEY BILLD_DATA_KEY CSMSD_SIGNING_KEY ROAMD_SIGNING_KEY EMPD_SIGNING_KEY BILLD_SIGNING_KEY POID_SIGNING_KEY; do
        if [ -z "${!key:-}" ]; then
            echo "❌ $key is unset — for a local run:  eval \"\$(just dev-keys)\"" >&2
            exit 1
        fi
    done
    [ -f target/dev-tls/server.pem ] || just dev-ca
    docker compose up --build -d
    echo "🚀 up — probes on :8080…:8087, Heute on :8088, OCPP on wss://localhost:9000"

# A throwaway CA, a loopback server certificate (127.0.0.1, ::1, localhost)
# and profile-3 client certificates for CP-1 and CP-2, with openssl, into
# target/dev-tls/ — what the demo estate's `[csms]` names. Never for a station
# anybody else can reach.
#
# 🔐 Mint the demo's TLS certificates
dev-ca:
    services/csmsd/tests/fixtures/tls/mint.sh target/dev-tls

# One 32-byte data key per daemon that seals what it journals, and one
# Ed25519 seed per journalled daemon that signs where its journal ends, as
# `export` lines: `eval "$(just dev-keys)"`. Losing a data key loses what it
# sealed — for empd that is crypto-erasure on purpose, for csmsd, roamd and
# billd a daemon that will not start; a daemon with another signing key than
# the one its `<journal>.head` was signed under will not start either.
#
# 🔑 Print fresh data and signing keys
dev-keys:
    @for key in CSMSD_DATA_KEY ROAMD_DATA_KEY EMPD_DATA_KEY BILLD_DATA_KEY CSMSD_SIGNING_KEY ROAMD_SIGNING_KEY EMPD_SIGNING_KEY BILLD_SIGNING_KEY POID_SIGNING_KEY; do echo "export $key=$(openssl rand -base64 32)"; done

# The whole estate on loopback in one process: the daemons through their own
# `daemon::run`, three simulated stations on OCPP 1.6 / 2.0.1 / 2.1 over wss://
# (security profile 2), a roaming partner, a seeded morning at 60×. Prints
# Heute (signed in), a receipt and the partner's inbox, and runs until Ctrl-C.
# Every secret is minted for the run and gone with it. `just demo --check`
# plays it at full speed and asserts what it printed — CI does, on every push.
#
# No Rust toolchain? Then it runs the released `ghcr.io/hupe1980/emob` image
# instead (spec 011): on the host's network, because the demo binds loopback
# only and the links it prints are the host's; as you, so the TLS key `just
# dev-ca` wrote stays yours and is mounted read-only.
#
# 🎬 Try it: the estate, three stations and a partner, in one command
demo *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    [ -f target/dev-tls/server.pem ] || just dev-ca
    if command -v cargo >/dev/null; then
        exec cargo run -q -p emob-cli -- demo {{ ARGS }}
    fi
    command -v docker >/dev/null || { echo "❌ neither cargo nor docker — see just doctor" >&2; exit 69; }
    echo "🐳 no cargo — running ghcr.io/hupe1980/emob (Ctrl-C ends it)" >&2
    tty=(); [ -t 0 ] && tty=(-it)
    exec docker run --rm ${tty[@]+"${tty[@]}"} --network host --user "$(id -u):$(id -g)" \
        -v "$PWD/target/dev-tls:/tls:ro" ghcr.io/hupe1980/emob demo --tls /tls {{ ARGS }}

# What this workspace needs, checked before something fails halfway: each
# missing tool on one line with the command that installs it. Exits non-zero
# only for a required one; screenshots, containers and the site are optional.
#
# 🩺 Check the tools this workspace needs
doctor:
    #!/usr/bin/env bash
    set -uo pipefail
    missing=0
    ok()   { printf '  ✓ %-8s %s\n' "$1" "$2"; }
    need() { printf '  ✗ %-8s %s\n' "$1" "$2"; missing=1; }
    may()  { printf '  · %-8s %s\n' "$1" "$2"; }
    want="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
    if command -v rustup >/dev/null; then
        if have="$(rustc --version 2>/dev/null)" && [[ "$have" == "rustc $want"* ]]; then ok rustc "$have"
        else need rustc "toolchain $want is not installed — rustup toolchain install $want --component rustfmt clippy"; fi
    else need rustup "Rust — curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; fi
    command -v cargo >/dev/null && ok cargo "$(cargo --version)" || need cargo "comes with rustup (above)"
    command -v just >/dev/null && ok just "$(just --version)" || need just "cargo install just  (or: brew install just)"
    if command -v openssl >/dev/null; then ok openssl "$(openssl version) — just dev-ca"
    else need openssl "for just dev-ca — brew install openssl  /  apt-get install openssl"; fi
    chrome="${CHROME:-}"
    for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" /usr/bin/google-chrome /usr/bin/google-chrome-stable /usr/bin/chromium /usr/bin/chromium-browser; do
        [ -z "$chrome" ] && [ -x "$c" ] && chrome="$c"
    done
    [ -n "$chrome" ] && ok chrome "$chrome — just screenshots" || may chrome "optional, for just screenshots — https://www.google.com/chrome/ (or set CHROME)"
    if command -v node >/dev/null && [ "$(node -p 'process.versions.node.split(".")[0]')" -ge 22 ]; then ok node "$(node --version) — just screenshots (no npm packages)"
    else may node "optional, ≥ 22 for just screenshots — brew install node  /  https://nodejs.org"; fi
    command -v docker >/dev/null && ok docker "$(docker --version) — just up" || may docker "optional, for just up — https://docs.docker.com/get-docker/"
    command -v zola >/dev/null && ok zola "$(zola --version) — just site" || may zola "optional, for just site — brew install zola"
    if [ "$missing" -eq 0 ]; then echo "🩺 ready — next: just demo"; else echo "🩺 install the ✗ above, then: just demo" >&2; fi
    exit "$missing"

# The README's and the site's screenshots, from the demo at a fixed clock and
# seed: Heute light and dark at 1280 and 390 px, a billed receipt light and
# dark, a refused one, and the receipt after "Selbst prüfen" — WebP from
# Chrome's own encoder, each under 200 kB. Chrome is driven over DevTools by
# `xtask/screenshots.mjs` (Node, no npm). Fails if any page scrolls sideways at
# 320 or 390 px, or shows a token.
#
# 📸 Re-take the README's screenshots
screenshots:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v node >/dev/null || { echo "❌ node is missing — see just doctor" >&2; exit 69; }
    [ -f target/dev-tls/server.pem ] || just dev-ca
    cargo build -q -p emob-cli
    links="$(mktemp -d)/links"
    target/debug/emob demo --clock 2026-06-01T08:00Z --speed max --links "$links" > /dev/null &
    demo=$!
    trap 'kill -INT "$demo" 2>/dev/null; wait "$demo" 2>/dev/null; rm -rf "$(dirname "$links")"' EXIT
    for _ in $(seq 1 240); do
        [ -s "$links" ] && break
        kill -0 "$demo" 2>/dev/null || { echo "❌ the demo stopped — run just demo to see why" >&2; exit 1; }
        sleep 0.25
    done
    node xtask/screenshots.mjs "$links" site/static/img

# Every page a person reads — Heute, the receipt billed, refused and checked,
# the invoice page when billd serves one — in light and dark at 320 and 1280
# px, through Chrome's own accessibility tree and computed styles (no npm, so
# no axe): one h1, ordered headings, every control named, alt text, landmarks,
# a visible focus on every Tab stop, text contrast, 24×24 targets and no
# sideways scroll. Without Node or Chrome it says so and skips — except in CI,
# where a skipped gate would be a passed one (SR-135).
#
# ♿ Check the pages' accessibility
a11y:
    #!/usr/bin/env bash
    set -euo pipefail
    chrome="${CHROME:-}"
    for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" /usr/bin/google-chrome /usr/bin/google-chrome-stable /usr/bin/chromium /usr/bin/chromium-browser; do
        [ -z "$chrome" ] && [ -x "$c" ] && chrome="$c"
    done
    for need in node chrome; do
        if { [ "$need" = node ] && ! command -v node >/dev/null; } || { [ "$need" = chrome ] && [ -z "$chrome" ]; }; then
            if [ -n "${CI:-}" ]; then echo "❌ $need is missing — the accessibility gate cannot run" >&2; exit 69; fi
            echo "♿ skipped: $need is missing — see just doctor"; exit 0
        fi
    done
    [ -f target/dev-tls/server.pem ] || just dev-ca
    cargo build -q -p emob-cli
    links="$(mktemp -d)/links"
    target/debug/emob demo --clock 2026-06-01T08:00Z --speed max --links "$links" > /dev/null &
    demo=$!
    trap 'kill -INT "$demo" 2>/dev/null; wait "$demo" 2>/dev/null; rm -rf "$(dirname "$links")"' EXIT
    for _ in $(seq 1 240); do
        [ -s "$links" ] && break
        kill -0 "$demo" 2>/dev/null || { echo "❌ the demo stopped — run just demo to see why" >&2; exit 1; }
        sleep 0.25
    done
    CHROME="$chrome" node xtask/screenshots.mjs "$links" --a11y

# deskd alone, against the example estate: it reads csmsd, billd and roamd at
# the addresses `[desk]` and `[services]` name, so start those (or `just demo`,
# which runs its own deskd) first. It needs the operator token it signs you in
# with and nothing else; a missing one refuses to start, naming the variable.
#
# 🖥️  Run the operator page (Heute) on http://127.0.0.1:9587/
desk:
    #!/usr/bin/env bash
    set -uo pipefail
    if [ -z "${EMOB_OPERATOR_TOKEN:-}" ]; then
        echo "❌ EMOB_OPERATOR_TOKEN is unset — the page answers nobody without one" >&2
        exit 1
    fi
    EMOB_ESTATE=deploy/estate.example.toml cargo run -q -p deskd

# 🛑 Take it down, keeping the journals
down:
    docker compose down

# 🌐 Serve the documentation site locally
site:
    cd site && zola serve

# 🌐 Build the documentation site
site-build:
    cd site && zola build

# One CycloneDX bill of materials for the whole workspace — every crate and
# daemon the release ships, and everything they pull in. `cargo cyclonedx`
# writes one file per member beside its manifest, which is a directory listing,
# not an answer to *which artefact contains this crate*; so the members are
# merged into one document and the per-member files removed again. `xtask` is
# left out: it is not shipped. The release attaches this file and attests it.
#
# 🧾 Write target/sbom/emob-<version>.cdx.json
sbom:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-cyclonedx >/dev/null || { echo "❌ cargo install --locked cargo-cyclonedx" >&2; exit 1; }
    cargo cyclonedx --format json --all-features --override-filename emob-sbom -q
    trap 'find crates services xtask -maxdepth 2 -name emob-sbom.json -delete' EXIT
    mkdir -p target/sbom
    out="target/sbom/emob-{{ version }}.cdx.json"
    members=($(find crates services -maxdepth 2 -name emob-sbom.json | sort))
    jq -s --arg v "{{ version }}" '{
        bomFormat: "CycloneDX", specVersion: .[0].specVersion, version: 1,
        metadata: { component: { type: "application", "bom-ref": "emob@\($v)", name: "emob", version: $v } },
        components: ([.[].metadata.component] + [.[].components[]] | unique_by(."bom-ref")),
        dependencies: ([{ ref: "emob@\($v)", dependsOn: [.[].metadata.component."bom-ref"] }] + [.[].dependencies[]] | unique_by(.ref))
      }' "${members[@]}" > "$out"
    echo "🧾 $out: ${#members[@]} workspace members, $(jq '.components | length' "$out") components"

# There is no update bot: a pin moves by hand, in a reviewed diff, and
# `cargo xtask check-actions` refuses one that names a tag. This is the list to
# move — every Action and base image, with the release its comment names.
#
# 📌 Every pinned Action and base image
pins:
    @grep -hoE 'uses: [^ ]+@[0-9a-f]{40} # .*' .github/workflows/*.yml | sed 's/uses: //' | sort -u
    @grep -E '^FROM .+@sha256:' Dockerfile | sed 's/^FROM //; s/ AS .*//'
