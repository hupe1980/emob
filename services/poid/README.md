# poid

The point-of-interest daemon: publishes a charge point operator's static and dynamic charging data to the national access point (in Germany, the Mobilithek) as DATEX II, as AFIR requires.

## Status

| | |
|---|---|
| ✅ library | `poid::Poid` builds checked snapshot and status publications from `emob-poi`'s DATEX II documents and reports a stale feed; `poid::Ledger` records which snapshot the access point accepted — tested in `tests/a_snapshot_goes_out.rs` |
| ✅ journal | on start the daemon folds its journal (`poid` under `EMOB_JOURNAL_DIR`) and restores the last accepted snapshot, or warns that none was ever accepted. Every record names its actor; the head is signed under `POID_SIGNING_KEY` after every append, and a journal shorter than its signed head refuses the start |
| ✅ estate | the inventory is the estate's `[[stations]]`, one site per `location.id`, with exact decimal coordinates; the publisher country is the operator party's |
| ✅ prices | each point's rate is the version of its station's tariff in force at the moment of publication, from the same history `csmsd` rates with, named `<tariff>-<Preisstand>` |
| ✅ push | with `[mobilithek]` (`url`, `client_cert`, `client_key`), the table goes to the Mobilithek at start and hourly over mutual TLS; only a `2xx` is journalled, and each documented refusal is named — `400` document, `401`/`403` certificate, `404` publication, anything else retried next round (`tests/the_table_reaches_the_access_point.rs`) |
| ⏳ not wired | the dynamic delta (status and ad-hoc price within a minute): the library builds it, the daemon does not send it yet; the inventory is the estate's, not yet `csmsd`'s registry |

## What it owns

`[AFIR Art. 20(2)]` obliges an operator to publish static and dynamic charge
point data free of charge, and `[AFIR Art. 20(3)]` makes the registered API free
and unrestricted. In Germany the format is the DATEX II Recharging profile
`[DATEX-II-Profil]` from 14.04.2026. The documents are `emob-poi`'s; `poid`
decides **when** a snapshot goes out, **which** status updates have accumulated,
and **whether** the access point took it.

### Checked before it is sent

`Feed::check` runs before every publication. Besides structural checks, it
catches a rate published in one time zone at a site in another:

```rust
poid.snapshot(now)
// Err: the rate is published in Europe/Berlin at a site in Europe/Lisbon
```

That document would be well-formed and lawful, with a `22:00` night price
starting an hour later than the driver on site expects.

### Status updates reference an accepted snapshot

A DATEX II status message addresses a facility **at the version** the table
publication gave it, and a consumer cannot resolve a version it never received
— it silently drops the update. So `Poid::status` refuses until a snapshot has
been accepted, and checks references against **the snapshot the access point
holds**, not the current inventory.

### Recording only what was accepted

`snapshot` and `status` return documents; the daemon pushes them; only a push
the access point accepted is recorded (`Poid::accepted`, `Ledger::accepted`).
The accepted snapshot survives a restart, so the daemon neither forgets its
version references nor republishes the whole estate after every restart.

### A stalled feed is named

```rust
poid.stale(now, within);   // Some(…) → route planners are reading a stalled feed
```

Nothing errors when a published feed goes stale; the map is simply wrong. The
daemon checks against a one-hour cadence. `informationStatus` is the same hazard:
a production feed marked `test` is invisible, a test feed marked `real` sends
drivers to a fiction.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[parties]`, `[[stations]]`, `[[operators]]` |
| `EMOB_OPERATOR_TOKEN` | **required** | the variable `[[operators]]` names: the read model's bearer |
| `POID_HTTP_BIND` | `127.0.0.1:9582` | health/readiness shell |
| `EMOB_HTTP_BIND` | — | estate-wide shell address, used when `POID_HTTP_BIND` is unset; defaults are loopback, unreachable inside a container |
| `POID_NAP_IDENTIFIER` | **required** | the identifier the national access point issued this operator, carried by every publication; `poid` refuses to start without it |
| `EMOB_JOURNAL_DIR` | `var` | journal directory — deliberately not a temp directory |
| `POID_SIGNING_KEY` | **required** | 32 random bytes, base64 (an Ed25519 seed); signs the journal head. A `.head` signed under another key refuses the start |
| `EMOB_LOG_FORMAT` | JSON | `text` for a terminal |
| `RUST_LOG` | `info,poid=debug` | log filter |

## Running

```console
EMOB_ESTATE=deploy/estate.example.toml EMOB_OPERATOR_TOKEN=… cargo run -p poid
```

Or `just up` for the whole estate (`poid`'s shell on `localhost:8084`).

## Endpoints

| Path | Answers |
|---|---|
| `GET /v1/poid/head` | operator token: the journal's signed head, `text/plain`, verifier key in `Emob-Verifier-Key`. deskd does not read it; witness it with `emob witness` |
| `GET /v1/poid` | operator token: the points (EVSE, location) and the last accepted snapshot |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the last accepted snapshot is restored; `503` listing `inventory` until then |
| `GET /about` | name and version |

## Built on

`emob-poi` (DATEX II documents and checks), `emob-service` (shell, journal). See
[Locations](https://hupe1980.github.io/emob/docs/locations/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0
