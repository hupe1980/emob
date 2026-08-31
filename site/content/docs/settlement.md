+++
title = "Sessions and settlement"
weight = 3
description = "The quarter-hour split that conserves energy exactly, why interpolated slots say so, and how a CDR is accepted once without letting a partner restate a settled number."
+++

# Sessions and settlement ✅

A session is what happened. A CDR is a claim about it, sent to somebody who was
not there and who will pay against it. Between the two sits arithmetic that has
to be exactly right, because the recipient will check it.

## The quarter-hour split

Germany's pass-through model settles a charging session against the quarter
hours it touched: the operator assigns each quarter hour's energy to the balance
group of the supplier the driver chose `[A6 §IV.1]`. A session running 10:01 to
10:22 touches two of them, and the boundary at 10:15 falls two thirds of the way
through.

Seven kilowatt-hours times two thirds does not terminate.

### Why the obvious approach is wrong

Compute each slot independently and the sum comes out a few milliwatt-hours
short of the session total, because rounding happened once per slot. The usual
fix is to shove the difference into the last slot — which silently misattributes
energy to whoever held 10:15.

### What this does instead

Compute the **cumulative** value at each boundary once, and take differences:

```text
slot[i] = cumulative(boundary[i+1]) − cumulative(boundary[i])
```

The sum telescopes. Every interior boundary appears once positive and once
negative and cancels exactly, whatever it was rounded to. What remains is
`cumulative(end) − cumulative(start)` — the session total, to the last digit,
always.

```rust
let split = split::into_quarter_hours(&series)?;
assert!(split.conserves());   // for every session, whatever the ratio
```

A test runs 144 generated sessions — six start offsets × six durations × four
totals, chosen for awkward ratios — and asserts it on every one. A six-hour
session with readings only at its ends produces 24 slots, every interior
boundary interpolated, and still sums exactly.

## Every slot says where its number came from

```rust
assert!(!split.fully_measured());
for slot in split.interpolated() { /* … */ }
```

`Measured` means a `Sample.Clock` reading landed on that boundary.
`Interpolated` means the number was derived by assuming constant power across a
gap — which a tapering charge curve does not deliver. A car at 80 % state of
charge draws far less at the end of a gap than at its start, so a straight line
over-allocates to the later side, and the error lands on whichever supplier held
the boundary.

That assumption therefore travels with the number, all the way onto the CDR the
partner receives, rather than being forgotten at the point it was made.

### A coincidence is not a measurement

A `Sample.Periodic` reading that happens to fall on `:15:00` does **not** count
as measured. The station chose that instant for its own reasons, and treating
the coincidence as clock-aligned reports a settlement as authoritative on a day
the phase drifted. This is why OCPP's `AlignedDataInterval = 900` matters, and
why the two reading contexts are kept apart.

### No daylight-saving branch

A quarter hour is an instant plus fifteen minutes of real time. Every civil UTC
offset in the world is a whole number of quarter hours, so a UTC quarter-hour
boundary is a local one everywhere — and the 92- and 100-slot days of a clock
change are simply days with fewer or more instants in them. Nothing counts to
96.

## Authorisation paths are not interchangeable

| Path | Contract-free | Strongest honest identification |
|---|---|---|
| `AdHoc` | ✅ — the one `[AFIR Art. 5(1)]` requires | trusted |
| `PlugAndCharge` | | secure |
| `Roaming`, `RemoteCommand` | | trusted |
| `LocalList`, `AutoCharge` | | hearsay |

`AutoCharge` recognises a vehicle by its MAC address. It is not a standard, not
authenticated and trivially spoofable, and it is kept rigorously apart from Plug
& Charge — the two are constantly conflated in marketing and must never be
conflated in evidence.

### The cross-check nobody runs

A session records *how* it was authorised. The signed meter record states *how
strongly* the driver was identified `[OCMF Tab. 11]`. Two statements about one
event, and they can disagree:

```rust
CdrBuilder::from_session(&ad_hoc_session, Direction::Import)?
    .key(party, id)
    .evidence(secure_evidence)
    .build()
// Err: the session claims ad-hoc authorisation, which supports at most
//      trusted identification, but the signed record reports secure
```

When they disagree, the one with a signature behind it is the one to believe, so
the CDR is refused rather than billed at the stronger claim's tariff.
Under-reporting is fine — a station being conservative is not a fault.

## Tokens are never stored raw

```rust
TokenRef::new("1F2D3A4F5506C7")   // Err: that is an RFID UID
TokenRef::new(&keyed_digest)      // Ok: 64 lowercase hex characters
```

A card UID is a lifelong identifier of a physical object a person carries.
Storing it on every session row builds a movement profile no charging platform
needs, so the type refuses anything that is not a 256-bit digest.

## A retransmission is not a conflict

Roaming transports retry. A partner that does not get a `200` in time sends the
CDR again. The usual handling is an upsert keyed on the CDR id, which is wrong
in both directions:

```rust
ledger.accept(cdr.clone());  // Stored
ledger.accept(cdr.clone());  // Duplicate — one session, one invoice line
ledger.accept(restated);     // Conflict { difference: "total energy 18.000 kWh → 118.000 kWh" }
```

A **retransmission** must not produce a second invoice line. A **different**
record under the same id is not a retry — it is a partner restating a settled
number, and an upsert accepts that without a sound. The original is left
untouched and a human is told what moved.

The key is the `(party, id)` pair, never the bare id: OCPI makes a CDR id unique
per party, so two CPOs may each have a CDR `1`.

## Validation reports, and never repairs

A CDR this workspace builds cannot fail its own arithmetic. One that arrives
from a partner was built by somebody else's code.

```rust
let report = validate(&incoming);
for reason in report.reasons() { eprintln!("{reason}"); }
// the periods sum to 18.000 kWh but the record claims 20.000 kWh
// the period at 2026-01-02 10:15 is out of time order
```

Every problem at once, not the first — a partner integration is debugged by
seeing all of it in one pass. And nothing is mutated: a record whose periods do
not sum to its total is never quietly adjusted, because that would be inventing
a number on behalf of somebody who will be invoiced for it.

Missing signed evidence is a **warning** rather than a block, deliberately. It
blocks a German energy invoice under `[MessEG §33]` and is merely notable
elsewhere, so the decision belongs to the billing layer that knows which regime
applies. Blocking here would refuse lawful settlement outside Germany.

## What is not here yet 📐

Rating — turning a settled quarter-hour split into money — is `emob-tariff`, and
the OCPI/OICP wire translation is `emob-roam`. Both are designed and not built.
