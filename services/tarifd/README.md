# tarifd

The tariff publication daemon: decides when a tariff version must reach the driver (OCPP 2.1), roaming partners (OCPI 2.3.0) and the national access point (DATEX II), and reports any price in force that somebody was never shown.

A seller can import the current driver tariff and retrieve its exact fingerprint using the [integration OpenAPI document](../../schemas/openapi.json).

## Status

| | |
|---|---|
| ✅ library | `tarifd::Tarifd` schedules versions by lead time, builds all three payloads at once or none, records confirmed deliveries, and names late versions with the article they breach — tested in `tests/a_version_takes_effect.rs` |
| ✅ estate | publishes the estate's `[[tariffs]]`, built by the same `TariffSpec::to_tariff` `csmsd` rates with, so the published price and the charged price cannot diverge; the party is the estate's operator |
| ✅ `driver-tariff.v1` | `GET /v1/driver-tariffs/{id}` (the version in force) and `/v1/driver-tariffs/{id}/{version}` (that fingerprint while retained in history): public, freshly fetched, a document per version a seller imports instead of typing a price — components with unit and basis, the display, worked totals and the tariff itself; schema `schemas/driver-tariff.v1.json` |
| Delivery | `tarifd` logs due publications. Station delivery belongs to `csmsd` and Mobilithek delivery to `poid`. |

## What it owns

### One price, three audiences

| Audience | Wire | Duty |
|---|---|---|
| the driver at the point, **before** they start | OCPP 2.1 `SetDefaultTariff` | `[AFIR Art. 5(4)]` |
| the roaming partner that settles against it | OCPI 2.3.0 | the roaming contract |
| the national access point | DATEX II | `[AFIR Art. 20(2)(f)]` |

`tarifd` computes no price. Each payload comes from the crate that owns that
crossing — `emob_ocpp::to_ocpp`, `emob_roam::ocpi::tariff::to_ocpi`,
`emob_poi::rate::publish` — from the same `Tariff` that rates the CDR, so the
three cannot drift from the invoice. (OICP pricing products go through
`roamd`, since they need estate facts such as maximum charging power.)

### Only content that rates sessions is published

`Tarifd::prepare` refuses a tariff whose **content** matches no version in its
`TariffHistory`, because only the history rates CDRs:

```rust
tarifd.prepare(&edited, &party, at)?;
// Err: tariff ad-hoc is published by this service and no version of it has the
//      content 9f2c…: publishing it would quote a price no session is rated at
```

Versions are keyed by content fingerprint, not by id: redeploying the same
numbers is the same publication; an edit under the same id is a new one.

### Due versus late

A version takes effect by its own window, and `[AFIR Art. 5(4)]` requires the
price to be known **before** a session starts. Publishing at the moment of
effect is already late for anyone at a point at that instant.

```rust
tarifd.due(now, lead);   // work: takes effect soon, not everyone told yet
tarifd.late(now);        // breach: in force now, and an audience was never told
```

### All three audiences, or none

`prepare` builds every payload before any is sent and fails as a whole. OCPP 2.1
refuses a tariff it cannot state without widening the price against the driver
(an hourly rate with no exact per-minute form, one dimension at two VAT rates);
publishing to the other two anyway would advertise a price the stations do not
charge.

### Recording only confirmed deliveries

`prepare` returns the payloads; the daemon sends; `Tarifd::confirm` records an
accepted delivery per audience. An unconfirmed audience makes the version show
up in `late` as soon as it takes effect.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[parties]`, `[[tariffs]]`, `[[operators]]` |
| `EMOB_OPERATOR_TOKEN`, `EMOB_DESK_READ_TOKEN` | **required** | the example estate’s named operator and background read credentials |
| `TARIFD_HTTP_BIND` | `127.0.0.1:9581` | health/readiness shell |
| `EMOB_HTTP_BIND` | — | estate-wide shell address, used when `TARIFD_HTTP_BIND` is unset; defaults are loopback, unreachable inside a container |
| `RUST_LOG` | `info,tarifd=debug` | log filter |

## Running

```console
source .env    # local estate and credentials from `just dev-keys`
cargo run -p tarifd
```

Or `just up` for the whole estate (`tarifd`'s shell on `localhost:8085`).

## Endpoints

| Path | Answers |
|---|---|
| `GET /v1/tarifd` | operator token: the tariffs it publishes |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the tariff store is loaded; `503` listing `tariffs` until then |
| `GET /about` | name and version |

## Built on

`emob-tariff`, `emob-ocpp`, `emob-roam`, `emob-poi`, `emob-service`. See
[Pricing](https://hupe1980.github.io/emob/docs/pricing/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0
