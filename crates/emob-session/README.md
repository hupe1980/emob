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

Both crates now do it through **one** function, `emob_core::apportion`, which
also quotes the result to a fixed scale. `Decimal` carries ninety-six bits, and a
boundary two thirds of the way through a gap spends all of them on its fraction:
adding two of those needs more digits than there are, the interior boundaries
stop cancelling, and `conserves()` fails by one unit in the last place — in the
assertion that exists to prove there is none. Twelve places is a nanowatt-hour,
and it is the difference between exactly and nearly.

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
let split = session.split(Direction::Import)?;           // grid + state changes + idle
let split = split::into_periods(&series, &cuts, &idle)?; // the primitive underneath
```

Conservation is unaffected — interior boundaries cancel wherever they fall — and
`market_series()` sums the slices of a quarter hour back together, so the market
side still sees one entry per Messperiode.

### …and constant power means constant power *while charging*

A straight line between two readings is the honest guess across a gap the
session says nothing about, and a wrong one across a gap it does. The ordinary
OCPP 2.0.1 transaction opens `EVConnected` with a `Transaction.Begin` reading,
starts charging thirty seconds later with no reading, and sends its next meter
value at the quarter hour; its register did not move for those thirty seconds,
and a line from the opening reading attributes energy to an interval the
operator's own state machine calls suspended.

So `Session::split` hands the interpolation `Session::idle_intervals()` — every
stretch of the history in a state other than `Charging` — and the register is
held flat across them, the gap's energy spread over the seconds that remain:

```rust
let split = split::into_periods(&series, &[charging_from], &[(begin, charging_from)])?;
assert!(split.slots[0].energy.is_zero());   // nothing flowed before the charge began
assert!(split.conserves());                  // and the total is untouched
```

Where a gap has no charging time at all and the register moved anyway, the line
is drawn across the whole gap and the contradiction is left standing for
`emob-cdr` to report — because it is now the meter's own.

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
`Provenance::Held` means the register was carried unchanged from a measured
reading across an interval the session says nothing flowed in — exact, on the
operator's own account, and a third answer because it rests on the state machine
rather than on a second reading; `fully_measured()` accepts it, `interpolated()`
does not list it. `Provenance::Interpolated` means it was derived by assuming
constant power
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

The market side *does* count, and 96 is what every exporter hard-codes. A
balance-group submission is validated on how many Messperioden a day holds
`[A6 §IV.1]`, so a series with 96 entries for 25 October is missing an hour of
somebody's balance group:

```rust
let berlin = TimeZone::new("Europe/Berlin")?;
QuarterHour::periods_in_local_day(&berlin, date!(2026-06-15));  // Some(96)
QuarterHour::periods_in_local_day(&berlin, date!(2026-03-29));  // Some(92)  spring forward
QuarterHour::periods_in_local_day(&berlin, date!(2026-10-25));  // Some(100) autumn fold
```

Measured between the two local midnights rather than read from a table of
transition dates, so a zone that shifts by half an hour or at an hour other than
02:00 needs no case. `None` says the day is not a whole number of periods at all
— a civil offset that is not a multiple of fifteen minutes, which Liberia was the
last of, in 1972.

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
session.transition_to(SessionState::SuspendedByVehicle, at(40))?;
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
assert_eq!(session.state_at(at(50)), Some(SessionState::SuspendedByVehicle));
assert_eq!(session.activity_throughout(at(45), at(60)), Some(Activity::Parked));
```

`emob-cdr` uses the same history the other way round: energy across a period
the session says it was not charging in means the meter and the state machine
disagree, and it refuses the record rather than picking one.

**And the question is asked once, as "what was it doing".**
`activity_throughout` returns an `Activity` and may return `None`: `Pending` is
authorised with nothing flowing and `Ended` is over, and both are "connected and
not charging" — which is exactly what the fee prices. Every period a CDR carries
is asked that one question, metered or not; asking `!suspended_throughout`
instead reads "the record never said suspended" as "the vehicle was charging",
and bills the minute a car sat `EVConnected` before its charge began as charging
time.

**There are two suspensions, because "no energy is flowing" has two causes and
only one of them owes a fee.** `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`
corrected its own definition of `PARKING_TIME` to the **vehicle's** demand, and
said why: under the old reading drivers "would be exposed to penalizing loitering
fees … when the EVSE is not offering energy to the vehicle while the vehicle is
still requesting power". So `SuspendedByVehicle` — the battery is full — is
`Activity::Parked` and prices as occupancy, while `SuspendedByOperator` — a
charging profile at zero, a `[EnWG §14a]` dimming, a grid limit, a fault — is
`Activity::Withheld` and prices as nothing. OCPP has distinguished them since
2.0.1, as `SuspendedEV` against `SuspendedEVSE`.

Import and export are separate registers counting separate quantities, and one
session can hold both. They never net: 18 kWh drawn and 5 kWh returned is 18 and
5, never 13.

## No I/O, no clock

Every instant is an argument, so a session from two years ago splits today
exactly as it split then.

## License

MIT OR Apache-2.0.
