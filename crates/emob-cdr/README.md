# emob-cdr

Charge detail records (CDRs) for charge point operators and e-mobility
providers: built so they cannot fail their own arithmetic, priced from their own
periods, accepted exactly once, and validated without being silently repaired.

[![crates.io](https://img.shields.io/crates/v/emob-cdr.svg)](https://crates.io/crates/emob-cdr) [![docs.rs](https://img.shields.io/docsrs/emob-cdr)](https://docs.rs/emob-cdr) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-cdr.svg)](#license)

A session is what happened; a CDR is a claim about it, sent to a company that
was not there and will pay against it. This crate builds CDRs whose periods sum
to their total, whose price comes from those same periods, and whose signed
evidence is referenced by digest. It accepts incoming CDRs exactly once, telling
a retransmission from a restatement, and pre-flights a partner's record by
reporting every problem at once.

```console
cargo add emob-cdr
```

## Example

```rust
use emob_cdr::{CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::Direction;

let evidence_ref = EvidenceRef::from_evidence(&evidence, "OCMF");

let cdr = CdrBuilder::from_session(&session, Direction::Import)?
    .key(party, "cdr-1".parse()?)
    .evidence(evidence_ref)
    .rated_with(&tariff)
    .build()?;

assert!(cdr.conserves());
assert!(cdr.fully_measured());
assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");

let mut ledger = CdrLedger::new();
ledger.accept(cdr.clone());  // Stored
ledger.accept(cdr);          // Duplicate — one session, one invoice line
```

## What it guarantees

- **Its own arithmetic holds.** Periods sum to the total exactly, checked at
  construction.
- **Every euro traces to a signed reading.** `rated_with` prices the record's own
  charging periods, so the CDR and its invoice line cannot disagree.
- **It names its evidence** by content digest, so "which meter values is this
  €8.82 made of?" is answerable years later.
- **It is immutable.** A correction is a new CDR that supersedes the old one.
- **The builder and the validator agree.** A property test generates a thousand
  realistic sessions and asserts every record that builds passes `validate`,
  partitions its session, and states the price its own periods produce.
- **Nothing is repaired.** A partner's record that does not add up is reported,
  never adjusted.
- **No I/O, no clock.** The ledger is in memory; persisting it is a service's job.

## The cross-checks between session and signed record

The builder compares the session's own account with what the meter signed. Each
check removes only what it must:

| Check | Source | Fails when | Effect |
|---|---|---|---|
| Energy verified | `[MessEG §33]` | the chain did not verify | refused |
| Direction | `[OCMF Tab. 25]` | the record claims import over an export (`C`) register | refused — import and export never net |
| Identification | `[OCMF Tab. 11]` | the session's auth path supports less than the signed record claims | refused; under-reporting is fine |
| Duration billable | `[OCMF Tab. 19]` | a per-minute tariff over a clock the record does not vouch for | refused, "price this session per kWh" |
| Clock resolution | `[REA 6-A §3.1]` | a per-minute charge on a span below the clock's resolution (default 60 s) | refused, "price this session per kWh" |
| Tariff change | `[OCMF Tab. 7, TX=T]`, `[AFIR Art. 5(4)]` | the meter marks a price change inside a session priced by one version | refused |

Read the identification strength off the evidence with
`EvidenceRef::from_evidence`; it is the weakest level any record asserted and
cannot be filled in by hand. A station with a better clock states it with
`.clock(ClockResolution::stated(…))`, and the record carries it in `Cdr::clock`
so a re-rating uses the same figure.

```rust
CdrBuilder::from_session(&session, Direction::Import)?
    .key(party, id)
    .evidence(evidence_ref)      // the clock was never synchronised
    .rated_with(&occupancy_tariff)
    .build()
// Err: the tariff charges for ParkingTime but the signed records do not support
//      billing a duration … The energy is unaffected — price this session per kWh
```

`EvidenceRef::compensated_loss` carries the cable loss the meter compensated
`[OCMF Tab. 7, CL]`. Nothing is subtracted (it is already inside the register);
it travels because `[REA 6-A §3.2]` entitles the customer to know.

## Charging periods: settlement slot, window and activity

Each `ChargingPeriod` states three facts:

```rust
assert_eq!(cdr.periods[0].quarter_hour.start(), at("10:00")); // settlement period [A6 §IV.1]
assert_eq!(cdr.periods[0].start, at("10:07"));                 // when its readings began
assert_eq!(cdr.periods[1].activity, Activity::Charging);       // stated, not inferred
```

- **The window comes from the slot's own readings**, not from the quarter hour
  clamped to the session, so the record never claims measurement that did not
  happen.
- **Activity is a stated fact** from the session history. A period at
  `0.000 kWh` can still be `Charging` (a full battery tapering), and pricing it as
  occupancy would bill a driver for parking. `validate` blocks a record whose
  energy and activity contradict each other.
- **Not charging is two facts.** OCPI 2.3.0 defines `PARKING_TIME` as time the
  vehicle is not requesting power `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`:

| `Activity` | priced as | energy transferred |
|---|---|---|
| `Charging` | `TIME` | ✅ |
| `Parked` — the vehicle stopped asking | `PARKING_TIME` | |
| `Withheld` — the point stopped offering | *nothing* | |

`Withheld` time still counts in `total_time` and `total_parking_time` (defined on
energy transfer), so those two differ by exactly the withheld time.

### Unmetered occupancy

A car that finishes at 11:00 and leaves at 13:00 occupies the space for two hours
the meter series does not cover — exactly what the occupancy fee
`[AFIR Art. 5(4)]` is for. The builder fills that gap with zero-energy periods
marked `Interpolated`, cut at the session's state changes and the quarter-hour
grid, and asks `Session::activity_throughout` for each. If the meter and the
state machine disagree, the builder refuses rather than picking one.

## Reservations

OCPI prices a reservation from when it is made until charging starts or it
expires `[OCPI 2.3.0]` — before any meter measured anything. `Cdr::reservation`
carries the window, `Cost::reservation` its rating, and both appear in
`total_reservation_cost`. It is a separate rating so reservation minutes and
charging minutes do not compete for one `TIME` dimension. `Cdr::total_cost` adds
both. `validate` blocks two currencies on one record, a priced reservation with
no window, and a reservation billed longer than its window.

## Re-rating a partner's record

An eMSP receives a CPO's CDR and owes its driver a different price:

```rust
let mine = theirs.rerated_with(&retail_tariff)?;
assert_eq!(mine.periods, theirs.periods);      // the same session, by construction
assert!(mine.was_priced_with(&retail_tariff)); // …and it names which tariff
```

`build` and `rerated_with` go through one pricing function, so the gates above
(verified chain, tariff in force at session start, no mid-session version
change, billable duration, clock resolution) apply to records assembled from a
partner's document too. Absent evidence still prices — which legal regime
applies is the billing layer's question — but evidence that is present and
failed does not.

## Accepted exactly once

Roaming transports retry. An upsert keyed on the CDR id is wrong both ways: a
retransmission must not create a second invoice line, and a different record
under the same id is a partner restating a settled number.

```rust
ledger.accept(cdr.clone());  // Stored
ledger.accept(cdr.clone());  // Duplicate
ledger.accept(restated);     // Conflict { difference: "total energy 18.000 kWh → 118.000 kWh" }

ledger.accept(first);                          // Stored
ledger.accept(correction_of(&first));          // Stored
ledger.accept(another_correction_of(&first));  // Forked { supersedes: DE*ABC/1, held: DE*ABC/2 }

let owed: Energy = ledger.live().map(|cdr| cdr.total_energy).sum();
```

- The key is `(party, id)`: OCPI makes a CDR id unique per party only.
- `live()` is what a billing run sums — everything nothing else supersedes;
  `iter()` includes superseded records.
- A record that supersedes itself is refused; a correction arriving before the
  record it corrects is stored, because transports do not order deliveries.

## Validation of incoming CDRs

```rust
let report = validate(&incoming);
if !report.is_settleable() {
    for reason in report.reasons() { eprintln!("{reason}"); }
    // the periods sum to 18.000 kWh but the record claims 20.000 kWh
    // the period at 2026-01-02 10:15 is out of time order
    // the price charges 1800 s of TIME and the record's own periods account
    //   for 900 s
}
```

Every problem is reported in one pass, as blocking or warning:

| Finding | Severity | Why |
|---|---|---|
| Periods do not sum to the total, or are out of order | blocking | the arithmetic is wrong |
| Price computed for more energy or minutes than the record states | blocking | money the payer must take on trust |
| Price for fewer minutes than the record spans | not a finding | an occupancy fee that starts after four hours is lawful |
| `FLAT` charged more than once `[OCPI 2.3.0 §mod_tariffs_tariffdimensiontype_enum]` | blocking | a session fee is per session |
| Missing signed evidence | warning | blocks a German invoice `[MessEG §33]`, lawful elsewhere — the billing layer decides |
| Every note the rating made (minimum charge, block rounding) | warning | a term of the price, surfaced |

Quantities are compared against `Rated::accounted_quantity_for`: the billed
quantity, less the rounding block a `step_size` adds
`[OCPI 2.3.0 §mod_cdrs_step_size]`, plus any quantity no tariff element priced
(free, per `[OCPI 2.3.0 §Tariff]` — how "the first 10 kWh are free" is written).

## API at a glance

| Item | Purpose |
|---|---|
| `CdrBuilder::from_session` … `.build()` | Build a consistent, priced CDR |
| `CdrBuilder::rated_with_history`, `reserved`, `supersedes`, `clock` | Tariff history, reservations, corrections, clock resolution |
| `Cdr::rerated_with`, `was_priced_with`, `total_cost` | Re-rate and inspect |
| `EvidenceRef::from_evidence` | Reference signed evidence by digest |
| `CdrLedger::accept`, `live`, `Acceptance` | Exactly-once acceptance |
| `validate`, `Report`, `Finding`, `Severity` | Pre-flight for a partner's record |

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `serde` | yes | `Serialize`/`Deserialize` for every public type |

## Standards and sources

OCPI 2.3.0 (CDRs, tariffs); OCMF tables 7, 11, 19, 25; MessEG § 33; REA 6-A;
PTB-A 50.7; AFIR Art. 5(4); BNetzA BK6-20-160 Anlage 6.

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. See
[sessions and settlement](https://hupe1980.github.io/emob/docs/settlement/) on
the documentation site and the [root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
