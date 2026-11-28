# emob-sim

A deterministic EV charging fleet simulator: virtual stations that sign genuine OCMF meter records, eight seeded faults, and a day-level reconciliation proving every kilowatt-hour was either billed or refused with a reason.

[![crates.io](https://img.shields.io/crates/v/emob-sim.svg)](https://crates.io/crates/emob-sim) [![docs.rs](https://img.shields.io/docsrs/emob-sim)](https://docs.rs/emob-sim) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-sim.svg)](#license)

A fleet test that asserts "no errors" passes by throwing sessions away. This
crate asserts something a silent failure cannot satisfy: over a whole day,
`Σ settled + Σ refused = Σ metered`, and every refusal carries a reason. The
stations are virtual — no socket, no hardware — but their ECDSA signatures over
OCMF (Open Charge Metering Format, in which a calibrated meter signs each
reading) are real, and everything downstream runs the production code.

```console
cargo add emob-sim
```

## Example

```rust
use emob_sim::{FaultPlan, Rate, ReferenceDay};

let outcome = ReferenceDay::builder()
    .seed(7)
    .stations(100)
    .sessions_per_station(4)
    .faults(FaultPlan::everything(Rate::one_in(9)))
    .build()
    .run();

println!("{}", outcome.summary());
// "400 sessions: … settled (… kWh), … refused (… kWh), metered … kWh"

assert!(outcome.reconciles());
assert!(outcome.every_session_is_accounted_for());
assert!(outcome.every_refusal_has_a_reason());
```

## What it guarantees

- **Nothing is unaccounted for.** `DayOutcome::reconciles` holds when every
  kilowatt-hour a meter moved reached a settled record or a reasoned refusal.
- **Signatures are genuine.** A real ECDSA key signs the real payload bytes,
  verified through the same path as a record from a physical meter. Hand-written
  fixtures only prove the parser accepts what a test author typed.
- **Nothing downstream is stubbed.** `emob-ocpp` assembles each session from the
  station's own OCPP transaction events, `emob-eichrecht` verifies the evidence,
  `emob-session` splits on the settlement grid, `emob-tariff` prices,
  `emob-cdr` builds the record and the ledger accepts it. Faults land on the
  OCPP events as they would on a wire, so a dropped record is genuinely absent.
- **One seed is one day.** No entropy source, no clock, no thread-local state;
  randomness is a built-in `SplitMix64` stream, so a dependency update cannot
  reorder it. Independent parts draw from labelled streams, so adding a draw in
  one generator does not reshape every session.
- **No I/O.** Nothing opens a socket, reads a file or asks the time; the day
  starts at `ReferenceDayBuilder::day_start`. `just purity` enforces it.

## How it works

### Realistic sessions

- **Every transaction opens `EVConnected`.** As on a real OCPP 2.0.1 station,
  the transaction starts with `chargingState = EVConnected` and a
  `Transaction.Begin` reading, and reports `Charging` 5 seconds to 3 minutes
  later. That exercises both halves of `[REA 6-A §3.1]` on fast posts: a wait the
  clock cannot resolve is dropped from the occupancy fee with a note; one it can
  is billed.
- **The register belongs to the station.** A meter counts up over its life and a
  session is the difference of two readings `[OCMF Tab. 7]`, so each station
  carries its register across sessions rather than resetting to zero.
- **The charge curve tapers**, so quarter-hour energies are not uniform and the
  split's interpolation is exercised on non-terminating ratios.
- **Half the posts are 22 kW, half 150 kW**, offered the same tariff, so the
  tariff-shape rule can refuse only the fast half.

### The eight faults

| Fault | Rule it exercises |
|---|---|
| `SubstituteReading` | the meter reported a value it could not measure `[OCMF Tab. 10]` |
| `DroppedRecord` | a gap in the pagination, with every remaining signature valid |
| `TamperedValue` | a payload byte changed after signing |
| `UnsynchronisedClock` | energy bills, duration does not `[OCMF Tab. 19]` |
| `WrongDirectionRegister` | a `C2` export register billed as a draw `[OCMF Tab. 25]` |
| `ExceptionDuringCharging` | `TX=X`: time and energy unusable from that point |
| `UnregisteredStation` | a provisioning gap — the station's key is not in the registry |
| `UnlawfulTariff` | a tariff the post may not offer at its power `[AFIR Art. 5(4)]` |

`UnsynchronisedClock` is the one fault that leaves the **energy** billable,
which is the distinction Eichrecht (German calibration law) rests on; the
catalogue makes every fault declare which side it falls on (`Fault::blocks_energy`). Use
`FaultPlan::everything` for fleet runs, so new rules are exercised without
anyone remembering to list them.

### Telemetry is not money

`ocpp-kit`'s CSMS ledger answers operational questions — did every event
arrive, was this a retry — and carries meter values as `f64`. No billed
kilowatt-hour goes through it: the billed value comes from the signed OCMF
register in exact decimal.

## API at a glance

| Item | Purpose |
|---|---|
| `ReferenceDay`, `ReferenceDayBuilder` | configure (`seed`, `stations`, `sessions_per_station`, `day_start`, `faults`) and `run` a day |
| `DayOutcome` | `reconciles`, `every_session_is_accounted_for`, `every_refusal_has_a_reason`, `every_record_conserves`, `faults_seen`, `summary` |
| `Fault`, `FaultPlan`, `Rate` | which faults, how often |
| `VirtualStation`, `SessionPlan`, `ChargedSession`, `Identification` | one simulated station and its sessions |
| `Rng` | the deterministic `SplitMix64` stream |

## Feature flags

None.

## Standards and sources

- OCMF: `[OCMF Tab. 7]`, `[OCMF Tab. 10]`, `[OCMF Tab. 19]`, `[OCMF Tab. 25]`
- `[REA 6-A §3.1]` — duration billing and clock synchronisation
- `[AFIR Art. 5(4)]` — tariff shapes at a given power
- OCPP 2.0.1 transaction events

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices; this crate
drives the whole domain chain end to end. Background:
[Architecture](https://hupe1980.github.io/emob/docs/architecture/) and
[Eichrecht](https://hupe1980.github.io/emob/docs/eichrecht/) on the site; see
also the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
