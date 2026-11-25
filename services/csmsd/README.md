# csmsd

The CSMS (charging station management system) daemon: an OCPP 1.6J / 2.0.1 / 2.1 WebSocket endpoint whose transaction events feed the Eichrecht billing chain.

## Status

| | |
|---|---|
| ✅ estate | stations (OCPP version, security profile, password hash), meter keys with their provenance, and tariffs come from `estate.toml`. An empty key registry keeps `/health/ready` at `503` naming `key-registry`, because every signed session would be refused |
| ✅ journal | the CDR ledger, outcomes, in-flight transaction events, the OCPP 1.6 transaction counter, every authorisation answer and the outbox are a fold of `csmsd.ndjson` — a restart loses nothing. Record bodies are sealed under `CSMSD_DATA_KEY`, so the card UIDs they carry are not readable on disk. Every record names its actor (`station:<identity>` for what a station sent); the head is signed under `CSMSD_SIGNING_KEY` after every append, and a journal shorter than its signed head refuses the start |
| ✅ authorisation | `Authorize` and every transaction start are asked of `empd` (`POST /v1/authorize`, service token). A timeout, an unreachable `empd` or a non-`200` answers `Invalid` with the reason journalled — fail closed; a refused start opens no transaction |
| ✅ outbox | each settled session with a provider is a `de.emob.cdr.issued` CloudEvent, delivered to `billd` and `roamd` (`POST /v1/deliveries`) and to every `[[subscribers]]` entry that takes it, signed per Standard Webhooks (a subscriber with its own secret), retried every `retry_secs`, recorded delivered only after a `2xx`; a final `4xx` is recorded as refused |
| ✅ stations known | presence as transitions (`online`, `offline` after silence past the heartbeat, found by a sweep every minute, `booted`), what each station says at boot (vendor, model, serial, firmware), what its meter signs about itself in a billed session's records, and its `FirmwareStatusNotification` and `SecurityEventNotification` — each journalled once, on change. A firmware change is the intervention record `[MessEG §31(2) Nr. 4]` asks for |
| ✅ station edge | OCPP security profiles 1–3 with TLS, a connection cap, a per-connection message budget and an idle timeout (below) |
| ✅ receipt | with receipts enabled, a recorded session gets a driver receipt at `<public_url>/r/<token>` (below), optionally shown on the station's own display |

`tests/a_station_connects.rs` runs a real `ocpp-kit` station against a real
`ocpp-kit` CSMS over a TCP WebSocket with `csmsd` as the handler. The station
sends the Open Charge Alliance's published `StopTransaction.req` from a DZG
GSH01.1K2L meter: its `meterStop` is the lifetime register, 108814 Wh; the CDR
bills **0.636 kWh**, the signed transaction difference.

## What it owns

### Two ledgers, kept apart by type

| Question | Answered by | Meter values |
|---|---|---|
| Did the traffic arrive? (every event, sequence complete, retries recognised) | `ocpp-kit`'s CSMS ledger, fed from its version-neutral `observe` view | numeric register (`f64`) — fine for telemetry |
| What may be billed? | `emob-ocpp` → `emob-eichrecht` → `emob-cdr` | signed OCMF register, exact decimal |

Both run side by side. Nothing in the billing path can see an `f64`, because
`emob_ocpp::TransactionEvent` has no field for one.

### One billing path for three OCPP versions

`observe_v16`, `observe_v201` and `observe_v21` produce the same `Observed`, so
billing is one function for all versions. The one protocol detail the daemon
owns is the OCPP 1.6 transaction id, which the CSMS assigns in the
`StartTransaction` response. Every rule that could be *wrong* — where 1.6 puts
signed data, clock alignment, `chargingState`, retries — lives in `emob-ocpp`
and is tested against the Open Charge Alliance's example messages.

A station that declares `format: SignedData` but sends an unparseable document
would otherwise look like one sending no signed data; that transaction is
refused by name on the first session. The RPC is still answered, so the station
does not retry the same broken payload.

### Two bindings a station cannot supply itself

- **Identity → charge point.** `Provisioning` maps the WebSocket identity to a
  `ChargePoint` (EVSE id, rated power) once, at authentication, and the binding
  travels with the connection. An unknown identity gets **404**, not 401, so an
  operator can tell a typo from a bad password `[OCPP 2.0.1 Part 4 §3.1.1]`.
- **Meter → public key.** The key that verifies OCMF comes from the
  `KeyRegistry`, out of band `[OCMF §Relation of Serial Numbers]`, never from
  the station's own `publicKey` claim `[OCA SMV §3.2.2]` or `MeterPublicKey`
  configuration key `[OCA SMV §3.3.1]`.

### Every transaction has an outcome

| `Outcome` | Meaning |
|---|---|
| `Settled` | a CDR was produced |
| `AlreadySettled` | the ledger already holds it — an OCPP retry, counted as success, not twice |
| `Refused` | the chain's reasons, in order (including a *different* record under a settled key) |
| `KeyMismatch` | the station signs with a key it was not provisioned with — likely a swapped meter; the chain refuses the session anyway |
| `UnreadableClaimedKey` | the claimed key does not decode, so the comparison cannot run; the session still bills |
| `AfterTheEnd` | the station sent data after its own `Ended` event |

The tariff is checked against the point's power **before** pricing, because
`[AFIR Art. 5(4)]` constrains the pairing of tariff and charge point.

### The station edge: security profiles 1–3

Each `[[stations]]` entry names its `security_profile` `[OCPP 2.0.1 Part 2 §A00]`:

| Profile | The station proves who it is with | csmsd checks |
|---|---|---|
| 1 | HTTP Basic on a plain socket | the password against an Argon2id `password_hash`, verified off the async runtime |
| 2 | HTTP Basic over TLS | the same; refused on a plain socket unless `[csms] tls_terminated_by_proxy = true` |
| 3 | a TLS client certificate | the chain to `[csms] client_ca` and a CN equal to the identity; refused at start behind a proxy |

An unknown identity gets **404**, a wrong credential **401** (logged with the
identity), a connection over `max_connections` **503**. csmsd offers 1.6,
2.0.1 and 2.1 and pins each station to its provisioned version, so one that
also speaks a newer one speaks its own; one that does not offer it is
refused. Each connection has a budget of
`messages_per_minute` (excess answered with `SecurityError`, not processed) and
an `idle_timeout_secs`; the counters are on the running daemon's edge. A second
connection under a known identity supersedes the first — the credential check
is what tells an impostor from a reconnect.

A non-loopback `CSMSD_BIND` without `[csms] tls_cert` (or a declared
TLS-terminating proxy) refuses to start, naming `csms.tls_cert`. Print a
station's `password_hash` with:

```console
printf '%s' "$PASSWORD" | cargo run -q -p csmsd -- hash-password 2.0.1
```

### The driver receipt

A transaction ID is scoped to its station. A new charge needs a fresh ID. When a station reuses the ID of an already closed refusal, the desk records a separate refusal. The old receipt still opens only its original evidence, and the repeated ID creates no charge or delivery.

Every fresh transaction csmsd settles or refuses at settlement has a receipt at
`<public_url>/r/<token>` (`[receipts] public_url` in the estate). It leads with
the verdict — *Gemessen, signiert, geprüft*, or *Nicht abgerechnet* with the
paragraph that refused it — then the energy, the price lines from the CDR, the
meter key in groups of four to compare with the charge-point label, the
S.A.F.E. transparency file, and **Selbst prüfen**: a WebAssembly build of
`emob-verify` that checks every signed value on the phone, offline once loaded
`[MessEG §33]`.

- The token is an HMAC under `receipt.key`, created beside the journal on first
  start and kept out of it; only its SHA-256 is stored.
- An unknown token gets the same `404` as any other path; no token is logged; no
  contract id is shown; beyond 60 requests per second the answer is `429`.
- The verifier is compiled in (`services/csmsd/assets/emob_verify.wasm`);
  `just wasm` rebuilds it reproducibly and CI fails on a diff.
- Each invoice line billd issues carries the receipt link. The receipt does not
  link back: the invoice is the partner's whole month, every driver on it.

### The receipt link on the station's display

A station with `receipt_display_minutes` (1–15) in its `[[stations]]` entry is
sent one `SetDisplayMessage` `[OCPP 2.0.1 Part 2 §O]` when a receipt is filed:
`Beleg: <link>`, in German, shown in front until now plus those minutes. It is
**off by default** because a screen is public — whoever stands there next can
read the link — which is also why the window is bounded and the receipt it
opens shows no name, card or contract.

The message is a courtesy, not a fact: one attempt with a 10-second timeout,
after the receipt is journalled; never retried, never journalled, and logged
with the station and the outcome only, never the link. OCPP 1.6 has no display
message, so a 1.6 station with the option is named once at start-up and
skipped; a value outside 1–15 refuses the start, naming the key.

### Graceful shutdown

On `SIGTERM` the shell stops reporting ready first, so the orchestrator routes
no new station here, then drains for 15 seconds so an in-flight
`StopTransaction` — the message carrying the signed meter record — can land.
Killing the process instead loses nothing the journal holds: every accepted
event is recorded before the station hears back.

## Configuration

`main.rs` is configuration only; everything that runs is `csmsd::daemon::run`.

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | the estate: `[[stations]]`, `[[meter_keys]]`, `[[tariffs]]`, `[csms]`, `[receipts]`, `[services]`, `[[operators]]` |
| `CSMSD_DATA_KEY` | **required** | 32 bytes, base64; seals the journal. Under another key csmsd refuses to start, naming the key id |
| `CSMSD_SIGNING_KEY` | **required** | 32 random bytes, base64 (an Ed25519 seed); signs the journal head. A `.head` signed under another key refuses the start |
| `EMOB_WEBHOOK_SECRET`, `EMOB_SERVICE_TOKEN`, `EMOB_OPERATOR_TOKEN`, `EMOB_DESK_READ_TOKEN` | **required** | the variables the estate names: delivery signatures, the token for `empd`, the example estate’s named operator and background read credentials |
| `CSMSD_BIND` | `127.0.0.1:9000` | the OCPP WebSocket stations dial |
| `CSMSD_HTTP_BIND` / `EMOB_HTTP_BIND` | `127.0.0.1:9001` | the shell: probes, read model, receipts |
| `EMOB_JOURNAL_DIR` | `var` | where `csmsd.ndjson`, its `.head` and `receipt.key` live |
| `EMOB_LOG_FORMAT` | JSON | `text` for a terminal. Each OCPP call's span carries `station:<identity>` |
| `RUST_LOG` | `info,csmsd=debug` | log filter |

## Running

```console
just demo                                   # the whole estate, csmsd included
eval "$(just dev-keys)" && just dev-ca      # data keys and loopback TLS
EMOB_ESTATE=deploy/estate.example.toml cargo run -p csmsd   # plus the *_env variables
```

In the example estate `CP-1` is a profile-2 station whose `AuthorizationKey` is
`6d6f636b2d73746174696f6e2d736563726574` (hex of `mock-station-secret`); it dials
`wss://127.0.0.1:9000/CP-1`.

## Endpoints

| Endpoint | Answers |
|---|---|
| OCPP WebSocket on `CSMSD_BIND` | OCPP 2.1, 2.0.1 and 1.6J |
| `GET /v1/csmsd/stations?q=&area=&limit=&offset=` | operator token: the registry searched — every word in a station's identity, EVSE id, OCPP version, state, vendor, model, firmware, network or carrier; `{ total, items }` |
| `POST /v1/csmsd/stations`, `PUT …/<identity>`, `POST …/<identity>/password`, `POST …/<identity>/retire` | operator token: register (the password in this answer only), change, mint a new password, retire — each with a reason, journalled under the operator; a refusal is `422` with `{ field, reason }` |
| `GET /v1/csmsd/stations/<identity>` | operator token: one station — estate entry, presence, what it said at boot, meters with the current registered curve and complete SEC1 point in hexadecimal, firmware history, reports, boots in 7 days, success today and over 7 days, last 20 sessions with receipt paths. An identity the estate does not list is an empty `404` |
| `POST /v1/csmsd/stations/<identity>/notes`, `POST …/notes/<number>/remove` | operator token: a note (`{text}`, 1–500 characters) under the operator, answered `{number}`; removing one is a fact too. The newest ten per station are in the station view, the newest on its silent row, all of them in the search |
| `POST /v1/csmsd/stations/<identity>/reservations`, `POST …/reservations/<number>/cancel` | operator token: hold the point for a card (`{uid, minutes, reason}`, 5–1440 min), answered `201 {number, until}` once the provider accepts the card and the station `ReserveNow`; else `422` with the station's word. While held, every other card is `Invalid`; the card is journalled as the provider's reference, never its UID |
| `GET /p/{evse}`, `GET /p/{evse}.atom` | public: the point's price — the version a session starting now is rated with, components in `[AFIR Art. 5(4)]`'s order, three worked totals, the Preisstand, announced versions — and every version as Atom. Served with `no-store` and language negotiation, `noindex`, rate-limited; a private or unknown EVSE is an empty `404` |
| `GET /v1/csmsd/map?q=&area=` | operator token: the map of a station search — `points` (identity, coordinates, presence) up to 300 matches, else `cells` of a 16 × 16 grid over their extent with station and silent counts; `area=west,south,east,north` in degrees narrows it, anything else is `400` |
| `GET /v1/csmsd/sessions/{position}` | Operator-only evidence for the decision linked by Heute or a station’s recent sessions. Shows the station’s first start and last end frames if retained, the CSMS decision time, observed and missing frame counts, the provider’s keyed card reference when known, and the receipt issued for that decision. A retired station’s history remains readable. |
| `POST /v1/csmsd/sessions/search` | Operator-only exact lookup. Send JSON with `transaction`, optional `before` and `limit` (1–200). Each match names its station and stable position. Results use `{total, items, older}`. A later decision can reuse the same local transaction reference. |
| `GET /v1/csmsd/today?limit=&refused=&silent=` | operator token: registry size, the charging success rate for **today** and the **last 7 days** in `[desk] time_zone` (each with causes and weakest points), seven dated daily rates, refused sessions with typed, cited `findings` and a stable decision position, silent stations with their mobile network, and `networks` (silent and total stations per network with a silent one) — each list `{ total, items, older }` — the newest `limit` (default 50, max 200; anything else is `400`), with `older` the cursor for the next page. Its size depends on `limit` and the number of stations, never on the history |
| `GET /v1/csmsd/head` | operator token: the journal's signed head, `text/plain`, verifier key in `Emob-Verifier-Key` |
| `GET /r/{token}`, `GET /r/{token}/transparenz.xml` | the driver receipt and its transparency file |
| `GET /r/verify.js`, `GET /r/verify.wasm` | the in-browser verifier |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once everything it waits for is loaded; `503` naming what is missing |
| `GET /about` | name and version |

## Built on

`emob-ocpp`, `emob-eichrecht`, `emob-session`, `emob-tariff`, `emob-cdr`,
`emob-service`, `emob-verify` (the receipt's verifier), and [`ocpp-kit`](https://crates.io/crates/ocpp-kit) 0.7 for
transport and the operational ledger. See
[The OCPP seam](https://hupe1980.github.io/emob/docs/ocpp/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0

The operator-only `GET /v1/csmsd/public-points` serves publicly accessible registry entries and timestamped connector observations. Use the returned revision for every following page. Restart the read on conflict.

### Public provider prices

At a publicly accessible EVSE the price page lists the provider plans explicitly published for that point. A driver can open `GET /p/{evse}/providers/{party}/{plan}` without a card or login, and save `GET /p/offers/{reference}` to reproduce the published terms after a change. The page distinguishes the operator's gross price, the provider's energy and session fees, the e-roaming fee and the monthly subscription. The worked example assumes 10 kWh over one hour at 10 kW. A session spanning an upcoming change has no quoted total. A saved reference attests to the public terms; an individual page view is never journalled.

The provider can read the same current terms as JSON at `GET /v1/csmsd/retail-offers/{point}/{provider}/{plan}`. The response pairs the full archive reference with the point, operator tariff, provider fee schedule and effective bounds. Amounts use decimal strings. A withdrawn or private point returns 404. An eligible point with a gap in its published prices returns 503. A caller that binds an agreement to this offer must check the current reference again before acceptance. The archived driver page remains available for inspecting the original terms.


Use the `[[public_plans]]` and `[[public_plans.schedules]]` example in [the estate](../../deploy/estate.example.toml). Each public code must denote the same terms for every contract with that code. Points are opt-in. A point that is private or retired has no current quote. A configured plan with a coverage gap or an unsupported tariff returns 503. Codes and fee versions admit 1–40 lowercase ASCII letters, digits and hyphens. Effective intervals are half-open. Publishing a later start supersedes an open-ended fee version. An explicit end can leave a gap. Reusing a published fee version with different terms refuses startup. Gross EUR terms with a common known VAT rate, unrestricted energy/time/session components and unit steps are eligible. A non-loopback public origin requires HTTPS and deployment TLS termination. The public routes have their own 60 requests/second budget.
