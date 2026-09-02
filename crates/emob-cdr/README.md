# emob-cdr

Charge detail records: the claim two companies settle against. Built so they
cannot fail their own arithmetic, priced from their own periods, accepted
exactly once, and validated without ever being silently repaired.

```console
cargo add emob-cdr
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Sessions and settlement](https://hupe1980.github.io/emob/docs/settlement/)**.
The signatures are on [docs.rs](https://docs.rs/emob-cdr).


## A CDR is a claim, not a session with a total on it

A session is what happened. A CDR is a claim about it, sent to somebody who was
not there and who will pay against it. Four things follow:

**It carries its own arithmetic.** The periods sum to the total, exactly,
checked at construction — because the recipient will check, and finding out then
costs a dispute.

**It names its evidence.** Every CDR built here references the signed records it
rests on by content digest, so *which meter values is this €8.82 made of* is
answerable years later.

**It carries its money, and the money comes from the same periods.**
`rated_with` prices the record's own charging periods, so every euro traces to a
quarter hour that traces to a signed reading. Rating in a separate service is how
a CDR and its invoice line come to disagree about one session.

**It is immutable.** A correction is a new CDR that supersedes the old one, so
sender and recipient can never hold different versions of one id.

```rust
let cdr = CdrBuilder::from_session(&session, Direction::Import)?
    .key(party, "cdr-1".parse()?)
    .evidence(evidence_ref)
    .rated_with(&tariff)
    .build()?;

assert!(cdr.conserves());
assert!(cdr.fully_measured());
assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
```

Because the periods *are* the rating periods, a tiered tariff prices the quarter
hours the split produced — and a receiving party re-rating the record reads
exactly the slices the issuer did, cut at the tariff's own thresholds so the
total does not depend on the slicing.

## The slot and the window are different facts

A session that starts at 10:07 has its first period reported under the quarter
hour beginning **10:00** — that is the settlement period the energy belongs to
`[A6 §IV.1]` — while the period itself runs from **10:07**, because that is when
the session began. Collapsing the two into one timestamp produces a record whose
first period starts before its own session, which every partner's validator
flags and none can fix.

```rust
assert_eq!(cdr.periods[0].quarter_hour.start(), at("10:00"));
assert_eq!(cdr.periods[0].start, at("10:07"));
```

The window comes from the **slot's own readings**, not from the quarter hour
clamped to the session. The two differ whenever a session is wider than its
meter series — a station that authorises at 10:00 and sends its first meter
value at 10:20 — and clamping would claim twenty minutes of measurement that
never happened.

## A span too short to resolve is not a span to bill

`[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen Zeitspanne werden
nicht für Abrechnungszwecke verwendet", and the shortest measurable span of a
conforming clock may be no worse than sixty seconds.

```rust
CdrBuilder::from_session(&thirty_second_session, Direction::Import)?
    .rated_with(&per_minute_tariff)
    .build()
// Err: the session lasted 30 s, below the 60 s its clock can resolve
//      [REA 6-A §3.1] … The energy is unaffected — price this session per kWh
```

The mirror of the unsynchronised-clock rule, arriving from the other end: there
the clock cannot be *placed* `[OCMF Tab. 19]`, here the span cannot be
*resolved*. A station whose type approval states a better figure says so with
`.clock(ClockResolution::stated(…))`; until it does, the builder assumes the
worst case the regulation permits, because it has not been told otherwise.

## Occupancy is a fact the record states

A period that moved no energy is **not** therefore occupancy. A car at 100 %
state of charge can leave a quarter hour at exactly `0.000 kWh` while the
session's own state machine says `Charging`, and pricing that quarter hour at
the occupancy fee `[AFIR Art. 5(4)]` permits charges a driver for parking they
were told was charging.

```rust
assert!(cdr.periods[1].energy.is_zero());
assert!(cdr.periods[1].charging);        // a taper, not an occupancy
```

So `ChargingPeriod::charging` is a **stated fact**, taken from the session
history — the same history that already refuses a record whose meter and state
machine disagree — and `validate` blocks a partner's record whose two halves
contradict each other. A check fed by an inference is not a check.

## …and occupancy is time nobody metered

The meter series spans the readings; the session spans the parking space. A car
that finishes charging at 11:00 and is collected at 13:00 leaves two hours the
split knows nothing about — and those two hours are precisely what
`[AFIR Art. 5(4)]`'s occupancy fee exists for.

A record that stops at the last reading cannot bill them; one that stretches the
last reading over them bills energy that did not flow. So the builder fills the
gap with periods carrying **no energy**, marked `Interpolated` because the zero
is an assumption rather than a measurement, and split on the same quarter-hour
grid as everything else.

**Their `charging` flag is read too.** "The meter said nothing" and "the vehicle
was not charging" are different claims, and only the second is one the operator's
own records make — a station that stops sending `MeterValues` while its own state
machine still says `Charging` would otherwise be billed an occupancy fee per
minute for time it says the car was taking energy. So the fill is built where
the session history is in scope, cuts at its state changes as well as at the
grid, and asks `Session::charging_throughout`, which is **not** the negation of
`suspended_throughout`: `Pending` and `Ended` are neither, and both are exactly
the "connected and not charging" the fee prices.

And when the meter and the state machine disagree — energy across a quarter hour
the session logged as suspended from end to end — the builder refuses rather than
picking one, because guessing is how a driver is billed for a charge the
operator's own log says never happened.

## The cross-checks nobody runs

### Who was charging

A session records *how* it was authorised. The signed meter record states *how
strongly* the driver was identified. Those are two statements about one event,
and they can disagree:

```rust
// The session says ad-hoc — a card at the point. The signed record claims the
// identity was established by a secure feature, which ad-hoc cannot do.
CdrBuilder::from_session(&ad_hoc_session, Direction::Import)?
    .key(party, id)
    .evidence(secure_evidence)
    .build()
// Err: the session claims ad-hoc authorisation, which supports at most
//      trusted identification, but the signed record reports secure
```

When they disagree, the one with a signature behind it is the one to believe —
so the CDR is refused rather than billed at the stronger claim's tariff.
Under-reporting is fine: a station being conservative is not a fault.

The check is only worth anything if nobody can hand it the answer, so the
strength is read off the records rather than passed in:

```rust
let evidence_ref = EvidenceRef::from_evidence(&evidence, "OCMF");
```

It is the **weakest** level any record asserted, because a chain is only as
strong as its weakest claim — and a hand-filled field can be filled with
whatever value makes the record build.

### Whether a duration may be billed at all

OCMF states how far the station's clock can be trusted `[OCMF Tab. 19]` and
flags a time value as unusable separately from an energy one. A tariff charging
per minute — the occupancy fee `[AFIR Art. 5(4)]` permits at 50 kW and above —
is billing a duration:

```rust
CdrBuilder::from_session(&session, Direction::Import)?
    .key(party, id)
    .evidence(evidence_ref)      // the clock was never synchronised
    .rated_with(&occupancy_tariff)
    .build()
// Err: the tariff charges for ParkingTime but the signed records do not support
//      billing a duration … The energy is unaffected — price this session per kWh
```

The energy really is unaffected, so the error names the fix rather than blocking
the session. The same gate runs again in the pre-flight, on records a partner
built.

### Which way the energy went

`[OCMF Tab. 25]` reserves the OBIS range `B0`–`B3` for import and `C0`–`C3` for
export, so the signed register states the direction. A record claiming the other
one is a V2G discharge billed as consumption:

```rust
CdrBuilder::from_session(&session, Direction::Import)?   // the session says draw
    .evidence(evidence_ref)                              // the register says C2
    .build()
// Err: the record claims import but the signed register measured export
//      [OCMF Tab. 25]: import and export never net
```

A register whose code the verifier could not classify states no direction, and
the record is free to claim one.

## Accepted exactly once — and a conflict is not a retry

Roaming transports retry. A partner that does not get a `200` in time sends the
CDR again. The usual handling is an upsert keyed on the CDR id, which is wrong
in both directions:

```rust
ledger.accept(cdr.clone());  // Stored
ledger.accept(cdr.clone());  // Duplicate — one session, one record, one invoice line
ledger.accept(restated);     // Conflict { difference: "total energy 18.000 kWh → 118.000 kWh" }
```

A **retransmission** must not produce a second invoice line. A **different**
record under the same id is not a retry — it is a partner silently restating a
settled number, and an upsert accepts it without a sound. `Acceptance` tells
them apart, and the original is left untouched.

The key is the `(party, id)` pair, never the bare id: OCPI makes a CDR id unique
per party, so two CPOs may each have a CDR `1` and a ledger keyed on the id
alone will drop one of them.

### …and a correction chain has one end

A correction is a *new* CDR, so a ledger holding a session and its correction
holds both. Two records that both supersede one key are two corrections of one
session — both live, neither superseded, and the session billed twice, which is
the failure content equality is checked to prevent arriving one link along.

```rust
ledger.accept(first);                     // Stored
ledger.accept(correction_of(&first));     // Stored
ledger.accept(another_correction_of(&first));
// Forked { supersedes: DE*ABC/1, held: DE*ABC/2 }

let owed: Energy = ledger.live().map(|cdr| cdr.total_energy).sum();
```

`live()` is the set a billing run sums: everything the ledger holds that nothing
else in it supersedes. `iter()` is every record, superseded ones included. A
record that supersedes *itself* is refused for the mirror reason — stored, it
would be superseded by its own presence and billed by nothing — and a correction
that arrives **before** the record it corrects is still stored, because roaming
transports do not order deliveries.

## Validation reports, and never repairs

A CDR this crate builds cannot fail its own arithmetic. One that arrives from a
roaming partner was built by somebody else's code.

```rust
let report = validate(&incoming);
if !report.is_settleable() {
    for reason in report.reasons() { eprintln!("{reason}"); }
    // the periods sum to 18.000 kWh but the record claims 20.000 kWh
    // the period at 2026-01-02 10:15 is out of time order
    // the signed record claims secure identification but the authorisation
    //   path supports at most hearsay
    // the price was computed for 18.000 kWh but the record claims 20.000 kWh
    // the price charges 1800 s of TIME and the record's own periods account
    //   for 900 s
}
```

Every problem at once, not the first — a partner integration is debugged by
seeing all of what is wrong in one pass.

The quantity is checked for the **energy and the minutes**, and the two are not
symmetric. `[AFIR Art. 5(4)]` prices occupancy per minute, so the minutes are
the half of a settlement most often disputed — but charging for *less* time than
the record spans is the ordinary shape of a lawful tariff: an occupancy fee that
begins after four hours prices nothing on a thirty-minute session. So a
shortfall is revenue nobody charged and an excess is money the payer is asked to
accept on the sender's word, and only the excess blocks. And nothing is mutated: a CDR whose
periods do not sum to its total is never quietly adjusted to sum, because that
would be inventing a number on behalf of somebody who will be invoiced for it.

The builder is held to the same rules. A meter series reaching past the
session's own `ended_at` — ordinary, because OCPP delivers `MeterValues`
asynchronously — produces periods outside the session window. The builder refuses
that rather than clamping the window and inventing time nobody can say the driver
was there for; a test asserts the property directly, that **every record this
builder emits passes its own validator**.

Findings are separated into blocking and warning. Missing signed evidence is a
**warning**, deliberately: it blocks a German energy invoice under `[MessEG §33]`
and is merely notable elsewhere, so the decision belongs to the billing layer
that knows which regime applies. Reporting it as blocking here would make this
crate refuse perfectly lawful settlement outside Germany.

Money that was computed for a different quantity than the record states is
**blocking** — it is the settlement fault that costs the most to unwind. It is
raised only where an energy price was actually charged: `quantity_for` returns
zero for a dimension with no lines, so comparing it unconditionally read "this
tariff charges nothing per kWh" as "this price was computed for 0 kWh" and
refused every lawful per-minute tariff below 50 kW. The other case is a warning,
because a dropped energy line looks identical from the record alone. Every
note the rating made is surfaced as a warning, so a minimum charge or a block
rounding reaches the receiving party as a term of the price rather than as a
discrepancy they have to discover.

## No I/O, no clock

The ledger is in memory and persisting it is a service's job, so a month of
roaming traffic replays as a unit test.

## License

MIT OR Apache-2.0.
