# emob-session

EV charging sessions for CSMS and billing developers: how a session was
authorised, what the meter said, and how its energy divides exactly across the
quarter hours the energy market settles in.

[![crates.io](https://img.shields.io/crates/v/emob-session.svg)](https://crates.io/crates/emob-session) [![docs.rs](https://img.shields.io/docsrs/emob-session)](https://docs.rs/emob-session) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-session.svg)](#license)

A session's energy has to be settled per quarter hour, and its idle time priced
per minute. This crate splits a meter series so the slots sum to the session
total to the last digit, records whether each slot was measured or
interpolated, and keeps a timestamped state history that answers "how long was
the vehicle connected and not charging?".

```console
cargo add emob-session
```

## Example

```rust
use emob_session::{MeterReading, MeterSeries, ReadingContext, split};
use emob_core::{Direction, Energy};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::datetime;

let kwh = |s: &str| Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap();

// 10:01 to 10:22 — the boundary at 10:15 falls two thirds of the way through,
// and 7 kWh times two thirds does not terminate.
let series = MeterSeries::new(Direction::Import, vec![
    MeterReading::new(datetime!(2026-01-02 10:01 +1), kwh("0"), Direction::Import, ReadingContext::TransactionBegin),
    MeterReading::new(datetime!(2026-01-02 10:22 +1), kwh("7"), Direction::Import, ReadingContext::TransactionEnd),
])?;

let split = split::into_quarter_hours(&series)?;
assert!(split.conserves());        // exactly 7 kWh, across two slots
assert!(!split.fully_measured());  // …and it says it had to interpolate
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What it guarantees

- **Conservation, exactly.** The slots of every split sum to the session total.
  A unit test checks 144 sessions chosen for awkward ratios, and a property test
  checks generated sessions with arbitrary cuts and idle intervals.
- **Provenance on every slot.** Each slot says whether its energy was measured,
  held flat, or interpolated.
- **Import and export never net.** 18 kWh drawn and 5 kWh returned is 18 and 5,
  never 13.
- **No raw card UIDs.** `TokenRef` accepts only a 256-bit keyed digest.
- **An ended session stays ended.** No transition, second end or late meter
  series follows it.
- **No I/O, no clock.** Every instant is an argument, so a two-year-old session
  splits today exactly as it did then.

## How the quarter-hour split works

Splitting each slot independently leaves a sum a few milliwatt-hours off, and
the usual fix pushes the difference into the last slot — misattributing energy
to whoever's balance group held it. Instead the split computes the cumulative
register value at each boundary once and takes differences:

```text
slot[i] = cumulative(boundary[i+1]) − cumulative(boundary[i])
```

Every interior boundary cancels, so the sum is `cumulative(end) −
cumulative(start)`. Because that holds whatever a boundary was rounded to, an
imprecise boundary would hide in a correct total; interpolation therefore goes
through `emob_core::apportion`, which multiplies before dividing and rounds to
twelve places (a nanowatt-hour).

### A slot carries both its settlement period and its measured window

A session whose readings begin at 10:07 settles its first slot in the quarter
hour beginning 10:00 `[A6 §IV.1]`, while its readings cover only 10:07–10:15.
`Slot` states both:

```rust
assert_eq!(split.slots[0].quarter_hour.start(), at("10:00"));
assert_eq!(split.slots[0].from, at("10:07"));
assert!(!split.slots[0].covers_the_whole_quarter_hour());
```

### Cuts at state changes, not only at the grid

The quarter hour is where energy settles, not where the price changes:
`[AFIR Art. 5(4)]` lets a fast charger price connected-but-not-charging time
per minute. `Session::split` also cuts at the session's own state changes, and
holds the register flat across `Session::idle_intervals()` so energy is spread
only over time spent charging:

```rust
let split = session.split(Direction::Import)?;           // grid + state changes + idle
let split = split::into_periods(&series, &cuts, &idle)?; // the primitive underneath
```

If the register moved across a gap with no charging time at all, the line is
drawn anyway and `emob-cdr` reports the contradiction.

### Market series are labelled by period end

German meters and MSCONS load profiles name 00:00–00:15 the **00:15** period
`[PTB-A 50.7 §3.1.7.2]`; `QuarterHour` names it by its start. The conversion
happens once:

```rust
let series = split.market_series();   // each period labelled by its end
```

### Daylight-saving days have 92 or 100 periods

A quarter hour is an instant plus fifteen minutes; every civil UTC offset in use
is a whole number of quarter hours, so no code counts to 96. The market does
count, and a balance-group submission is validated on how many periods a day
holds `[A6 §IV.1]`:

```rust
let berlin = TimeZone::new("Europe/Berlin")?;
QuarterHour::periods_in_local_day(&berlin, date!(2026-06-15));  // Some(96)
QuarterHour::periods_in_local_day(&berlin, date!(2026-03-29));  // Some(92)  spring forward
QuarterHour::periods_in_local_day(&berlin, date!(2026-10-25));  // Some(100) autumn fold
```

## Slot provenance: measured, held or interpolated

```rust
assert!(!split.fully_measured());
for slot in split.interpolated() {
    // derived, not measured
}
```

| `Provenance` | Meaning | `fully_measured()` |
|---|---|---|
| `Measured` | A `Sample.Clock` reading landed on the boundary | ✅ |
| `Held` | Register carried flat across an interval the session says nothing flowed in | ✅ |
| `Interpolated` | Derived assuming constant power across a gap | ❌ |

A tapering charge curve over-allocates to the later side of an interpolated
gap, so the assumption travels with the number. A `Sample.Periodic` reading
that happens to fall on `:15:00` does not count as measured.

Two readings of one register at one instant that disagree are refused with
`MeterError::ContradictoryReading`; an identical duplicate passes.

## Authorisation paths and identification strength

| `AuthPath` | Contract-free | Strongest plausible identification |
|---|---|---|
| `AdHoc` | ✅ (the one AFIR requires) | trusted — no contract to certify |
| `PlugAndCharge` | | secure — `ISO15118_PNC` `[OCMF Tab. 15]` |
| `LocalList` | | secure — a secured RFID card, `RFID_PSK` `[OCMF Tab. 13]` |
| `Roaming`, `RemoteCommand` | | certified — `OCPP_CERTIFIED` `[OCMF Tab. 14]` |
| `AutoCharge` | | hearsay — a MAC address off the wire |

`strongest_plausible_level()` is a ceiling read off OCMF tables 13–16, graded on
the scale of `[OCMF Tab. 11]`. It lets `emob-cdr` flag a session claiming Plug &
Charge whose signed record reports a bare RFID UID: under-reporting is
conservative, over-reporting is a contradiction. `AutoCharge` (vehicle MAC
address, unauthenticated and spoofable) is kept distinct from ISO 15118 Plug &
Charge.

## Tokens are never stored raw

```rust
TokenRef::new("1F2D3A4F5506C7")   // Err: that is an RFID UID
TokenRef::new("NLTNM000122045U")  // Err: that is an eMAID
TokenRef::new(&keyed_digest)      // Ok: 64 lowercase hex characters
```

A card UID is a lifelong identifier of something a person carries; storing it on
every session row builds a movement profile.

## Session state history

```rust
session.transition_to(SessionState::SuspendedByVehicle, at(40))?;
session.end(at(70), EndReason::Local)?;
session.attach_series(more)?;   // Err: AlreadyEnded

assert_eq!(session.state_at(at(50)), Some(SessionState::SuspendedByVehicle));
assert_eq!(session.activity_throughout(at(45), at(60)), Some(Activity::Parked));
```

Transitions dated before their predecessor are refused, so the history always
reads as intervals. `activity_throughout` returns an `emob_core::Activity`:

| `SessionState` | `Activity` | Priced as |
|---|---|---|
| `Charging` | `Charging` | charging time |
| `SuspendedByVehicle` (OCPP `SuspendedEV`) | `Parked` | occupancy |
| `SuspendedByOperator` (OCPP `SuspendedEVSE`, `[EnWG §14a]` dimming, faults) | `Withheld` | nothing |

The split follows OCPI 2.3.0's definition of parking time as time the vehicle
is not requesting power `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`. `Pending`
and `Ended` are also connected-and-not-charging, so every period is asked "what
was it doing", never "was it marked suspended".

## The charging success rate

The industry reports *uptime*; a driver experiences *success* — did plugging
in end in a charge? `success` counts it from the same facts that bill, so
nobody can argue with the numerator: an attempt succeeds when its record
**settles**, and a record settles once.

```rust
use emob_session::success::{Cause, Ending, Tally};

let mut tally = Tally::default();
tally.record("CP-1", Ending::Billed);
tally.record("CP-2", Ending::Failed(Cause::Evidence));
tally.estate.percent();   // Some(50.0) — exact, half-even to one place
tally.worst(1);           // [("CP-2", …)]
```

| Item | Meaning |
|---|---|
| `success::Cause` | why an attempt failed, by owner: `Authorisation` (provider or card), `Evidence` (`[MessEG §33]` refused the energy), `StationFault`, `Cancelled` |
| `success::cause_of_refusal` | the cause, from how the station ended the session and whether anything was measured |
| `success::Ending` | `Billed`, or `Failed(Cause)` |
| `success::Rate` | `attempted`, `billed`, failures by cause; `percent()` is `None` before the first attempt — a point nobody tried is not 100 % reliable |
| `success::Tally` | the estate's rate and each point's; `record` is called once per fact in journal order, so a replay is the same tally; `worst(n)` names the weakest points |

## The MiSpeL month: levy relief for bidirectional charging

A bidirectional charge point draws energy from the grid and feeds some of it
back. Under the Bundesnetzagentur's *MiSpeL* Festlegung (618-25-02, adopted
01.10.2026) the part fed back out of a store — and a bidirectional point is one
`[MiSpeL A1 3.2.5]` — relieves the same amount of the site's grid draw of levies.
Net too much and the site keeps levies it owed; net too little and it pays
twice. `mispel` does the arithmetic on quarter-hour values, and only the **levy**
side: the market-premium side applies only to a premium-supported plant.

| Option | Period | Function |
|---|---|---|
| *Abgrenzungsoption* (Anlage 1) | calendar month or *Rumpfmonat* | `mispel::month`: Basisfall A2 (grid meter Z1 and a meter Z2 behind the points, formulas (1)–(21)) or Sonderfall A11 (Z1 only) |
| *Pauschaloption* (Anlage 2) | calendar year or *Rumpfjahr* | `mispel::pauschal_year`: Basisfall P2, factor 0,2 for a charge point |

`mispel::governed` turns the Tenor into the one window a settlement may cover:

- in effect from the publication on **21.10.2026** `[MiSpeL Tenor 8]`;
- until **30.09.2027** only with the grid and metering operators' declared
  consent `[MiSpeL Tenor 9]`;
- the *Pauschaloption* only from the month after the European Commission's
  state-aid approval — and not at all while there is none, because the
  Festlegung makes it conditional on that approval;
- a first assignment may start mid-month, and the rest of the month is a
  *Rumpfmonat* that takes the calendar month's place `[MiSpeL A1 11]`; an
  installation already assigned switches only on the first of a month.

```rust
use emob_session::mispel::{self, Choice, Conditions, Refusal};
use time::macros::date;

let conditions = Conditions {
    assigned_from: date!(2026-10-01),
    first_assignment: true,
    operator_consent: true,
    state_aid_approved_on: None,
};

// October 2026 is governed from 21.10. on: a Rumpfmonat.
let october = mispel::governed(Choice::Abgrenzung, &conditions, date!(2026-10-01), date!(2026-10-31))?;
assert_eq!((october.first, october.rumpf), (date!(2026-10-21), true));

// The Pauschaloption is refused until the state-aid approval is known.
assert_eq!(
    mispel::governed(Choice::Pauschal, &conditions, date!(2026-01-01), date!(2026-12-31)),
    Err(Refusal::NoStateAid),
);
# Ok::<(), Box<dyn std::error::Error>>(())
```

A month is **refused by name** (`mispel::Refusal`, each naming its rule) rather
than netted wrongly when:

- a point the declared metering concept does not cover fed back — a discharge
  no approved Formelsatz sees cannot be netted;
- a covered quarter hour was interpolated, not measured `[MiSpeL A1 3.2.1]`;
- the grid meter series misses a quarter hour of the window or holds one twice
  — a missing quarter hour of draw is levied draw that vanishes;
- an installation already assigned switches mid-month;
- consent is missing before 01.10.2027, or the *Pauschaloption* has no
  state-aid approval;
- the *Pauschaloption* is asked for without solar, or with more than 30 kWp.

Draw and feed-in stay apart until the formula takes the minimum, every
intermediate is kept with the Festlegung's own number (`mispel::Month`,
`mispel::A2Figures`, `mispel::Year`, `mispel::Limits`), and the German legal
calendar counts the 92- and 100-quarter days of a clock change. The tests
reproduce Beispielrechnungen 1 and 2 of Anlage 1 and the two Rumpfjahre of
Anlage 2 to the digit, and a property test over 500 seeded months checks that
relieved and levied draw add up to the grid draw and that no figure exceeds the
one it is bounded by.

## API at a glance

| Item | Purpose |
|---|---|
| `MeterReading`, `MeterSeries`, `ReadingContext` | A validated, cumulative register series |
| `split::into_quarter_hours`, `split::into_periods` | The conserving split |
| `SessionSplit`, `Slot`, `Provenance` | The result, with provenance |
| `SessionSplit::market_series` | Periods labelled by their end, for the market |
| `Session`, `SessionState`, `StateChange`, `EndReason` | The timestamped state machine |
| `Session::split`, `idle_intervals`, `activity_throughout` | Questions a CDR asks |
| `Authorization`, `AuthPath`, `TokenRef` | How the session started, without raw UIDs |
| `success::Tally`, `success::Rate` | The charging success rate from the ledger that bills |
| `mispel::month`, `mispel::pauschal_year`, `mispel::governed`, `mispel::Refusal` | The MiSpeL month and year, and why one is refused |

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `serde` | yes | `Serialize`/`Deserialize` for every public type |

## Standards and sources

BNetzA BK6-20-160 Anlage 6 (market communication and balancing), PTB-A 50.7, OCMF tables 11 and 13–16, OCPP 2.0.1
transaction events, OCPI 2.3.0 charging periods, AFIR Art. 5(4), EnWG § 14a,
and the Bundesnetzagentur's MiSpeL Festlegung 618-25-02 of 01.10.2026 (Tenor,
Anlage 1 and Anlage 2).

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. See
[sessions and settlement](https://hupe1980.github.io/emob/docs/settlement/) on
the documentation site and the [root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
