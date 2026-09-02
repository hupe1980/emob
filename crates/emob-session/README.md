# emob-session

Charging sessions: how they were authorised, what the meter said, and how that
divides across the quarter hours the market settles in.

```console
cargo add emob-session
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Sessions and settlement](https://hupe1980.github.io/emob/docs/settlement/)**.
The signatures are on [docs.rs](https://docs.rs/emob-session).


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

### …and conservation is also a hiding place

Because the sum telescopes *whatever* each boundary was rounded to, an imprecise
boundary is invisible in the total. It does not disappear — it lands entirely on
the supplier who held that quarter hour, which is exactly the misattribution the
telescoping was introduced to prevent. So the interpolation multiplies before it
divides:

```text
delta × offset / gap     ✅ exact wherever the ratio terminates
delta × (offset / gap)   ❌ precision spent on the ratio first
```

The same rule the rating engine follows, for the same reason — and the invariant
this module is proudest of is exactly what would hide the difference.

## A slot carries the window it measured, beside the one it settles in

A session whose readings begin at 10:07 has its first slot reported under the
quarter hour beginning **10:00** — that is the settlement period the energy
belongs to `[A6 §IV.1]` — while its readings only cover **10:07 to 10:15**. Both
statements are true and they are different instants, so `Slot` states both:

```rust
assert_eq!(split.slots[0].quarter_hour.start(), at("10:00"));
assert_eq!(split.slots[0].from, at("10:07"));
assert!(!split.slots[0].covers_the_whole_quarter_hour());
```

A consumer that reconstructs one from the other has to guess, and guesses wrong
whenever the session is wider than its meter series.

## The grid is not the only thing that cuts a session

A quarter hour is where the **energy** settles. It is not where the **price**
changes: `[AFIR Art. 5(4)]` prices the time a vehicle is connected and *not*
charging per minute, and a vehicle stops charging when it stops charging, not at
`:15:00`. A slot running 10:15 to 10:30 with the charge finishing at 10:20 is ten
minutes of occupancy and five of charging, which one `charging` flag cannot say.

So `Session::split` cuts at the session's own state changes as well as at the
grid, and each is another boundary in the same telescoping sum:

```rust
let split = session.split(Direction::Import)?;    // grid + state changes
let split = split::into_periods(&series, &cuts)?; // the primitive underneath
```

Conservation is unaffected — interior boundaries cancel wherever they fall — and
`market_series()` sums the slices of a quarter hour back together, so the market
side still sees one entry per Messperiode.

## The instant that names a period

`[PTB-A 50.7 §3.1.7.2]`, in a footnote: "Der Zeitstempel, der zu einer
Messperiode gehört, ist immer der Zeitpunkt des **Endes** einer Messperiode."

A German meter, an MSCONS load profile and `mako-emob` all call 00:00–00:15 the
**00:15** period. `QuarterHour` calls it 00:00, because a half-open interval
named by its start is the only spelling in which `containing()` is a truncation.
Both are consistent, and mixing them shifts every slot by fifteen minutes — an
error that sums to zero across a session and is wrong for every balance group.

So the conversion has a name and happens once, rather than in each adapter:

```rust
let series = split.market_series();   // each period labelled by its end
```

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

And **a meter had one value at each instant**. Two readings of one register at
one instant that disagree are a contradiction rather than an ordering question:
a stable sort keeps the caller's order at equal keys, so left to the ordering the
same pair reads as a register running backwards when the larger arrives first and
passes in silence when the smaller does — and the arrival order of two messages
is not evidence about a meter. `MeterError::ContradictoryReading` is asked before
the monotonicity question; a duplicate that says the same thing still passes,
because it contradicts nothing.

## No daylight-saving branch

A quarter hour here is an instant plus fifteen minutes of real time. Every civil
UTC offset in the world is a whole number of quarter hours, so a UTC
quarter-hour boundary is a local one everywhere — and the 92- and 100-slot days
of a clock change are simply days with fewer or more instants in them. Nothing
counts to 96.

## Authorisation paths are not interchangeable

Six ways a session starts, and two things depend on which:

| Path | Contract-free | Strongest honest identification |
|---|---|---|
| `AdHoc` | ✅ (the one AFIR requires) | trusted — no contract, so nothing for a certificate to certify |
| `PlugAndCharge` | | secure — `ISO15118_PNC` `[OCMF Tab. 15]` |
| `LocalList` | | secure — a local list decides against an RFID card, and `RFID_PSK` `[OCMF Tab. 13]` is a secured one |
| `Roaming`, `RemoteCommand` | | certified — `OCPP_CERTIFIED` `[OCMF Tab. 14]` certifies the mapping with a backend certificate |
| `AutoCharge` | | hearsay — a MAC address off the wire |


**The ceiling is read off Tables 13–16, not off who decided.** `[OCMF Tab. 11]`
grades *how the user was identified*; Tables 13–16 say which identifications each
mechanism can carry, and the two axes are largely orthogonal. A ceiling set from
the decision-maker refuses ordinary hardware — a local list decides against an
RFID card, and a secured card is Table 11's own example of `SECURE`. What stays
low is what cannot rise: an unauthenticated MAC address, and an ad-hoc session
that presented no contract for anything to certify.

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
TokenRef::new("NLTNM000122045U")  // Err: that is an eMAID
TokenRef::new(&keyed_digest)      // Ok: 64 lowercase hex characters
```

A card UID is a lifelong identifier of a physical object a person carries.
Storing it on every session row builds a movement profile no part of a charging
platform needs, so the type refuses anything that is not a 256-bit digest.

## Sessions are state machines, and the history is timestamped

```rust
session.transition_to(SessionState::Suspended, at(40))?;
session.end(at(70), EndReason::Local)?;
session.attach_series(more)?;   // Err: AlreadyEnded
```

Nothing follows the end — not a second end, not a resumption, not a late meter
series, because a series arriving after the end is either a duplicate or a
different session and guessing which is how energy gets double-billed. A
transition dated before the one it follows is refused too: a history that is not
ordered cannot be read as intervals.

And it has to be readable as intervals, because "suspended" is not a status
light. `[AFIR Art. 5(4)]` lets a fast charger charge an occupancy fee **per
minute** for the time a vehicle is connected and not charging, so the question
is *for how long*, and a state field without a timestamp cannot answer it.

```rust
assert_eq!(session.state_at(at(50)), Some(SessionState::Suspended));
assert!(session.suspended_throughout(at(45), at(60)));   // half an hour to price
```

`emob-cdr` uses the same history the other way round: energy across a quarter
hour the session logged as suspended from end to end means the meter and the
state machine disagree, and it refuses the record rather than picking one.

**And the interval is asked two ways, because there are four states.**
`charging_throughout` is not the negation of `suspended_throughout`: `Pending` is
authorised with nothing flowing and `Ended` is over, and both are "connected and
not charging" — which is exactly what the fee prices. Where a meter reading
exists it is the evidence, so a slot is charging unless the session says it was
suspended throughout; where nothing was measured there is none, so the question
has to run the other way round. Asking `!suspended_throughout` there would read
"the record never said suspended" as "the vehicle was charging", which is an
absence standing in for a claim.

Import and export are separate registers counting separate quantities, and one
session can hold both. They never net: 18 kWh drawn and 5 kWh returned is 18 and
5, never 13.

## No I/O, no clock

Every instant is an argument, so a session from two years ago splits today
exactly as it split then.

## License

MIT OR Apache-2.0.
