# emob-sim

A deterministic e-mobility fleet, driven through the whole chain: virtual
charging stations that **sign genuine OCMF**, eight seeded faults, and a
reconciliation that proves every kilowatt-hour either billed or was refused
with a reason.

Part of [emob](https://github.com/hupe1980/emob), the open-source e-mobility
operating stack.

```console
cargo add emob-sim
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Architecture](https://hupe1980.github.io/emob/docs/architecture/)**.
The signatures are on [docs.rs](https://docs.rs/emob-sim).

## The demo that cannot lie

```rust
use emob_sim::{FaultPlan, Rate, ReferenceDay};

let outcome = ReferenceDay::builder()
    .stations(100)
    .sessions_per_station(4)
    .faults(FaultPlan::everything(Rate::one_in(9)))
    .build()
    .run();

println!("{}", outcome.summary());
// 400 sessions: 197 settled (8969.120 kWh), 203 refused (9555.738 kWh), metered 18524.858 kWh

assert!(outcome.reconciles());
assert!(outcome.every_session_is_accounted_for());
assert!(outcome.every_refusal_has_a_reason());
```

The assertion is **not** "everything billed". It is:

> Every kilowatt-hour a meter moved either reached a settled record or was
> refused with a reason. Nothing is unaccounted for.

That is `Σ allocated + residual = total` over a day rather than a session, and
it is the only assertion a fleet run can make that a silent failure cannot
satisfy. A run asserting "no errors" passes by throwing sessions away.

## What is virtual, and what is not

The station is imaginary: no socket, no cabinet, no WebSocket. Its
**signatures are genuine** — a real ECDSA key, over the real payload bytes,
verified through the same code path a record off a real meter goes through.

A simulator whose fixtures are hand-written strings proves that the parser
accepts what the test author typed. One that signs proves that the chain
accepts what a station produces.

Everything downstream is the real thing too: `emob-ocpp` assembles the session
from the station's own OCPP transaction events, `emob-eichrecht` verifies,
`emob-session` splits on the settlement grid, `emob-tariff` prices, `emob-cdr`
builds and the ledger accepts. Nothing is stubbed.

The OCPP step matters more than it looks. A fleet that built its sessions
directly from the meter series it had just generated would prove everything
downstream of a CSMS and nothing about the CSMS itself — so the station emits
events, the faults land on those events the way they would land on a wire, and a
dropped record is genuinely absent from the stream. The reconciliation is
unchanged to the last digit, which is the point: the seam is transparent to the
arithmetic and now under the same assertion as everything else.

## The register is the station's, not the session's

A real meter counts up over its whole life, and a session is a *difference*
between two of its readings `[OCMF Tab. 7]`. So the station carries its register
across sessions, and the second session on a post starts where the first left
it. A generator that resets to zero hides an off-by-one nobody would notice.

The charge curve tapers, too — a battery at 80 % takes far less than one at
20 % — because a session that delivers the same energy every quarter hour makes
every interpolation exact and every ratio terminate, which is precisely the case
the split's conservation proof does not need.

## Faults are the point

| Fault | The rule it exercises |
|---|---|
| `SubstituteReading` | a meter that invented a value it could not measure `[OCMF Tab. 10]` |
| `DroppedRecord` | a hole in the pagination that every remaining signature verifies across |
| `TamperedValue` | a payload byte changed after signing |
| `UnsynchronisedClock` | the energy bills and the duration does not `[OCMF Tab. 19]` |
| `WrongDirectionRegister` | a `C2` export register billed as a draw `[OCMF Tab. 25]` |
| `ExceptionDuringCharging` | `TX=X`: time and energy unusable from there on |
| `UnregisteredStation` | a provisioning gap — the commonest unbillable session in the field |
| `UnlawfulTariff` | a tariff the post may not offer at its power `[AFIR Art. 5(4)]` |

Each names a rule this workspace enforces somewhere, and a rule nothing
exercises is a rule that quietly stops holding. `FaultPlan::everything` is the
setting a fleet run should use: a run that exercises only the rules somebody
remembered to list is a run that drifts.

`UnsynchronisedClock` is the one that leaves the **energy** billable, which is
the distinction the whole Eichrecht chain is built on — and the fleet asserts
it, over the catalogue, so a new fault has to declare which side it is on.

`UnlawfulTariff` is the one that has nothing to do with the meter, and it is why
a post carries a rated power. Seven metering faults never once ran the tariff
shape gate: the energy was measured perfectly, every signature held, and the
session was priced with whatever tariff the fleet handed it. The fault cannot be
a property of the tariff either — a per-minute-only tariff is an ordinary product
— so it is a property of the **pairing**: half the fleet is a 22 kW post and half
a 150 kW charger, the same tariff is offered to both, and only the fast half is
refused.

## One seed is one day

No entropy source, no clock, no thread-local state, and a `SplitMix64` stream
rather than a crate whose next release might reorder its output. A fleet run
that fails once a month for reasons nobody can recreate is worse than no fleet
run at all.

Independent parts of a run draw from **labelled** streams, so adding a draw to
the station generator does not reshape every session — otherwise a regression
looks like a change everywhere.

## The seam this crate is *not*

`ocpp-kit`'s CSMS ledger answers an operational question — did every event
arrive, is the sequence complete, was this a retry — and takes its meter values
as `f64`. That is right for telemetry and wrong for money, so nothing here
routes a billed kilowatt-hour through it: **the billed value comes from the
signed OCMF register**, in exact decimal, and the OCPP ledger's job is to say
whether anything went missing on the way. The two are complementary and must not
be confused.

## No I/O, no clock

Nothing here opens a socket, reads a file or asks the time; the day starts at an
instant you pass in. `just purity` fails the build if that stops being true.

## License

MIT OR Apache-2.0.
