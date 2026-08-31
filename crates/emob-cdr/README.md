# emob-cdr

Charge detail records: the claim two companies settle against. Built so they
cannot fail their own arithmetic, accepted exactly once, and validated without
ever being silently repaired.

```console
cargo add emob-cdr
```

## A CDR is a claim, not a session with a total on it

A session is what happened. A CDR is a claim about it, sent to somebody who was
not there and who will pay against it. Three things follow:

**It carries its own arithmetic.** The periods sum to the total, exactly,
checked at construction — because the recipient will check, and finding out then
costs a dispute.

**It names its evidence.** Every CDR built here references the signed records it
rests on by content digest, so *which meter values is this €14.46 made of* is
answerable years later.

**It is immutable.** A correction is a new CDR that supersedes the old one, so
sender and recipient can never hold different versions of one id.

```rust
let cdr = CdrBuilder::from_session(&session, Direction::Import)?
    .key(party, "cdr-1".parse()?)
    .evidence(evidence_ref)
    .build()?;

assert!(cdr.conserves());
assert!(cdr.fully_measured());
```

## The cross-check nobody runs

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
}
```

Every problem at once, not the first — a partner integration is debugged by
seeing all of what is wrong in one pass. And nothing is mutated: a CDR whose
periods do not sum to its total is never quietly adjusted to sum, because that
would be inventing a number on behalf of somebody who will be invoiced for it.

Findings are separated into blocking and warning. Missing signed evidence is a
**warning**, deliberately: it blocks a German energy invoice under `[MessEG §33]`
and is merely notable elsewhere, so the decision belongs to the billing layer
that knows which regime applies. Reporting it as blocking here would make this
crate refuse perfectly lawful settlement outside Germany.

## No I/O, no clock

The ledger is in memory and persisting it is a service's job, so a month of
roaming traffic replays as a unit test.

## License

MIT OR Apache-2.0.
