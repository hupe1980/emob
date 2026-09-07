# emob-eebus

**The § 14a wire**: a grid operator's setpoint lifted off a SPINE datagram as an
exact decimal, and the five states `[LPC §2.3.2]` gives a controllable site.

```console
cargo add emob-eebus
```

## What is here, and what is `eebus`'s

[`eebus`](https://crates.io/crates/eebus) implements *Limitation of Power
Consumption* on both sides, against all 203 of its published abstract test
cases. Nothing here re-implements any of it — the datagrams, the bindings, the
subscriptions and the acknowledgement a write is owed are the kit's, and this
crate drives its `Engine` rather than replacing it.

What is here is the seam, and it is the two things the kit leaves to a caller.

## The value, exactly

SPINE carries power as a `ScaledNumber` — `{ number, scale }`, an integer
mantissa and a power of ten, which *is* a decimal:

```rust
assert_eq!(watts_of(&ScaledNumber::new(42, 2)),  Some(Decimal::from(4200)));
assert_eq!(watts_of(&ScaledNumber::new(414, 1)), Some(Decimal::from(4140)));
```

Sixty watts apart, and the difference matters: `[BK6-22-300 Ziff. 4.5]`'s
Mindestleistung is 4.2 kW and three-phase at 6 A is 4.14 kW, so a platform that
cannot tell them apart cannot say whether it honoured an Anspruch. One statute
along, `[EEG §9]`'s 70 % of a 9.9 kW array is `6930.000000000001` in `f64` — and
a feed-in payment is computed against it.

A value no decimal can hold is **refused**, not approximated. A `scale` is a
signed 16-bit number the peer chooses, so an unrepresentable figure is one
well-formed message away; a NACK is a fact the Energy Guard can act on, and a
substituted value is a limit nobody asked for applied to a site that will obey
it.

## Which state a silence has put the site in

`[LPC §2.3.2]` gives a controllable system five states and **three of them allow
full power**. A depot at full power because its grid operator says so, one whose
control box died four hours ago, and one nobody ever commissioned are the same
number and three different problems.

`emob_smart::GridLimit` is those five, read as a rule before this crate existed.
What `Watchdog` adds is the clock:

| Silence | State | Ceiling |
|---|---|---|
| never spoken | `Uncontrolled` | the site's own connection |
| speaking, curtailed | `limited` | the setpoint |
| speaking, released | `unlimited/controlled` | the connection |
| past the Failsafe Duration Minimum `[LPC-912]` | `failsafe state` | the failsafe value |
| past `[LPC-022]`'s release | `unlimited/autonomous` | the connection |

The last row is the half every summary of § 14a drops. A site leaves its
failsafe after between two and 24 hours and runs on its own connection — because
a building throttled to 4.2 kW for a week because a control box died is not what
§ 14a asks for, and a grid operator that believes it can still curtail such a
site is wrong about something it needs to be right about.

## Why the value does not come through the use-case actor

`csmsd` keeps the same line with `ocpp-kit`: that crate's CSMS ledger answers an
*operational* question from a numeric register, and **no billed kilowatt-hour is
ever routed through it**.

Here the line falls in the same place. `eebus`'s use-case actor answers the
communication question and its surface is `f64`, because SPINE's implementation
guides talk in watts. The value that binds an allocation comes off the
`ScaledNumber` on the datagram, exactly, and this workspace's `no-floats` guard
is what keeps that from quietly stopping being true.

## No I/O, no clock

A datagram is a value and every instant is an argument. `eebus` is taken
**without `runtime`**, so there is no tokio in this tree and
`cargo xtask check-graph` is what holds it — the sockets, the TLS and the mDNS
belong to `sited`.

## Licence

MIT OR Apache-2.0
