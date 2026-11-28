# emob-eichrecht

German calibration law (*Eichrecht*) for EV charging: decides whether a signed
meter value may be invoiced, for charge point operators and the platforms that
bill for them.

[![crates.io](https://img.shields.io/crates/v/emob-eichrecht.svg)](https://crates.io/crates/emob-eichrecht) [![docs.rs](https://img.shields.io/docsrs/emob-eichrecht)](https://docs.rs/emob-eichrecht) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-eichrecht.svg)](#license)

A customer may be billed for a measured value only if they can verify it, long
after the session `[MessEG §33]`, `[PTB-A 50.7]`, `[REA 6-A]`. This crate makes
that a property of the types: **a value that does not verify does not bill.**
The only way to obtain a billable quantity is `Evidence::billable_energy()`,
which returns `None` whenever anything is wrong, and an `Evidence` can only be
built by running the whole check.

```console
cargo add emob-eichrecht
```

## Example

```rust
use emob_eichrecht::{Evidence, KeyRegistry};

// A `Record` borrows the bytes its signature covers, so the texts outlive it.
let records = raw_records
    .iter()
    .map(|r| ocmf::Record::parse(r))
    .collect::<Result<Vec<_>, _>>()?;

let evidence = Evidence::assemble(&records, &registry, session_start);

match evidence.billable_energy() {
    Some(energy) => println!("bill {energy}"),
    None => for reason in evidence.reasons() {
        eprintln!("blocked: {reason}"); // the session goes to an operator
    },
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What it guarantees

- **Nothing unverified bills.** A bad signature, an unregistered key, a missing
  or substituted record each yield `None`, with a reason.
- **Energy and duration are judged separately.** An unsynchronised clock
  disqualifies a per-minute charge but not a per-kWh one `[OCMF Tab. 19]`.
- **The key must be this charge point's key.** A key that arrives beside a
  record proves only that the sender owns a private key
  `[OCMF §Relation of Serial Numbers]`; `KeyRegistry` holds the binding.
- **The customer can repeat the check.** The transparency file contains only
  records that verified against a registered key.
- **No I/O, no clock.** The registry is handed in populated and every instant is
  an argument, so a two-year-old dispute replays exactly. `cargo xtask
  check-graph` keeps clocks, sockets and databases out of the dependency graph
  too.

## What this crate does, and what `ocmf` does

OCMF — the Open Charge Metering Format, in which a calibrated meter signs each
reading — is parsed and verified by the [`ocmf`](https://crates.io/crates/ocmf)
crate. It is tested against all 256 records of the S.A.F.E. Transparenzsoftware
reference corpus, with OpenSSL as an independent oracle. That corpus shows why a
hand-rolled parser fails on real meters:

| Measured in the corpus | Count | What a naive parser does |
|---|---:|---|
| Records omitting `MS`, which `[OCMF Tab. 3]` marks `1..1` | 229 / 256 | strict cardinality rejects nine in ten |
| Readings omitting `TM`, relying on carry-forward | 205 / 705 | readings parsed in isolation are wrong |
| Readings writing `RV` as a JSON string | 23 | a typed deserialiser refuses them |
| Pretty-printed payloads | 9 | re-serialising destroys the signature |
| OBIS codes written as `[OCMF Tab. 25]` specifies | **0** | a canonical-form check refuses every record |

`ocmf` reports findings but deliberately does not decide money. This crate adds
the key registry, the law, and the mapping from each finding to what it costs:

| Question | Answered by | Crate |
|---|---|---|
| Did *this key* produce *these bytes*? | `ocmf::verify()` | `ocmf` |
| Are any records missing from the session? | `ocmf::session` | `ocmf` |
| Is this key *this charge point's* key? | `KeyRegistry` | this |
| Which quantity does each failure take away? | `chain::validate()` | this |
| May the *energy* be billed? | `Evidence::billable_energy` | this |
| May the *duration* be billed too? | `Evidence::billable_duration` | this |
| Can the customer repeat all of that? | `transparency` | both |

## Four quantities, not one boolean

OCMF flags energy and time trustworthiness separately `[OCMF Tab. 7]`, states
clock quality separately `[OCMF Tab. 19]`, and identification separately
`[OCMF Tab. 11]`. `Evidence` keeps them apart:

```rust
evidence.billable_energy();          // Some(0.268 kWh) — the register held up
evidence.billable_duration();        // None — the clock was only informative
evidence.identification_strength();  // the weakest level any record asserted
evidence.direction();                // Import, from the OBIS code the meter signed

evidence.reasons();                             // everything wrong with these records
evidence.reasons_for(Disqualifies::Duration);   // what took away the minutes
evidence.reasons_for(Disqualifies::Energy);     // empty — the register held up
```

`ChainFinding::disqualifies()` maps every finding to `Energy`, `Duration` or
`Both`. It is total over `ocmf::session::Finding` with a fallback of `Both`, so a
finding added in a later `ocmf` release cannot widen what this build bills. A
signature failure appears in every answer, because it stops chain validation.

### Billing rules beyond the format

One row per `ChainFinding` variant other than `ChainFinding::Sequence`:

| Rule | Source | Disqualifies |
|---|---|---|
| The billed register is not one `[OCMF Tab. 25]` reserved and never defined | `[OCMF Tab. 25]` | energy |
| The reading is in an energy unit at all — `mOhm` is a lawful `RU` | `[OCMF Tab. 7, RU]` | energy |
| …and both ends of the subtraction are counted in the **same** one | `[OCMF Tab. 7, RU]` | energy |
| Only one **direction** moved, so which register the session is has an answer | `[OCMF Tab. 25]` | energy |
| Cable-loss compensation is reported against an accumulation register | `[OCMF Tab. 7, CL]` | both |
| …and is reset at `TX=B`, so the session's own loss is `CL_end` | `[OCMF Tab. 7, CL]` | both |
| The identification level does not change mid-session | `[OCMF Tab. 11]` | both |
| An `EF` character this build does not know | `[OCMF Tab. 7, EF]` | both |
| There is a usable **pair** of readings on one register to subtract at all | `[OCMF Tab. 7]` | energy |

- **Same unit at both ends.** `RU` carries forward only within a record, so a
  chain can open at `2935.600 kWh` and close at `2965100 Wh`. Subtracting the
  numbers as written would bill 2 962 164.4 kWh with every signature valid.
- **One direction.** A bidirectional meter reports import and export registers.
  If both moved, the session is refused rather than silently dropping one
  direction; a register that stood still is not an error.
- **Cable loss per register.** `CL` is read on the billed register only, so two
  registers' loss series are never crossed, and each finding is raised once per
  register, not once per reading.

## Key registry with half-open validity windows

A station whose key is replaced has two keys over its life, and a record from
before the swap must still verify years later. Windows are `[from, until)`, so
consecutive keys partition the timeline exactly:

```rust
use emob_eichrecht::RegisteredKey;

let key = RegisteredKey {
    valid_from: Some(from),
    valid_until: Some(until),
    ..RegisteredKey::unbounded(public_key, "type approval DE-M-17-XYZ")
};
registry.insert(component, key)?; // refuses overlapping and empty windows
```

Lookup distinguishes "nothing registered" (a provisioning gap) from "a key whose
window has closed" (a replacement nobody registered), because an operator acts
differently on each.

## The transparency file for the customer

`[MessEG §33]` requires that the customer can check the value, so the deliverable
is the container the S.A.F.E. Transparenzsoftware reads. `ocmf::xml` writes it;
this crate decides which records go in — only those verified against a
registered key:

```rust
let xml = transparency::to_xml(&evidence)?;   // only what held up
let back = transparency::from_xml(&xml)?;     // reads back, for a dispute
```

`ocmf::xml` groups records by `transactionId` the way the verifier expects (one
`Transaction.Begin` and one `Transaction.End` per group), matching the 257
S.A.F.E. reference values.

## Typed findings: why a session did not bill

`Evidence::reasons` gives one English sentence per problem, for a log.
`Evidence::findings` gives the same problems typed, so a page can word them in
two languages, say what would have made the session billable, and cite the
rule — without reading anything back out of a sentence.

```rust
for finding in evidence.findings() {
    println!("{:?} {} — {}", finding.kind, finding.kind.citation(), finding.detail);
    // Signature [MessEG §33] — record 2: signature does not verify against this key
}
```

`Finding { kind, takes, detail }`: `takes` is what it disqualifies (an
unsynchronised clock takes only the duration). `FindingKind` has twelve kinds —
`NoKey`, `Signature`, `Unreadable`, `MeterState`, `Clock`, `Register`, `Unit`,
`Directions`, `CableLoss`, `Identification`, `Sequence`, `NoEnergy` — each with
its `citation()`. The mapping is total over `EvidenceProblem`. The operator page
Heute shows these under "Warum?".

## Session shape from signed markers

`[OCMF Tab. 7, TX]` defines markers that state facts money turns on, signed by
the meter rather than asserted by the CSMS:

```rust
evidence.suspended_intervals();      // `S`: connected but not charging
evidence.tariff_change_instants();   // `T`: on a grid boundary, `[PTB-A 50.7 §3.1.7.2]`
evidence.compensated_loss();         // cable loss inside the register, `[OCMF Tab. 7, CL]`
```

`emob-ocpp` compares the suspensions with OCPP's `chargingState`; `emob-cdr`
refuses a tariff change inside a session priced with one version
`[AFIR Art. 5(4)]` and carries the cable loss onto the record
`[REA 6-A §3.2]`. The `S` marker is optional, so it is evidence for an idle fee
where present, never a precondition.

Duties about the *meter* rather than its records — verification periods,
firmware records, the operator's confirmation `[MessEG §31(2)]`,
`[MessEG §33(2)]` — live in `emob_core::obligation`.

## Signature algorithms

Four of the seven algorithms in `[OCMF Tab. 22]` verify in pure Rust, including
`ECDSA-secp192r1-SHA256`, which the common eBZ LD3 meter uses. The brainpool
pair and secp192k1 have no audited pure-Rust implementation; `ocmf` reaches them
through OpenSSL, which this I/O-free crate does not link, so they are refused by
name.

## API at a glance

| Item | Purpose |
|---|---|
| `Evidence::assemble` | Run the whole check over a session's records |
| `Evidence::billable_energy`, `billable_duration` | The billable quantities, or `None` |
| `Evidence::reasons`, `reasons_for` | Why, overall or per quantity |
| `KeyRegistry`, `RegisteredKey`, `ComponentRef` | Which key belongs to which meter, when |
| `chain::validate`, `ChainFinding`, `Disqualifies` | Billing rules over a record chain |
| `transparency::to_xml`, `from_xml` | The customer-verifiable container |
| `Record`, `PublicKey`, `ObisCode` | Re-exported from `ocmf` |

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `serde` | yes | `Serialize`/`Deserialize` for the parsed payload types |

## Standards and sources

MessEG §31, §33; PTB-A 50.7; REA 6-A; OCMF (tables 3, 7, 11, 19, 22, 25); the
S.A.F.E. Transparenzsoftware and its reference data; AFIR Art. 5(4).

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. See
[the Eichrecht chain](https://hupe1980.github.io/emob/docs/eichrecht/) on the
documentation site and the [root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
