# emob-eebus

The § 14a EnWG wire for EV charging sites: a grid operator's EEBUS power limit read off a SPINE datagram as an exact decimal, and a watchdog that says which of the five LPC states a site is in.

[![crates.io](https://img.shields.io/crates/v/emob-eebus.svg)](https://crates.io/crates/emob-eebus) [![docs.rs](https://img.shields.io/docsrs/emob-eebus)](https://docs.rs/emob-eebus) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-eebus.svg)](#license)

In Germany a grid operator curtails controllable devices under `[EnWG §14a]`
through an Energy Guard (control box) that speaks EEBUS — SHIP transport and
SPINE data model — using the Limitation of Power Consumption (LPC) use case. The
[`eebus`](https://crates.io/crates/eebus) crate (0.14) implements LPC on both
sides, against all 203 of its published abstract test cases; this crate is the
thin seam between it and `emob-smart`. It does two things the kit leaves to a
caller: read the limit exactly, and track which state a silence has put the site
in. It has no I/O and no clock.

```console
cargo add emob-eebus
```

## Example

```rust
use emob_eebus::{Watchdog, watts_of};
use eebus::model::ScaledNumber;
use rust_decimal::Decimal;
use time::macros::datetime;

// 42 × 10² watts, off the wire, exactly.
assert_eq!(watts_of(&ScaledNumber::new(42, 2)), Some(Decimal::from(4200)));

let mut watchdog = Watchdog::new(Decimal::from_str_exact("4.2")?, time::Duration::hours(2));
watchdog.heard_from(datetime!(2026-06-01 18:00 +2));

assert_eq!(watchdog.limit_at(datetime!(2026-06-01 18:00 +2)).lpc_state(), Some("init"));
```

## What it guarantees

- **The limit is exact.** SPINE carries power as a `ScaledNumber` — an integer
  mantissa and a power of ten, which is a decimal. `watts_of` reads it without
  floating point. The `[BK6-22-300 Ziff. 4.5]` minimum of 4.2 kW and a
  three-phase 6 A draw of 4.14 kW are 60 W apart, and that gap decides whether
  an operator's legal minimum was honoured.
- **An unrepresentable value is refused, not approximated.** The `scale` is a
  signed 16-bit number the peer picks, so `read_limit_write` returns
  `EebusError::OutsidePrecision` rather than substitute a limit nobody asked for.
  A negative limit, which `[LPC-001]` forbids, is `EebusError::NegativeLimit`.
- **A resolved write names one limit, its activation flag and its value.** Missing fields or a duplicate target get a refusal. Explicit zero watts is valid for both an active limit and a release.
- **Silence has a state.** `Watchdog::limit_at` returns an `emob_smart::GridLimit`
  from time-stamped receipts and a caller-supplied evaluation instant.

## When the ceiling changes

| Trigger | LPC state | Power before the site floor and connection cap |
|---|---|---|
| Controller starts | `init` | Configured failsafe power |
| Heartbeat followed within 60 seconds by an applicable active limit | `limited` | Requested watts |
| Validity expires during healthy contact, or the guard releases the limit | `unlimited/controlled` | Connection capacity |
| Last heartbeat reaches age 120 seconds in either controlled state | `failsafe state` | Configured failsafe power |
| Minimum residence elapses after entry into failsafe | `unlimited/autonomous` | Connection capacity |

The local profile chooses the optional transition to autonomous at the configured residence (2–24 hours). Only a heartbeat updates the contact timer. A returning heartbeat requires a following limit for recovery. A value-only update preserves an existing deadline. An explicit duration starts at receipt, and deleting the end time clears the deadline. [EEBUS Limitation of Power Consumption](https://www.eebus.org/wp-content/uploads/2023/04/EEBus_UC_TS_LimitationOfPowerConsumption_V1.0.0_public.pdf)

`eebus` is used **without** its `runtime` feature, so this crate pulls in no
tokio, TLS or mDNS; `cargo xtask check-graph` enforces it. The sockets belong to
`sited`.

## API at a glance

| Item | Purpose |
|---|---|
| `watts_of` | `ScaledNumber` → exact `Decimal` watts, or `None` |
| `read_limit_write`, `LimitWrite` | the limit an LPC write would set, read from the resolved SPINE payload |
| `Watchdog` | `heard_from`, `on_write`, `limit_at` — the LPC state at an instant |
| `MIN_FAILSAFE_RESIDENCE` | default two-hour residence |
| `EebusError` | typed refusal for invalid data, timing or local precision |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | serialization of limit values. The control exchange is volatile |

## Standards and sources

- EEBUS Limitation of Power Consumption: `[LPC §2.3.2]`, `[LPC-001]`, `[LPC-021]`, `[LPC-022]`, `[LPC-901/1]`, `[LPC-912]`
- `[EnWG §14a]`, `[BK6-22-300 Ziff. 4.5]`
- SHIP and SPINE via [`eebus`](https://crates.io/crates/eebus)

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices. This crate
feeds `emob-smart`, which allocates power under the limit; `sited` is the site
daemon that will hold the SHIP session. Background:
[Smart charging](https://hupe1980.github.io/emob/docs/smart-charging/) on the
site; see also the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
