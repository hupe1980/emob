# agentd

The advisory daemon: deterministic specialists, run on [`agentplane`](https://github.com/hupe1980/agentplane) 0.61, that correlate many exact answers into findings an operator can act on — and that cannot move money, by construction.

## Status

| | |
|---|---|
| ✅ library | three specialists (`compliance-sweep`, `evidence-triage`, `tariff-review`), the `Advice` output type, the read-only `advisory` principal — runs and replays tested in the crate |
| ✅ daemon | builds the `agentplane` runtime with the three specialists over an **in-memory** store, acting as the advisory principal's delegation chain for the estate's operator party (`agentd::runtime(store, &agent)`). `GET /v1/agentd` lists the specialists and their capabilities |
| ⏳ not wired | nothing in the estate triggers a run |
| — | it runs **no language model** and exposes **no MCP surface**. Every specialist is plain Rust |

## What it owns

Every domain crate answers a question about **one** thing: is this chain sound,
is this tariff lawful at this power. An operator's morning question is about a
**population**:

> Four hundred sessions were refused overnight. Is that four hundred support
> tickets, or one meter?

```text
[evidence-triage] BQ27400330016: the meter was in state Substitute at record 2,
                  which may not be billed
                  (3800.000 kWh at risk across 380) — that is a device fault
                  rather than a dispute: raise it with the station vendor…
```

### The specialists

| Specialist | Answers |
|---|---|
| `compliance-sweep` | which duties the estate fails — and which it **will** fail on the day they commence |
| `evidence-triage` | which single fault caused most of today's unbillable energy |
| `tariff-review` | which points offer a tariff `[AFIR Art. 5(4)]` forbids at their power |

**`compliance-sweep`** calls `emob_core::obligation::assess` once per point — no
second implementation of any rule — and adds two things one assessment cannot:
which duty fails across how many points, and breaches that have not started yet,
each judged at its own commencement date. The date and horizon are inputs, so a
run replays to the same answer.

```console
[compliance-sweep] 400 point(s) fail `afir-digitally-connected` [AFIR Art. 5(7)]
                   → connect the point to a CSMS: without it neither the data
                     nor the smart-charging duties can be met
[compliance-sweep] 12 point(s) will fail `da656-iso15118-20-public`
                   [DA-656 Anh. 2.1.2] from 2027-01-01
                   → TLS 1.3 and the larger certificates of -20 usually mean a
                     hardware refresh
```

The suggested action is the calendar's own `remedy`. A forgone entitlement such
as the greenhouse-gas quota is worded as "lawful, and worth money", never as a
breach.

**`tariff-review`** exists because `[AFIR Art. 5(4)]` binds a tariff at the
power a point offers it: the same tariff can be lawful on the 22 kW posts and a
breach on the 150 kW cabinets beside them.

### Advisory only — structurally

- **The output is a leaf.** `Advice` carries observations and a suggestion for a
  human; nothing in the workspace converts it into a CDR, tariff, invoice or
  posting.
- **The principal cannot write.** `advisory` is the only constructor, built with
  `emob_service::Principal::attenuate`, which refuses to widen. A test asserts
  every write capability is absent.
- **The runtime enforces it at the call.** Each specialist is started by the one
  read capability it provides — `emob.evidence.read`, `emob.tariff.read`,
  `emob.location.read` (`skills::evidence::CAPABILITY` and its siblings) — and
  the runtime acts as the advisory chain, so `agentplane` refuses at admission
  any run whose capability lies outside it. A specialist that declared a write
  would be refused by the runtime, not by review; a test starts one with
  `emob.invoice.write` and asserts the refusal.

```rust
let agent = advisory(&operator)?;
agent.may(caps::CDR_READ);       // true
agent.may(caps::INVOICE_WRITE);  // false
```

### Ranked by a quantity, grouped by unit

Findings are ranked by the workspace's own units — kilowatt-hours that cannot be
billed, money not accepted, charge points in breach — never by an invented
"high/medium/low". Each unit is its own queue: EUR and CHF are not compared, and
neither are sessions and charge points.

### Code, journalled and replayable

Specialists are deterministic functions; an answer that varied between runs
would be useless in a queue and indefensible in a dispute. `agentplane` records
each run, its input, answer and effects in an append-only hash-chained journal,
and a replay re-executes the logic reading effects back:

```rust
let runtime = agentd::runtime(store, &agent)?;   // acting as the advisory chain
let outcome = runtime
    .start(skills::evidence::CAPABILITY, Tainted::trusted(refusals))
    .run()
    .await?;
let replayed = runtime.replay(outcome.run_id, Mode::Strict).await?;
assert_eq!(outcome.output, replayed.output);
```

The daemon's store is in memory on purpose: it holds specialist runs, not
decisions, so persisting it is a deployment choice. `emob_service::journal` is
not used.

### What triggers a run

Nothing in the estate does. `agentd` subscribes to no event: neither type emob
emits (`de.emob.cdr.issued`, `de.emob.invoice.issued`) is what a specialist
reads.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[parties]`, `[[operators]]` |
| `EMOB_OPERATOR_TOKEN` | **required** | the variable `[[operators]]` names: the read model's bearer |
| `AGENTD_HTTP_BIND` | `127.0.0.1:9580` | health/readiness shell |
| `EMOB_HTTP_BIND` | — | estate-wide shell address, used when `AGENTD_HTTP_BIND` is unset; defaults are loopback, unreachable inside a container |
| `RUST_LOG` | `info,agentd=debug` | log filter |

`EMOB_JOURNAL_DIR` is not read.

## Running

```console
EMOB_ESTATE=deploy/estate.example.toml EMOB_OPERATOR_TOKEN=… cargo run -p agentd
```

Or `just up` for the whole estate (`agentd`'s shell on `localhost:8086`).

## Endpoints

| Path | Answers |
|---|---|
| `GET /v1/agentd` | operator token: specialists and capabilities |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the run store and the advisory principal exist; `503` listing `journal` / `principal` until then |
| `GET /about` | name and version |

## Built on

[`agentplane`](https://github.com/hupe1980/agentplane) 0.61 (runtime and run
journal), `emob-core` (the obligation calendar), `emob-service` (shell,
principals, event catalogue). See
[Compliance](https://hupe1980.github.io/emob/docs/compliance/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0
