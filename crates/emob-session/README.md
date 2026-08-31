# emob-session

Charging sessions: how they were authorised, what the meter said, and how that
divides across the quarter hours the market settles in.

```console
cargo add emob-session
```

## The quarter-hour split conserves energy exactly

A session running 10:01 to 10:22 has to be settled against two quarter hours,
and the boundary at 10:15 falls two thirds of the way through. Seven kilowatt
hours times two thirds does not terminate.

The naive approach computes each slot independently, discovers the sum is a few
milliwatt-hours off the total, and shoves the difference into the last slot —
silently misattributing energy to whoever held 10:15.

Instead this computes the **cumulative** value at each boundary once and takes
differences:

```text
slot[i] = cumulative(boundary[i+1]) − cumulative(boundary[i])
```

The sum telescopes. Every interior boundary appears once positive and once
negative and cancels exactly, whatever it was rounded to; what is left is
`cumulative(end) − cumulative(start)`, the session total, to the last digit,
always.

```rust
let split = split::into_quarter_hours(&series)?;
assert!(split.conserves());        // exactly, for every session
```

A test runs 144 generated sessions — six start offsets × six durations × four
totals, chosen to produce awkward ratios — and asserts it on every one.

## Every slot says where its number came from

```rust
assert!(!split.fully_measured());
for slot in split.interpolated() {
    // this one was derived, not measured
}
```

`Provenance::Measured` means a `Sample.Clock` reading landed on that boundary.
`Provenance::Interpolated` means it was derived by assuming constant power
across a gap — which a tapering charge curve does not deliver: a car at 80 %
state of charge draws far less at the end of a gap than at its start, so a
straight line over-allocates to the later side. The error lands on whichever
supplier held the boundary, so the assumption travels with the number instead of
being forgotten.

A `Sample.Periodic` reading that happens to fall on `:15:00` does **not** count
as measured. The station chose that instant for its own reasons, and treating
the coincidence as a clock-aligned measurement reports a settlement as
authoritative on a day the phase drifted.

## No daylight-saving branch

A quarter hour here is an instant plus fifteen minutes of real time. Every civil
UTC offset in the world is a whole number of quarter hours, so a UTC
quarter-hour boundary is a local one everywhere — and the 92- and 100-slot days
of a clock change are simply days with fewer or more instants in them. Nothing
counts to 96.

## Authorisation paths are not interchangeable

Five ways a session starts, and two things depend on which:

| Path | Contract-free | Strongest honest identification |
|---|---|---|
| `AdHoc` | ✅ (the one AFIR requires) | trusted |
| `PlugAndCharge` | | secure |
| `Roaming`, `RemoteCommand` | | trusted |
| `LocalList`, `AutoCharge` | | hearsay |

`AutoCharge` recognises a vehicle by its MAC address. It is not a standard, not
authenticated and trivially spoofable, and it is kept rigorously distinct from
Plug & Charge — the two are constantly conflated in marketing and must never be
conflated in evidence.

`strongest_plausible_level()` is a **ceiling**, and it is what lets `emob-cdr`
notice a session claiming Plug & Charge whose signed record reports a bare RFID
UID. Under-reporting is a station being conservative; over-reporting is a
contradiction.

## Tokens are never stored raw

```rust
TokenRef::new("1F2D3A4F5506C7")   // Err: that is an RFID UID
TokenRef::new("DE8AACA2B3C4D51")  // Err: that is an eMAID
TokenRef::new(&keyed_digest)      // Ok: 64 lowercase hex characters
```

A card UID is a lifelong identifier of a physical object a person carries.
Storing it on every session row builds a movement profile no part of a charging
platform needs, so the type refuses anything that is not a 256-bit digest.

## Sessions are state machines

```rust
session.end(at, EndReason::Local)?;
session.attach_series(more)?;   // Err: AlreadyEnded
```

Nothing follows the end — not a second end, not a resumption, not a late meter
series, because a series arriving after the end is either a duplicate or a
different session and guessing which is how energy gets double-billed.

Import and export are separate registers counting separate quantities, and one
session can hold both. They never net: 18 kWh drawn and 5 kWh returned is 18 and
5, never 13.

## No I/O, no clock

Every instant is an argument, so a session from two years ago splits today
exactly as it split then.

## License

MIT OR Apache-2.0.
