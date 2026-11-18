# emob-eebus

The § 14a EnWG wire for EV charging sites: a grid operator's EEBUS power limit read off a SPINE datagram as an exact decimal, and a watchdog that says which of the five LPC states a site is in.

[![crates.io](https://img.shields.io/crates/v/emob-eebus.svg)](https://crates.io/crates/emob-eebus) [![docs.rs](https://img.shields.io/docsrs/emob-eebus)](https://docs.rs/emob-eebus) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-eebus.svg)](#license)

In Germany a grid operator curtails controllable devices under `[EnWG §14a]`
through an Energy Guard (control box) that speaks EEBUS — SHIP transport and
SPINE data model — using the Limitation of Power Consumption (LPC) use case. The
[`eebus`](https://crates.io/crates/eebus) crate (0.11) implements LPC on both
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

// Two hours of silence puts the site into its failsafe [LPC-912]…
assert_eq!(
    watchdog.limit_at(datetime!(2026-06-01 20:30 +2)).lpc_state(),
    Some("failsafe state")
);
// …and two more release it [LPC-022].
assert_eq!(
    watchdog.limit_at(datetime!(2026-06-01 23:00 +2)).lpc_state(),
    Some("unlimited/autonomous")
);
```

## What it guarantees

- **The limit is exact.** SPINE carries power as a `ScaledNumber` — an integer
  mantissa and a power of ten, which is a decimal. `watts_of` reads it without
  floating point. The `[BK6-22-300 Ziff. 4.5]` minimum of 4.2 kW and a
  three-phase 6 A draw of 4.14 kW are 60 W apart, and that gap decides whether
  an operator's legal minimum was honoured.
- **An unrepresentable value is refused, not approximated.** The `scale` is a
  signed 16-bit number the peer picks, so `read_limit_write` returns
  `EebusError::UnreadableLimit` rather than substitute a limit nobody asked for.
  A negative limit, which `[LPC-001]` forbids, is `EebusError::NegativeLimit`.
- **Silence has a state.** `Watchdog::limit_at` returns an `emob_smart::GridLimit`
  for any instant, carrying when the Energy Guard was last heard from.
- **No floats on the binding path.** `eebus`'s use-case actor exposes `f64`
  (SPINE guides talk in watts); this crate drives the `eebus` SPINE `Engine`
  directly and takes the value off the datagram, and the workspace's
  `no-floats` guard keeps it that way.

## How it works

`[LPC §2.3.2]` gives a controllable site five states, three of which allow full
power for different reasons. `Watchdog` decides which holds at a given instant:

| Situation | LPC state | Ceiling |
|---|---|---|
| never heard from | uncontrolled | the site's own connection |
| heard from, curtailed (`limited_to`) | `limited` | the setpoint |
| heard from, released (`released`) | `unlimited/controlled` | the connection |
| silent past the failsafe period `[LPC-912]` | `failsafe state` | the failsafe value `[LPC-901/1]` |
| still silent past the release `[LPC-022]` | `unlimited/autonomous` | the connection |

The failsafe period is written by the Energy Guard; until it arrives,
`FAILSAFE_AFTER` uses the two-hour floor of `[LPC-021]`, the reading that keeps a
site under control longer. `Watchdog::released_after` sets the release period.
The release matters: a building held at 4.2 kW for a week because a control box
died is not what § 14a asks for, and a grid operator must know that such a site
can no longer be curtailed.

`eebus` is used **without** its `runtime` feature, so this crate pulls in no
tokio, TLS or mDNS; `cargo xtask check-graph` enforces it. The sockets belong to
`sited`.

## API at a glance

| Item | Purpose |
|---|---|
| `watts_of` | `ScaledNumber` → exact `Decimal` watts, or `None` |
| `read_limit_write`, `LimitWrite` | the limit an LPC write would set, read from the resolved SPINE payload |
| `Watchdog` | `heard_from`, `limited_to`, `released`, `limit_at` — the LPC state at an instant |
| `FAILSAFE_AFTER` | the `[LPC-021]` two-hour floor |
| `EebusError` | `NotALimitWrite`, `UnreadableLimit`, `NegativeLimit` |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | `Serialize`/`Deserialize` on every public type |

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
