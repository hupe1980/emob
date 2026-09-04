# emob-eichrecht

German calibration law (*Eichrecht*) for EV charging, in Rust: the step between
**a signature that checks out** and **a kilowatt-hour somebody may be invoiced
for**.

```console
cargo add emob-eichrecht
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[The Eichrecht chain](https://hupe1980.github.io/emob/docs/eichrecht/)**.
The signatures are on [docs.rs](https://docs.rs/emob-eichrecht).

## The rule this crate enforces

A customer may be billed for a measured value only if they can verify it, long
after the session — `[MessEG §33]`, `[PTB-A 50.7]`, `[REA 6-A]`. Every serious
charging platform treats this as a checkbox; every open-source CSMS ignores it.
Here it is the invariant everything else hangs from:

> **A value that does not verify does not bill.**

And it is a property of the types rather than a convention: the only way to
obtain a billable quantity is `Evidence::billable_energy()`, which returns `None`
whenever anything at all is wrong, and an `Evidence` can only be built by running
the whole check.

## This crate does not implement OCMF

[`ocmf`](https://crates.io/crates/ocmf) does, against the whole **S.A.F.E.
Transparenzsoftware reference corpus** — 256 records from eleven manufacturers,
705 readings — with **OpenSSL's verdict on each one** as an independent oracle,
plus a published 162-case conformance suite. What that measurement says about the
specification is the argument for not writing it twice:

| Measured | Count | What a hand-rolled parser does with it |
|---|---:|---|
| Records omitting `MS`, which `[OCMF Tab. 3]` marks `1..1` | 229 / 256 | strict cardinality rejects nine real records in ten |
| Readings omitting `TM`, relying on carry-forward | 205 / 705 | a reading read independently of its neighbours is wrong |
| Readings writing `RV` as a JSON **string** | 23 | a typed deserialiser refuses them |
| Records whose payload is pretty-printed | 9 | any re-serialisation destroys the signature |
| OBIS codes written the way `[OCMF Tab. 25]` specifies | **0** | a canonical-form check refuses every record ever sent |

Not one record in the corpus writes the OBIS code the way the table does.

The format is not here, and neither is the sequence check
`[OCMF §Signing and Verification Process]` assigns to a check component, nor the
transparency container. What is here is the one thing `ocmf` refuses, in its own
words:

> It reports findings. Whether a session may be invoiced depends on tariffs, on
> a key registry binding each record to *this* charge point, and on law — none
> of which is in scope.

A format crate can say a record is missing. Only law can say what a missing
record costs you, and the answer is **not one boolean**.

## The questions, kept apart

Conflating these is how a "verified" session turns out to be a signed fragment
of a session somebody edited:

| Question | Answered by | Whose |
|---|---|---|
| Did *this key* produce *these bytes*? | `ocmf::verify()` | the format's |
| Are any records missing from the session? | `ocmf::session` | the format's |
| Is this key *this charge point's* key? | `KeyRegistry` | **ours** |
| Which quantity does each failure take away? | `chain::validate()` | **ours** |
| May the *energy* be billed? | `Evidence::billable_energy` | **ours** |
| May the *duration* be billed too? | `Evidence::billable_duration` | **ours** |
| Can the **customer** repeat all of that? | `transparency` | both |

## Four quantities, not one

OCMF states them separately and so does this. A record carries `EF` flags for
energy (`E`) and time (`t`) apart `[OCMF Tab. 7]`, states its clock's
trustworthiness separately again `[OCMF Tab. 19]`, and states how the user was
identified separately from both `[OCMF Tab. 11]`. Collapsing them into one
boolean throws away exactly the distinctions the format was shaped to carry:

- a session on an **unsynchronised clock** has perfectly good energy and a
  duration nobody can defend — a per-kWh tariff bills it and a per-minute tariff
  must not;
- a session whose **identification failed** has good energy and nobody to bill
  it to.

```rust
let evidence = Evidence::assemble(&records, &registry, session_start);

evidence.billable_energy();          // Some(0.268 kWh) — the register held up
evidence.billable_duration();        // None — the clock was only informative
evidence.identification_strength();  // the weakest level any record asserted
evidence.direction();                // Import, off the OBIS code the meter signed
```

`ChainFinding::disqualifies()` is that mapping, and it is **total** over
`ocmf::session::Finding` with a fallback of `Both`: a fault a later release of
that crate adds must not be able to widen what this build bills by being
unrecognised.

### …and five rules that are about billing rather than about the format

| Rule | Source | Disqualifies |
|---|---|---|
| The billed register is not one `[OCMF Tab. 25]` reserved and never defined | `[OCMF Tab. 25]` | energy |
| The reading is in an energy unit at all — `mOhm` is a lawful `RU` | `[OCMF Tab. 7, RU]` | energy |
| Cable-loss compensation is reported against an accumulation register | `[OCMF Tab. 7, CL]` | both |
| …and is reset at `TX=B`, so the session's own loss is `CL_end` | `[OCMF Tab. 7, CL]` | both |
| The identification level does not change mid-session | `[OCMF Tab. 11]` | both |

## The key is this charge point's, or it proves nothing

`[OCMF §Relation of Serial Numbers]` puts the binding between a signing component
and its key **out of band** — a type approval, a provisioning run — so a key that
arrives beside a record proves only that whoever sent it owns a private key.

`KeyRegistry` holds the binding, and its windows are **half-open**. A station
whose key is replaced has two keys over its life, and a record from before the
swap must still verify years later:

```rust
registry.insert(component, RegisteredKey::valid_between(key, from, until)?)?;
```

With two inclusive bounds a meter exchanged at midnight has two keys covering
that instant and the answer depends on insertion order. `[from, until)` makes
consecutive windows partition the timeline exactly, which is what a key history
is — and an *empty* window is refused, because a window covering no instant reads
exactly like a component nobody registered.

The two failures are kept apart, because an operator acts on the difference:
nothing registered is a provisioning gap; a key whose window has closed is a key
that was replaced without the replacement being registered.

## The transparency file

`[MessEG §33]` does not require a measured value to be correct. It requires the
affected party to be able to **check** it — so a platform that verifies
internally and reports "verified" has satisfied nobody. The deliverable is the
container the S.A.F.E. Transparenzsoftware reads.

Writing it is `ocmf::xml`'s job. What is this crate's is **which records go in**:
only the ones whose signatures verified against a *registered* key, because a
container built from raw records hands a driver a file whose verifier says
"valid" about a record no registry ever vouched for.

```rust
let xml = transparency::to_xml(&evidence)?;   // only what held up
let back = transparency::from_xml(&xml)?;     // and it reads back, for a dispute
```

The part that is easy to get wrong is not the schema, it is `transactionId`: the
verifier **groups** by it and then wants exactly one `Transaction.Begin` and one
`Transaction.End` per group. A writer that numbers its records `1, 2, 3…`
produces a schema-valid file it refuses — and a writer that puts an id on a
record carrying *both* markers makes it look for a partner that does not exist.
`ocmf::xml` groups the way S.A.F.E.'s own 257 reference values are grouped,
counted: 223 of them are a whole transaction in one record and none carries an
id.

## The records also state the shape of the session

`[OCMF Tab. 7, TX]` names ten markers. Most are structure, and two are facts
about the session nothing else in the evidence states:

- `S` — "Suspended = Transaction active, but currently not charging";
- `T` — a tariff change.

Both are exactly the intervals money turns on. `[AFIR Art. 5(4)]` prices the time
a vehicle is connected and *not* charging per minute, and the alternative source
for that interval is OCPP's `chargingState` — a protocol field, asserted by the
same party that issues the invoice.

```rust
evidence.suspended_intervals();      // what the meter signed, not what the CSMS said
evidence.tariff_change_instants();   // `[PTB-A 50.7 §3.1.7.2]` wants these on a grid boundary
evidence.compensated_loss();         // how much of the register was cable `[OCMF Tab. 7, CL]`
```

Empty for the ordinary station that never emits `S` — the marker is optional — so
it is evidence *for* a fee where it exists, never a precondition of one.

**And all three leave this crate.** `emob-ocpp` compares the suspensions against
the protocol's own account of them; `emob-cdr` refuses a record whose signed
records mark a tariff change **inside** a session it prices with the one version
in force when that session started `[AFIR Art. 5(4)]`, and carries the cable loss
onto the record so a partner disputing the energy — and a customer
`[REA 6-A §3.2]` entitles to know what is inside a measured value — can be told.
Each of the three was computed here and consulted nowhere until it was.

## Scope

Four of the seven algorithms of `[OCMF Tab. 22]` verify in pure Rust, secp192r1
included: the eBZ LD3 sample the Transparenzsoftware ships is signed
`ECDSA-secp192r1-SHA256`, so a verifier without it cannot check a common German
meter. The remaining three — the brainpool pair and secp192k1 — have no audited
pure-Rust arithmetic; `ocmf` reaches them through OpenSSL, which a crate that
promises to open no socket and read no file may not link, so here they are
recognised and refused **by name** rather than silently failing.

## No I/O, no clock

Nothing here opens a socket, reads a file or asks the time. The key registry is
handed in already populated and every instant is an argument, so a whole fleet's
verification runs as a deterministic unit test — and a dispute from two years ago
is replayed exactly as it happened.

`cargo xtask check-graph` enforces the other half of that: no clock, socket or
database may appear in this crate's *dependency* graph either, because the purity
guard greps this workspace's source and cannot see into a dependency.

## A cable loss belongs to the register it was measured on

`CL` is a property of the register beside it `[OCMF Tab. 7, CL]` — "given in the
same unit as `RV`", accumulating across the transaction, reset at `TX=B`. A meter
reporting two registers reports two `CL` series, and taking the last value seen
against the first `TX=B` seen crosses them: `CL_end(C2) − CL_begin(B2)` is not a
quantity, and nothing in the format says those two numbers are about the same
thing.

So the register is chosen first and the compensation read on **that** one; a
chain with no billable register reports none rather than one belonging to
nothing. The two `CL` findings are raised once per register, not once per
reading — a quarter-hourly session carries ninety-six of them.

## License

MIT OR Apache-2.0
