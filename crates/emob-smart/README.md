# emob-smart

§ 14a EnWG as executable law for EV charging sites: which charge points a German grid operator may curtail, the minimum power their operator keeps a claim to, and a site allocation that cannot exceed the grid limit.

[![crates.io](https://img.shields.io/crates/v/emob-smart.svg)](https://crates.io/crates/emob-smart) [![docs.rs](https://img.shields.io/docsrs/emob-smart)](https://docs.rs/emob-smart) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-smart.svg)](#license)

`[EnWG §14a]` lets a distribution grid operator reduce the draw of
*steuerbare Verbrauchseinrichtungen* (controllable consumption devices, SteuVE)
to relieve local overload; the Bundesnetzagentur's Festlegung BK6-22-300 sets
the details. This crate decides which charge points are SteuVE, computes the
guaranteed minimum (*Mindestleistung*), and allocates power across a site so
that no caller — dynamic tariff, fleet schedule or driver — can exceed the grid
limit. It reads no clock and opens no socket.

```console
cargo add emob-smart
```

## Example

```rust
use emob_smart::{Control, Demand, GridLimit, Site, SteuVe, allocate};
use rust_decimal::Decimal;
use time::macros::datetime;

// Two private 11 kW boxes in a yard, on one energy-management system.
let devices: Vec<SteuVe> = [&box_1, &box_2]
    .iter()
    .map(|profile| SteuVe::of(profile, Control::Ems))
    .collect::<Result<_, _>>()?;
let site = Site { connection_kw: Decimal::from(22), devices: devices.clone() };

// Mindestleistung: 4.2 + (2 − 1) × 0.8 × 4.2 kW
assert_eq!(site.minimum().kw, Decimal::try_from(7.56)?);

// The market wants both boxes flat out; the grid operator has said 8 kW.
let plan: Vec<Demand> = devices
    .iter()
    .map(|d| Demand { evse_id: d.evse_id().clone(), wanted_kw: Decimal::from(11) })
    .collect();
let allocation = allocate(
    &site,
    &plan,
    &GridLimit::curtailed(Decimal::from(8), datetime!(2026-06-01 18:00 +2)),
);

assert_eq!(allocation.total_kw(), Decimal::from(8));
assert!(allocation.within_ceiling());
```

## What it guarantees

- **Public charge points are never SteuVE.** `[BK6-22-300 Ziff. 2.4.1]` defines
  a SteuVE charge point as one that is **not** publicly accessible (§ 2 Nr. 5
  LSV), over 4.2 kW, in low voltage. The forecourt fast charger is outside
  § 14a; the depot, workplace car park and fleet yard are inside. `SteuVe::of`
  reads these facts off the `ChargePointProfile` and refuses with `NotSteuVe`
  naming which condition failed (public, too small, not low voltage).
- **Charge points count one by one.** `Ziff. 2.4.2` lets several devices behind
  one connection count as one SteuVE only for heat pumps and space cooling, so
  nine wall boxes are nine SteuVE.
- **The operator's minimum is never under-claimed.** `Ziff. 4.5` guarantees
  4.2 kW per **directly** controlled device, and for devices on an
  energy-management system (EMS) `Pmin = 4.2 kW + (n − 1) × GZF × 4.2 kW`, with
  `n` counting only the EMS devices and the simultaneity factor GZF running from
  0.8 at two devices to 0.45 at nine or more. Two direct and six EMS boxes are
  owed `8.4 + 16.8 = 25.2 kW`; folding all eight into one group would give
  18.9 kW. A setpoint below the minimum is held at the minimum, with a note.
- **The ceiling cannot be raised.** `Ziff. 4.6` gives the grid operator's
  reduction precedence over *marktliche Laststeuerung* (market-driven load
  control). `Allocation` has no constructor and no public fields, `allocate` is
  the only function returning one and it takes the `GridLimit`, and a market
  plan is only a `Demand` input. A property test over 4000 generated sites
  asserts it.
- **No proportional shares below what a box can signal.** See below.

## How it works

### Grid control states

| State | Ceiling before allocation |
|---|---|
| `init` | Configured failsafe power while the site awaits its first control exchange |
| `limited` | Active setpoint |
| `unlimited/controlled` | Connection capacity while the guard permits full power |
| `failsafe state` | Configured failsafe power after 120 seconds without a heartbeat in a controlled state |
| `unlimited/autonomous` | Connection capacity after the local policy releases failsafe under [LPC-922] |
| No guard commissioned | Connection capacity |

The allocator protects the minimum draw and caps every allocation at the connection capacity. It attaches an operator note to failsafe and autonomous decisions. The `emob-eebus` watchdog evaluates time at the edge, and `sited` supplies the resulting state to this crate.

### Why 4.2 kW: the 6 A pilot floor

`DIN EN 61851-1` signals charging current over the control pilot's PWM duty
cycle and cannot express less than 6 A, so a three-phase point cannot be told to
draw under `3 × 230 V × 6 A = 4.14 kW`. The Bundesnetzagentur raised its draft
3.7 kW threshold for that reason.

So power is never split proportionally: 10 kW across four boxes is a figure none
can signal. Points are seated at their **own** floor first, in the order the
operator gives (`Ziff. 4.5.2` makes that order the operator's), and a point that
cannot be seated is held at zero and named. A box that can switch to one phase
mid-session reaches about 1.38 kW — `Ziff. 4.6`'s *"nächstgeringerer Wert, der
technisch möglich ist"*.

### The figures in force

`Ziff. 4.5.2` presumes its calculation until a further recommendation. VDE FNN's
January 2025 recommendation concluded not to change the Mindestleistung without
practical experience, so the table here is the one in force.

## API at a glance

| Item | Purpose |
|---|---|
| `SteuVe::of`, `NotSteuVe`, `Control` | classify a charge point as directly or EMS-controlled SteuVE, or refuse |
| `minimum_for`, `simultaneity_factor`, `Minimum`, `THRESHOLD_KW` | the Mindestleistung arithmetic of `Ziff. 4.5` |
| `Site`, `Demand`, `GridLimit` | a site's connection and devices, the market plan, the grid state |
| `allocate`, `Allocation`, `Curtailment`, `SmartNote` | the only way to obtain an allocation, and its notes |
| `GridConnection`, `GridLevel` | re-exported from `emob-core` |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | `Serialize`/`Deserialize` on every public type |

## Standards and sources

- `[EnWG §14a]`; Bundesnetzagentur Festlegung `[BK6-22-300 Ziff. 2.4.1]`, Ziff. 2.4.2, 4.5, 4.5.2, 4.6
- EEBUS Limitation of Power Consumption: `[LPC §2.3.2]`, `[LPC-901/1]`, `[LPC-922]`, `[LPC-912]`, `[LPC-022]`
- DIN EN 61851-1 (control pilot current floor)

Not covered: heat pumps, space cooling and storage. A site with a heat pump over
11 kW on the same EMS is owed more than `minimum_for` returns.

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices. Setpoints
arrive through `emob-eebus`, allocations leave as OCPP charging profiles through
`emob-ocpp`, and `sited` is the site daemon. Background:
[Smart charging](https://hupe1980.github.io/emob/docs/smart-charging/) on the
site; see also the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
