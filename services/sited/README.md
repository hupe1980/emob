# sited

The site controller for § 14a EnWG: receives a grid operator's power limit over EEBUS, shares it among the site's charge points, and turns the result into OCPP charging profiles — designed to run at the site, next to the grid connection.

## Status

| | |
|---|---|
| ✅ library | `sited::Sited` reads an EEBUS limit write, tracks the LPC state with a watchdog, allocates power, produces OCPP charging profiles, and keeps the log of instructions and answers — tested in `tests/a_setpoint_arrives_over_the_wire.rs` |
| ✅ daemon | starts with an **empty** site (0 kW connection, no devices) and logs the LPC state; `GET /v1/sited` shows that state to the estate's operators |
| ⏳ not wired | the site register, the SHIP/EEBUS session (`eebus` with its `runtime` feature) and the OCPP leg. It has no journal, so the instruction log is in memory only |

## What it owns

Three crates own every rule; `sited` owns **when**, because telling a setpoint
from a silence needs a clock and domain crates have none.

| Crate | Owns |
|---|---|
| `emob-eebus` | the exact limit off SPINE's `ScaledNumber`, and which of `[LPC §2.3.2]`'s five states a silence has put the site in |
| `emob-smart` | what the site may draw, and the minimum it keeps a claim to `[BK6-22-300 Ziff. 4.5]` |
| `emob-ocpp` | the charging profile a station obeys, at a purpose an optimiser cannot outrank `[BK6-22-300 Ziff. 4.6]` |

`Sited::on_limit` reads a write through `emob_eebus::read_limit_write`;
`Sited::allocate` calls `emob_smart::allocate`; `Sited::charging_profiles` calls
`emob_ocpp::to_charging_profiles`. None of them chooses a number.

### Read the site state

`GET /v1/sited` returns the LPC state as a string, the ceiling stated by that state in kW (null for unlimited operation), and the effective site ceiling after applying the connection capacity and allocation floor. An empty site can report `init` with 4.2 kW configured and 0 kW available.

| Observation | What to check |
| --- | --- |
| `init` | After startup, the guard must send a heartbeat followed within 60 seconds by a limit. |
| A timed limit ends | Compare receipt time and duration. A value-only write retains the deadline. |
| `failsafe state` | Check the last heartbeat. Both controlled states enter failsafe at 120 seconds of silence. |
| `unlimited/autonomous` | Failsafe residence elapsed. Check the connection before expecting a new active limit. |

### The evidence § 14a asks for

`[LPC §4.1.5]` makes the **pair** — what the grid operator asked for and what was
answered, refusals included — the operator's evidence. `Sited::log` is that pair,
in order.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[[operators]]` |
| `EMOB_OPERATOR_TOKEN`, `EMOB_DESK_READ_TOKEN` | **required** | the example estate’s named operator and background read credentials |
| `SITED_HTTP_BIND` | `127.0.0.1:9586` | health/readiness shell |
| `EMOB_HTTP_BIND` | — | estate-wide shell address, used when `SITED_HTTP_BIND` is unset; defaults are loopback, unreachable inside a container |
| `SITED_FAILSAFE_KW` | `4.2` | failsafe consumption limit in kW `[LPC-901/1]` — the `[BK6-22-300 Ziff. 4.5]` minimum unless the connection contract says otherwise |
| `RUST_LOG` | `info,sited=debug` | log filter |

The daemon uses a two-hour failsafe residence. The library accepts a configured residence within the LPC range.

## Running

```console
source .env    # local estate and credentials from `just dev-keys`
cargo run -p sited
```

Or `just up` for the whole estate (`sited`'s shell on `localhost:8087`).

## Endpoints

| Path | Answers |
|---|---|
| `GET /v1/sited` | operator token: the LPC state |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the site inventory is loaded; `503` listing `site-inventory` until then |
| `GET /about` | name and version |

## Built on

`emob-eebus`, `emob-smart`, `emob-ocpp`, `emob-service`. See
[Smart charging](https://hupe1980.github.io/emob/docs/smart-charging/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0
