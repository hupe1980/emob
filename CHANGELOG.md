# Changelog

All notable changes to this workspace. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) — before 1.0 the minor version is the
breaking one. Every publishable crate carries the one workspace version.

Changes are hard cuts: nothing is deprecated first, and a release that removes or
renames something says so under **Removed** or **Changed** instead.

## [Unreleased]

## [0.5.0] - 2026-11-20

The way in is short, and the fleet has a registry, a map and a network.

### Added

- **Station onboarding at fleet scale.** `csmsd` owns a journalled **station
  registry** (`station_registered`, `station_updated`, `station_retired`); the
  estate's `[[stations]]` seed it, after which it rules. Admission reads it on
  every connection, so a station connects the moment it is registered and is
  refused the moment it is retired — no restart. Heute gains **Stationen**
  (search by identity, EVSE, vendor, model, serial, firmware, in pages),
  **Station hinzufügen** (one form; an installer sheet with the password minted
  and shown once) and, on a station's page, a change, a new password or
  retirement, each with a reason; a refusal is marked at the field it names. API: `GET|POST /v1/csmsd/stations`, `PUT /v1/csmsd/stations/{id}`,
  `POST …/{id}/password`, `POST …/{id}/retire`. A search over 10 000 stations
  is one pass under one lock (a test holds it under 50 ms).
- **Where the fleet is.** Stationen draws the search as a map — a dot per
  station by state, or above 300 a grid whose circles open the search narrowed
  to their area (`GET /v1/csmsd/map`, `area=` on the search). Server-rendered
  SVG over a bundled Natural Earth outline: no tiles, no script, no third
  party. A station's location card draws its own locator and warns when the
  coordinates lie outside its country or look swapped. `csmsd` keeps the
  mobile network a station reports at boot (MCC-MNC, never the IMSI); the
  search finds stations by network or carrier, and Heute groups silent
  stations by network — "Telekom Deutschland 12 von 40 still".
- **`emob check estate.toml`** — every section against the whole estate (a
  section no daemon reads is refused), then each daemon's own checks; mistakes
  reported like `rustc` reports them, with the line and `= help: did you mean …`.
  `--secrets` requires every `*_env` variable (named, never printed); `--schema`
  prints the estate's JSON Schema, published as `schemas/estate.schema.json` and
  referenced by `#:schema` in the example so editors complete and check it.
- **The station page.** `csmsd` journals presence as transitions (`online`,
  `offline`, `booted`), what a station says at boot, what its meter signs
  about itself in a billed session, and its firmware and security reports; a
  firmware change is the intervention record `[MessEG §31(2) Nr. 4]` asks for.
  `GET /v1/csmsd/stations/{identity}` and `deskd`'s `/station/{identity}` show
  one station as an asset card; every station on Heute links to it.
- **Three commands to a running estate:** `just dev-keys > .env` (every secret
  the estate names), `just dev-ca`, `just run` (checks the estate, starts the
  nine daemons, waits until each is ready).
- JSON Schemas for `de.emob.cdr.issued` and `de.emob.invoice.issued`
  (`schemas/events/`), held to what the daemons send; `emob subscribers test
  <name>` sends a subscriber one signed, marked sample of each type it takes.
- `cargo xtask check-docs`: every documented `/v1/…` route is served and every
  emob release a document names is tagged. `CONTRIBUTING.md` and issue and pull
  request templates.
- Pages crossfade between each other where the browser supports view
  transitions.

### Changed

- `datex2` 0.3: the status publication is wrapped by its own
  `json::mobilithek::snapshot` and written by the same `to_string` as the
  table; a refused amount names its type (`… of type AmountOfMoney`).
- `sepa` 0.10: every pain.008 states the creation time `emob-billing` passes
  (the crate no longer reads a clock), and `sepa::Iban` masks itself in
  `Debug`, so `Creditor` derives it.
- `de.emob.cdr.issued` carries `data.point` (EVSE, connector, power, location);
  `roamd` builds the CDR location from it, so a station registered after
  `roamd` started still roams. `[desk]` gains `ocpp_url` for the installer
  sheet.
- `csmsd::edge::Gate` takes a `Registry`; `csmsd::read::router` takes whether
  profile 3 can be admitted instead of the estate's stations.
- `poid` refuses to start without `POID_NAP_IDENTIFIER` (was `DE-NAP-0000`).
- `csmsd` acknowledges and journals `FirmwareStatusNotification`,
  `SecurityEventNotification`, `LogStatusNotification` and (1.6)
  `DiagnosticsStatusNotification` (was `NotSupported`).
- `tarifd` logs a version's state once when it starts, not every minute.
- `just dev-keys` writes `.env` lines (with `export`) and includes every
  `*_env` the estate names; `eval "$(just dev-keys)"` still works.
- `ocpp-kit` 0.6: the kit enforces `messages_per_minute` and
  `idle_timeout_secs` and serves profiles 2 and 3 on one listener;
  `csmsd`'s own token bucket, idle sweeper and `rustls` server config are gone
  (`edge::Guarded` removed, `Edge::new` returns a `Result`).
- `location.country` must be ISO 3166-1 alpha-2 (`DE`), as OCPI's
  `country_code` is; a registration with anything else is refused.

### Fixed

- `deskd` answered `503` on `/health/ready` forever (it declared no probe).
- `billd` built on its own (as its image is) had no TLS in its HTTP client, so
  an `https://` operator system or subscriber could not be reached.
- Heute's "weakest points" linked to anchors that existed only for silent
  stations; they open the station page.

## [0.4.0] - 2026-11-20

Events leave the estate, and the Mobilithek feed is written by `datex2`.

### Added

- **CloudEvents everywhere.** Every event emob sends is one structured-mode
  CloudEvent 1.0 (`emob_service::cloudevent`, `source` `urn:emob:<daemon>`),
  signed per Standard Webhooks over the whole envelope.
- **Subscribers.** `[[subscribers]]` (name, `https` URL, `secret_env`, `types`)
  in `csmsd` and `billd` sends `de.emob.cdr.issued` and
  `de.emob.invoice.issued` to systems outside the estate. Checked at start (a
  pattern that admits no emitted type refuses the start), owed at emission,
  retried until a `2xx`, every answer journalled as `subscriber:<name>`; an
  answer accepts or books nothing.
- `emob-poi`: `Station::staffed`, published as the profile's required
  `serviceType`; `PoiError::AmountNotInCents`.

### Changed

- `csmsd` delivers each record to `billd` and `roamd` as a
  `de.emob.cdr.issued` CloudEvent; both refuse any other body (`422`).
- **The invoice to the ERP is a `de.emob.invoice.issued` CloudEvent**: the
  UBL or CII file is `data` (`application/xml`), the subject
  `invoice/<number>`, the page link the `invoiceurl` attribute (was the
  `emob-invoice-url` header). Receivers read the file from `data`.
- `emob-poi` builds both publications from `datex2` 0.2 and writes them with
  its Mobilithek writer, which refuses what the profile's schemas reject.
  Output is compact JSON; `to_json` returns `datex2::Error`;
  `PriceUpdate::price_type` is `rate::PriceType`; `authentication_methods`
  returns `datex2` values; `v2g_literal` is `v2g_type`.
- `emob-poi` publishes amounts in whole cents only: a per-minute price only
  where sixty divides the hourly one into cents (otherwise the hourly figure as
  `other`), and any other amount with a fraction of a cent, such as `0.589`
  per kWh, refuses the feed instead of being rounded.
- Coordinates in the DATEX II feed carry the profile's `Float` precision,
  about seven significant digits (`50.779599` → `50.7796`).

### Removed

- The 30 event types nothing emitted; `emob_service::events` holds the two a
  daemon emits, and a test holds every constant to an emitter.
- `agentd`'s subscription table (`agentd::subscriptions`).
- `PriceType::as_profile_str`, `RatePolicy::as_profile_str` and
  `emob_poi::datex::wire::{Exact, Enumerated, Extended, Multilingual}`;
  `serde_json`'s `raw_value` feature.

### Fixed

- `csmsd` answers `BootNotification` and `Heartbeat` with its own clock, not
  the Unix epoch, so a real station no longer sets its clock to 1970.
- Every DATEX II station carries the `serviceType` the profile requires; the
  Mobilithek's schema rejected the table without it.

### Upgrading

- Let `csmsd`'s outbox drain before upgrading: a delivery still pending from
  0.3 is sent in the old shape and refused by the new `billd` and `roamd`.
- Journals from 0.3 replay.

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

[Unreleased]: https://github.com/hupe1980/emob/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/hupe1980/emob/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/hupe1980/emob/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/hupe1980/emob/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/hupe1980/emob/compare/v0.1.0...v0.3.0
[0.1.0]: https://github.com/hupe1980/emob/releases/tag/v0.1.0
