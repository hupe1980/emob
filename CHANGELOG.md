# Changelog

All notable changes to this workspace. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) — before 1.0 the minor version is the
breaking one. Every publishable crate carries the one workspace version.

Changes are hard cuts: nothing is deprecated first, and a release that removes or
renames something says so under **Removed** or **Changed** instead.

## [Unreleased]

## [0.3.1] - 2026-11-18

The first release published through crates.io trusted publishing. The crates'
code is unchanged from 0.3.0; this release completes what 0.3.0's release run
could not.

### Fixed

- The release workflow packages only the published crates, so the GitHub
  release carries its SBOM, checksums and build provenance.
- Container images build in parallel without two builds unpacking the same
  crate into the shared cargo cache.
- The receipt verifier (WebAssembly) is built in the pinned Rust container, so
  every machine produces the same bytes and CI can hold the committed module
  to its source.
- `just a11y` starts headless Chrome reliably on CI runners.

## [0.3.0] - 2026-11-18

### Added

- **The journal you can trust.** Every journal record carries `actor` beside
  `kind`, inside the digest: `station:<identity>`, `partner:<party>`,
  `operator:<name>`, `service:<daemon>`, `timer` or `estate`, set from the
  authenticated caller (`emob_service::Actor`).
- **Signed journal heads.** `csmsd`, `billd`, `roamd`, `empd` and `poid` sign
  their head (position and hash) after every append with Ed25519, as a C2SP
  signed note in `<journal>.head` (`emob_service::head`), and serve it at
  `GET /v1/<name>/head` (operator token; verifier key in `Emob-Verifier-Key`).
  A daemon refuses to start from a journal shorter than its signed head,
  diverging at its position, more than one record past it, or longer than one
  record with no head — naming both positions.
- **The witness.** `emob witness [--file PATH] <url>...` records every head in a
  chained `witness.ndjson`, pins each key at first sight and exits `1` on a
  regression, fork or rekey, now or on record. Heute gains a **Journale**
  section: `deskd` witnesses `csmsd`, `billd`, `roamd` and `empd` every 30
  seconds, and an alarm stays red until the witness file is moved aside. New
  optional `[witness] keys` estate section (`emob_service::witness`).
- **JSON logs with request ids.** Daemons log JSON by default
  (`EMOB_LOG_FORMAT=text` for a terminal). Every HTTP request has a span with
  its id (inbound `x-request-id`, or minted, echoed in the response), method,
  matched route and actor; `csmsd` puts `station:<identity>` on each OCPP call.
  `emob demo --check` fails on a log line holding a token, secret, key,
  password or card UID.
- **The MiSpeL month** (`emob_session::mispel`): the Bundesnetzagentur's
  Abgrenzungsoption per calendar month or Rumpfmonat on quarter-hour values
  (Anlage 1 A2 and A11, levy side), the Pauschaloption per year (Anlage 2 P2,
  factor 0,2) refused until the state-aid approval, the Tenor window (in effect
  21.10.2026, operator consent until 30.09.2027), and refusals by name for an
  uncovered or interpolated discharge, an incomplete grid series and a
  mid-month switch. Beispielrechnungen 1–2 reproduce to the digit.
- Two calendar entitlements, `mispel-metering-concept` and
  `mispel-operator-consent` (51 entries now), and
  `ChargePointProfile::mispel` (`MispelPosture`, `Formelsatz`).
- **ISO 15118-20 contract validation under named roots.**
  `emob_pnc::Generation::{Iso2, Iso20}`, chosen per call; -20 chains on
  secp521r1; Ed448 refused by name (`PncError::UnsupportedAlgorithm`);
  `fixtures::iso20` with three test roots named for the European ecosystems;
  `scripts/make-test-pki.sh [all|iso2|iso20]`.
- **The invoice reaches its recipient.** Every issued invoice has a page at
  `/i/{token}` on `billd` with its UBL and CII files (XRechnung or EN 16931),
  under a capability token keyed by `invoice.key`. With `[billing.delivery]`,
  `billd` posts each issued invoice to the operator's ERP or Peppol provider as
  a signed Standard Webhooks delivery and books it once accepted. New estate
  keys: `[billing] iban`, `creditor_id`, `contact`, `public_url`, `delivery`;
  `[[partners]] specification`, `buyer_reference`, `mandate`;
  `electronic_address` on both parties. `emob demo` prints the `Invoice` link,
  and `--check` validates both files.
- **Accessibility checked on every page.** `just a11y` checks Heute, the receipt
  (billed, refused, checked) and the invoice page in light and dark at 320 and
  1280 px through Chrome's accessibility tree — one h1, ordered headings, named
  controls, alt text, landmarks, visible focus, contrast, 24×24 targets, no
  overflow — and runs in CI with zero violations.
- **The receipt on the station's display.** `[[stations]]
  receipt_display_minutes` (1–15, off when absent): `csmsd` sends
  `SetDisplayMessage` "Beleg: <link>" to an OCPP 2.0.1 / 2.1 station once per
  receipt.
- **The `emob` image.** `ghcr.io/hupe1980/emob` is built and pushed with the
  daemon images; without `cargo`, `just demo` runs it with Docker (from the
  first release that publishes it).
- **The estate runs.** Every daemon starts from one `estate.toml`
  (`EMOB_ESTATE`; `deploy/estate.example.toml` for loopback,
  `deploy/estate.compose.toml` for compose). Secrets are environment variables
  the file names (`*_env`); a daemon refuses to start naming the bad key or the
  unset variable.
- `csmsd` folds its CDR ledger, outcomes, in-flight transactions, authorisation
  answers and outbox from its journal; asks `empd` (`POST /v1/authorize`) on
  `Authorize` and at every transaction start, failing closed; and delivers each
  settled session to `billd` and `roamd` as a signed webhook, retried until
  acknowledged.
- `billd` receives CDRs (`POST /v1/deliveries`) and closes one EN 16931 invoice
  per partner and calendar month, once.
- `roamd` mounts the inbound OCPI router (`/ocpi`: versions, credentials, CDRs)
  behind a reach-back guard, registers the estate's partners with their
  Token A, and consigns delivered CDRs with the verbatim OCMF records.
- An operator read model on every daemon (`GET /v1/<daemon>`, operator bearer
  token).
- **Try it in one command.** `emob demo` / `just demo`: the estate on loopback
  with three simulated stations (OCPP 1.6, 2.0.1, 2.1 over `wss://`, security
  profile 2), an OCPI partner and a seeded morning; prints Heute, a receipt and
  the partner's inbox. `--check` is CI's end-to-end test.
- `just doctor` (the tools this workspace needs, and how to install them),
  `just screenshots` (the README's and site's screenshots, from the demo at a
  fixed clock and seed), `just dev-ca`, `just dev-keys`, `just wasm`.
- **A receipt the driver can check.** Every settled or refused session has a
  page at `<public_url>/r/<token>`: the verdict with its citation, the energy,
  the price lines, the meter key, the S.A.F.E. transparency file and *Selbst
  prüfen* — a WebAssembly verifier that checks every signed value on the phone.
  New `[receipts]` estate section. Invoice lines carry the receipt link.
- `emob-verify` (one OCMF record, one key, one verdict; native and C-ABI wasm)
  and `emob-cli` (`emob verify <transparency-file>`, `emob demo`).
- **Heute**, the operator's page (`deskd`, ninth daemon and image): the
  charging success rate and its causes, refused sessions with "Warum?" — the
  evidence findings, worded and cited — silent stations, records a partner has
  not taken, invoices awaiting acceptance, and the next 90 days' duties. New
  `[desk]` estate section; `just desk`.
- `emob_session::success` (the charging success rate from the ledger that
  bills) and `emob_eichrecht::findings` (typed, cited evidence findings).
- **Safe to expose.** OCPP security profiles 1–3 per station: Basic against an
  Argon2id `password_hash`, TLS, client certificates chained to `client_ca`;
  connection cap, per-connection message budget and idle timeout (`[csms]`
  estate section); `csmsd hash-password`. A non-loopback OCPP bind without TLS
  refuses to start.
- `emob_service::vault`: one data key per daemon (`CSMSD_DATA_KEY`,
  `ROAMD_DATA_KEY`, `EMPD_DATA_KEY`, `BILLD_DATA_KEY`); journals sealed with
  AES-256-GCM while the hash chain stays verifiable without the key; tokens a
  daemon only recognises are journalled as keyed digests.
- `SECURITY.md`: GitHub private vulnerability reporting as the disclosure route,
  supported versions, response times; `/.well-known/security.txt` (RFC 9116) on
  the site points to the same route.
- `THREAT_MODEL.md`: the public summary of the system threat model — assets,
  trust boundaries, threats with their status and evidence, and the open risks.
- Release evidence: one CycloneDX SBOM of the workspace (`just sbom`),
  `SHA256SUMS`, and a build-provenance attestation over every `.crate` and the
  SBOM, attached to each GitHub release.
- The nine daemon images are built from `docker-bake.hcl` and pushed to
  `ghcr.io/hupe1980/emob-<daemon>` on every release tag, with BuildKit
  provenance.
- `cargo xtask check-actions` (in `check-all`): every `uses:` is pinned by commit
  SHA, every `FROM` by digest, and every workflow declares its token
  `permissions:`. `just pins` lists what to move by hand.
- `CHANGELOG.md` (this file).

### Changed

- **Journals written before 0.3 do not replay**: every record now needs an
  actor. Start from empty journals; no migration.
- Every journalled daemon requires its signing key — `CSMSD_SIGNING_KEY`,
  `ROAMD_SIGNING_KEY`, `EMPD_SIGNING_KEY`, `BILLD_SIGNING_KEY`,
  `POID_SIGNING_KEY` (32 random bytes, base64). `just dev-keys` prints them;
  `just up` and compose refuse without them.
- `[[operators]] name` is the actor: 1–64 of `[A-Za-z0-9*._:=+|@ -]`, refused at
  start otherwise.
- `emob_service::journal::Folded::record` takes the actor;
  `emob_service::telemetry::init` takes no format argument (the format comes
  from `EMOB_LOG_FORMAT`).
- `emob_pnc::PoolRegistry::validate_in`, `verify_contract` and
  `verify_provisioning` take a `Generation`; `VerifiedContract` and
  `VerifiedProvisioning` carry it.
- `agentd` starts specialists by capability (`skills::*::CAPABILITY`), and
  `agentd::runtime` takes the advisory principal and acts as its delegation
  chain; a run outside it is refused at admission.
- `billd` refuses to close a month whose document the partner's specification
  rejects.
- `deskd` keeps `witness.ndjson` and needs a volume (`deskd:/var/lib/emob` in
  compose).
- The receipt verdict is the page's `h1`; larger touch targets on Heute and the
  receipt. Screenshots retaken.
- Releases publish with crates.io **trusted publishing** (OIDC); the
  `CARGO_REGISTRY_TOKEN` secret is no longer read and should be deleted. Each
  crate needs its trusted publisher configured once on crates.io (repository
  `emob`, workflow `release.yml`, environment `crates-io`).
- Every GitHub Action is pinned by commit SHA with its release as a comment, and
  both base images by digest. Actions moved to their current majors
  (`actions/checkout` v7, `upload-pages-artifact` v5, `deploy-pages` v5, …).
- Workflow tokens are least-privilege: read-only by default, write scopes only
  on the job that writes.
- Images are built for `linux/amd64` only.
- Dependencies upgraded as hard cuts, with no compatibility shims: `jiff` 1,
  `ocpp-kit` 0.5, `iso15118` 0.9, `eebus` 0.11, `en16931`/`en16931-formats`
  0.10, `sepa` 0.9, `doubleentry` 0.9, `agentplane` 0.61.
- `billd`'s ledger runs on `doubleentry` 0.9.
- `[[stations]] password_env` is replaced by `security_profile` (required) and
  `password_hash`; `EMPD_TOKEN_KEY` by `EMPD_DATA_KEY`. Journals written by 0.1
  are not readable by 0.2: start from empty journals.
- `roamd::Node::open` takes the data key; `Dispatcher::deliver` takes
  `(registration, key, document)`. `empd::Empd::new` takes a `DataKey`.
- `Debug` of `Mandate`, `Creditor` and `PaymentDetails` masks IBANs and names.
- `deskd::Options` takes its clock (`deskd::system_clock()` in `main`).
- Feature specifications are written with Spec Kit, each with a threat model.

### Security

- `cargo deny` is green again: RUSTSEC-2026-0387 (`crypto-bigint` 0.5, timing
  variability, via the RustCrypto 0.13 generation `ocmf` pins) and
  RUSTSEC-2026-0543 (`chrono` unmaintained, via `cedar-policy`) are ignored in
  `deny.toml` with the exposure analysis, the upstream and a review date
  (2027-03-01). emob verifies signatures over public data; only `emob-sim` and
  test fixtures sign.

## [0.1.0] - 2026-09-01

First release: fifteen crates from the signed meter value to the ledger posting,
and eight daemons.

[Unreleased]: https://github.com/hupe1980/emob/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/hupe1980/emob/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/hupe1980/emob/compare/v0.1.0...v0.3.0
[0.1.0]: https://github.com/hupe1980/emob/releases/tag/v0.1.0
