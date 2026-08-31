+++
title = "The Eichrecht chain"
weight = 2
description = "How a signed meter value becomes an invoice line: OCMF parsed without destroying its signed bytes, four questions kept apart, and the reason a valid signature is not enough."
+++

# The Eichrecht chain ✅

German calibration law makes a kilowatt-hour bill valid only if the customer can
verify the measured value against a conformity-assessed meter, arbitrarily long
after the session — `[MessEG §33]`, `[PTB-A 50.7]`, `[REA 6-A]`. This page is
how `emob-eichrecht` implements that, and why each piece is separate from the
others.

## The promise

> **A value that does not verify does not bill.**

Not a convention, a type. `Evidence::billable_energy()` is the only route to a
billable quantity, and it returns `None` whenever any check failed. A caller who
wants to bill anyway has to write code that visibly ignores the answer.

## Parsing is the hard part

OCMF frames a record in three pipe-separated sections:

```text
OCMF|{"FV":"1.4","PG":"T12345",…,"RD":[…]}|{"SD":"3045…"}
```

The signature covers the middle section **exactly as written**. The
specification is explicit:

> Between signing and validation, the payload section must not be manipulated
> (removing and adding white spaces), otherwise positive validation is not
> possible.

So a parser that deserialises the JSON and re-serialises it to hash has already
lost. Key order, insignificant whitespace and number formatting are all free to
change under a round trip, and each of them changes the digest. `emob-eichrecht`
keeps the payload's raw byte span beside the typed view:

```rust
let record = ocmf::parse(raw)?;
record.signed_bytes();   // the span as it arrived — never a re-serialisation
record.payload;          // the typed view, for everything else
```

A test holds the line:

```rust
// Adding one space is a legal JSON edit and an illegal OCMF one.
let reformatted = raw.replacen(r#"{"FV""#, r#"{ "FV""#, 1);
assert!(matches!(verify(&ocmf::parse(&reformatted)?, &key), Err(VerifyError::SignatureMismatch)));
```

### Numbers keep their scale

The same reasoning reaches inside the JSON. OCMF says a reading value's
representation "must not be transformed by further handling methods … since this
would change the representation of the physical quantity and thus potentially
the number of valid digits". `2935.600 kWh` is a meter stating three decimals of
resolution.

`serde_json` routes numbers through `f64` by default, which silently turns that
into `2935.6`. This workspace enables `arbitrary_precision` for exactly that
reason, and reads every value as an exact decimal from the token's own text.

```rust
assert_eq!(record.payload.readings[0].value.unwrap().to_string(), "2935.600");
```

## Four questions, kept apart

| Question | Answered by | Skipping it means |
|---|---|---|
| Did *this key* produce *these bytes*? | `ocmf::verify` | anyone can bill you |
| Is this key *this charge point's* key? | `KeyRegistry` | the record vouches for itself |
| Are any records missing? | `chain::validate` | a session with a hole in it bills |
| May these readings be billed? | `MeterState`, error flags | a substitute value becomes an invoice |

### Signatures cannot see a deletion

This is the one people miss. Each record is signed independently, so removing
the middle of a session leaves a sequence in which every remaining signature
still verifies. The specification assigns the check to a separate "check
component": pagination must be contiguous within one context, the first record
must open the transaction, the last must close it, and nothing may be added or
removed in between.

```rust
// Every signature genuine; the session still does not bill.
let report = chain::validate(&[record_pg1_begin, record_pg3_end]);
assert!(report.findings.contains(&ChainFinding::PaginationBreak { after: 1, found: 3 }));
assert!(!report.is_billable());
```

Pagination parsing refuses `T007` alongside `T7` for the same reason: two
spellings of one counter value would break the "increments by exactly one"
property the whole check rests on.

### The key binding travels out of band

A verifier that reads the public key from the record it is checking has verified
nothing. OCMF says so:

> The public keys to the charging point must be transmitted to the verification
> component by means other than this protocol (out-of-band) …

`KeyRegistry` is populated from type approval documents or a provisioning
system, and it models the identification rules the specification actually gives:
a meter serial when the meter signs, a gateway serial when a gateway signs for
one point, and **both** when one gateway serves several — because then neither
alone is unique.

Keys carry validity windows. Without them a registry holds only the current key,
and the day a meter is exchanged every historical session becomes unverifiable —
which is the same failure as an undefendable invoice, arriving late.

```rust
registry.key_at(&component, datetime!(2026-01-15 12:00 UTC));  // the original key
registry.key_at(&component, datetime!(2026-09-15 12:00 UTC));  // the one after the swap
```

### A substitute value is not a measurement

`ST=S` means the meter produced a number because it could not measure one.
Legitimate telemetry; never a basis for an invoice. Only `ST=G` is billable, and
an `ST` code this build has never heard of is an error rather than an optimistic
pass — a signature component reporting an unknown state is exactly when billing
should stop.

The same holds for the `EF` error flags. An unrecognised flag character blocks
billing rather than being skipped, so a future OCMF revision cannot widen what
gets invoiced by adding a character an old implementation ignores.

## Curves

`[OCMF Tab. 22]` names seven algorithms. secp256r1 (the default since OCMF 0.4),
secp384r1 and secp256k1 are verified; the two brainpool curves are recognised
and refused with a named error, because no audited pure-Rust implementation
exists and a wrong answer here is worse than no answer.

One subtlety worth knowing if you implement this yourself: OCMF pairs
**SHA-256 with every curve it names**, including secp384r1. The usual typed
digest APIs require the hash to be exactly the field width, so a 32-byte digest
against a 48-byte field does not even compile. Verification goes through the
prehash path instead, which applies the conversion the standard actually
specifies for a hash shorter than the field.

## What is not here yet 📐

The transparency-file emission that the S.A.F.E. Transparenzsoftware consumes,
the retention windows per artefact, and the seam into `emob-billing` where the
verified energy becomes an invoice line. The verification half — the part that
has to be right before any of that means anything — is done.
