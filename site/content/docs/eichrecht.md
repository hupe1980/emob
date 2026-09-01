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

Five gates stand between a station's records and a quantity an invoice may use.
Each is a separate question with a separate answer, and each can refuse on its
own:

```mermaid
flowchart LR
    RAW["OCMF records<br/>from the station"] --> P["parse<br/>signed bytes kept verbatim"]
    P --> V["verify<br/>ECDSA, four curves"]
    KR[("key registry<br/>out of band")] -.->|the key, never<br/>the record's own| V
    V --> C["chain<br/>pagination, markers, states"]
    C --> E["evidence"]
    E --> Q{"billable?"}
    Q -->|yes| BILL["energy · duration<br/>each answered separately"]
    Q -->|no| WHY["reasons<br/>naming what failed"]
    E --> TX["transparency file<br/>the driver's own verifier"]

    classDef gate fill:#b8410f22,stroke:#b8410f
    classDef out fill:#0a7d3322,stroke:#0a7d33
    class P,V,C gate
    class BILL,TX out
```

The transparency file hangs off the evidence rather than off a successful
outcome, because a dispute is exactly the case where the session did **not**
bill and the driver still has to be able to check it.

What happens to a verified quantity next is
[sessions and settlement](@/docs/settlement.md); what turns it into money is
[tariffs](@/docs/pricing.md).

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

### An omitted field is unchanged, not absent

"For the readings, fields that have an identical value to the previous reading
are omitted. However, this only applies within a signed record"
`[OCMF Tab. 7 preamble]`. The rule is over **fields**; `RI` and `TX` are its
examples, not its list. `RU`, `RT`, `ST` and `EF` carry forward on the same
footing.

```rust
// The first reading is flagged `E`, the second omits `EF`. Still flagged.
assert!(record.payload.readings[1].error_flags.energy_unusable);
```

`EF` is the one that decides money: reading the omission as "no fault" would
clear something the station signed. On the first reading there is nothing to
carry, so an absent `EF` is genuinely no flags.

## The questions, kept apart

| Question | Answered by | Skipping it means |
|---|---|---|
| Did *this key* produce *these bytes*? | `ocmf::verify` | anyone can bill you |
| Is this key *this charge point's* key? | `KeyRegistry` | the record vouches for itself |
| Are any records missing? | `chain::validate` | a session with a hole in it bills |
| May these readings be billed? | `MeterState`, error flags | a substitute value becomes an invoice |
| May the **duration** be billed too? | `TimeStatus`, the `t` flag | an occupancy fee is billed off an unsynchronised clock |
| Which way did the energy go? | the OBIS code | a V2G discharge is billed as consumption |
| Who was charging, and did it hold? | `IdentificationLevel` | a rejected certificate becomes a customer |
| Can the **customer** repeat all of it? | `transparency::to_xml` | the law's actual requirement is unmet |

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

The chain also holds every record to one signing component
`[OCMF §Relation of Serial Numbers]`, taking its reference from the first record
that *names* one rather than from the first record. OCMF lets a station omit
`MS`, and a check whose anchor the assembler chooses is a check the assembler can
switch off.

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

The windows are **half-open**, `[from, until)`. Two inclusive bounds leave the
swap instant covered by both keys, and the registry then answers with whichever
was inserted first — so the same session verifies or does not depending on
insertion order, which is not a property anybody wants to defend in a dispute.
Half-open windows partition the timeline exactly, which is what a key history is.

The partition is enforced rather than assumed: `insert` refuses a window that
overlaps one the component already holds — two unbounded keys for one meter being
the ordinary form of the mistake.

A *gap* between two windows stays a gap. A record from a month with no
registered key fails to resolve rather than falling through to a neighbouring
key, because inventing a binding is how a forged record verifies.

### A substitute value is not a measurement

`ST=S` means the meter produced a number because it could not measure one.
Legitimate telemetry; never a basis for an invoice. Only `ST=G` is billable, and
an `ST` code this build has never heard of is an error rather than an optimistic
pass — a signature component reporting an unknown state is exactly when billing
should stop.

The same holds for the `EF` error flags. An unrecognised flag character blocks
billing rather than being skipped, so a future OCMF revision cannot widen what
gets invoiced by adding a character an old implementation ignores.

## Four quantities, not one

OCMF distinguishes them and so does this. `EF` flags energy (`E`) and time (`t`)
separately `[OCMF Tab. 7]`, `TM` states how far the clock can be trusted
`[OCMF Tab. 19]`, `IL` states how the user was identified `[OCMF Tab. 11]`, and
the OBIS code states which way the energy went `[OCMF Tab. 25]`. Collapsing them
into one boolean throws away exactly the distinctions the format was shaped to
carry.

```rust
// The same session, clock status `U` — unsynchronised.
assert_eq!(report.billable_energy.unwrap().to_string(), "29.500 kWh");
assert!(!report.is_billable_for_time());
```

A session on an unsynchronised clock has perfectly good energy and a duration
nobody can defend — so a per-kWh tariff bills it and the per-minute occupancy fee
`[AFIR Art. 5(4)]` permits must not. A faulty register is the mirror image: no
energy, and the car was still plugged in for twenty minutes.

And a register from the range `[OCMF Tab. 25]` **reserves and does not define** —
`B4`–`BF`, `C4`–`C7` — is a third case, taking the energy and leaving the clock.
An unrecognised manufacturer code is still evidence and still bills; a reserved
one is a billing-relevant quantity the specification has claimed and not
published.

`ChainFinding::disqualifies()` says which quantity each finding takes away, and
a test asserts that every finding takes away at least one — a finding that
disqualified nothing would be a finding that changes no answer. The gate is
enforced one layer up too: `emob-cdr` refuses to price a per-minute tariff
against evidence that cannot carry a duration, and names the fix.

### A user assignment can fail

`MISMATCH`, `INVALID`, `OUTDATED`, `UNKNOWN` are not weak assignments. The UIDs
did not match; the certificate did not check out; the trust anchor had expired.
The energy was measured and there is nobody provably behind it, so they block —
and they are deliberately kept **off** the ordered strength scale. Putting them
at the bottom of it would make "the certificate was rejected" compare as slightly
worse than an RFID UID, and some `>=` would then bill it.

The strength that *is* on the scale — `HEARSAY` to `SECURE` — is read off the
records and taken as the **weakest** any of them asserted, then handed to the
CDR cross-check. Taking it from a field a caller filled in would make that check
a formality.

### The register states the direction

`[OCMF Tab. 25]` reserves a range of OBIS codes: `B0`–`B3` is import, `C0`–`C3`
export, with the scope (one transaction or the meter's lifetime) and the
measurement point (at the meter or at the vehicle) in the same nibble.

```rust
assert_eq!(ObisCode::new("01-00:C2.08.00*FF").direction(), Some(Direction::Export));
```

A workspace whose founding claim is that import and export **never net** cannot
hold that as an opaque string and take the direction from the session model
without ever comparing the two — a record claiming a draw over a `C2` register is
a V2G discharge billed as consumption, and nothing downstream of the register
could see it. `emob-cdr` refuses it, and so does the pre-flight on a partner's
record.

A code the crate cannot classify states *no* direction rather than defaulting to
import.

Reading the code is also what makes the cable-loss rules checkable. `CL` may be
reported "only when RI is indicating an accumulation register reading" and "must
be reset at TX=B" `[OCMF Tab. 7]` — a transaction opening on a non-zero
cumulated loss is carrying compensation from the previous session into this one.
The loss is never subtracted (the compensation is already inside `RV`) but it is
carried onto the report, because a partner disputing the energy will ask how much
of it was cable.

And it comes back as an `Energy`, not a bare decimal: `CL` "is given in the same
unit as RV which is specified in RU" `[OCMF Tab. 7, CL]`, and `RU` is `Wh` on
ordinary German hardware. Reported raw beside a `billable_energy` in kWh, a
420 Wh cable loss reads as 420 kWh — a figure a thousand times larger than the
session it exists to explain, in the middle of a dispute about that session.

### An exception marker stops the arithmetic

`TX=X` is "error during charging, transaction continues, time and/or energy are
no longer usable **from this reading (incl.)**". The transaction carries on; the
numbers may not be billed across it.

### Two transactions can hide in one chain

Two `TX=B` markers mean two charging processes were concatenated. Pagination
stays contiguous across the join, so nothing else sees it, and subtracting the
last register value from the first spans both sessions — a number larger than
either and belonging to neither.

## The records also state the shape of the session

Eight of `[OCMF Tab. 7, TX]`'s ten markers are structure. Two are facts nothing
else in the evidence carries, and they are the intervals money turns on: `S`
("Suspended = Transaction active, but currently not charging") and `T` (a tariff
change).

```rust
for (from, to) in evidence.suspended_intervals() { /* signed occupancy */ }
```

`[AFIR Art. 5(4)]` prices those minutes per minute, and the only other account of
them is OCPP's `chargingState` — a protocol field asserted by the same party that
issues the invoice. `emob-ocpp` compares the two.

A **note, never a refusal**: `S` is optional, so its absence says nothing and
most of the fleet never emits one. Its presence against a contrary protocol claim
is two stories about one event, and only one of them is signed.

## The transparency file

`[MessEG §33]` does not say a measured value must be **correct**. It says the
affected party must be able to **check** it. A platform that verifies internally
and reports "verified" has satisfied nobody: the customer cannot repeat the
check, and repeating it is the entire requirement.

What they repeat it with, in Germany, is the S.A.F.E. Transparenzsoftware — an
independent verifier the industry maintains. It reads an XML container:

```xml
<values>
  <value transactionId="120" context="Transaction.Begin">
    <publicKey encoding="hex">3059301306072A8648CE3D…</publicKey>
    <signedData format="OCMF" encoding="plain">OCMF|{…}|{"SD":"…"}</signedData>
  </value>
  <value transactionId="120" context="Transaction.End">…</value>
</values>
```

```rust
let xml = transparency::to_xml(&evidence);   // hand this to the driver
```

The key comes first, because the verifier's own `values.xsd` sequences it first.
The format's prose example shows the other order, and both unmarshal today only
because the reference implementation does not validate against its own schema.

Each record appears **verbatim** — a reassembled one hashes differently, which
is why the parser keeps the whole `OCMF|…|…` string and not only the span the
signature covers. Beside it is the key it was checked against: the one the
registry supplied out of band, never one chosen later to make the file verify.
That is why the export takes an `Evidence` rather than a pile of records — a
function taking keys as arguments could be handed the convenient key.

A record that did **not** verify has no key binding and therefore no `<value>`;
`reasons()` explains that to the operator. And a session that does not bill still
gets a file, because the customer's right to check does not depend on the answer
and a dispute is exactly when it matters.

### One id per transaction, not one per record

`transactionId` is what the verifier **groups** by, and that is the whole of its
job: `MainView` collects `getValues(currentTransactionId)` and hands the list to
`verifyTransaction`, which is where the begin/end pairing and the energy
difference happen. The reference data set carries one id across both halves of a
session.

Writing each record's pagination counter there instead is schema-valid, distinct,
and degrades one session into N single-record transactions the driver cannot
pair — the exact failure `[MessEG §33]` is about, produced by the code written to
prevent it. OCMF has no transaction number of its own, so `to_xml` derives one
from the counter the transaction opened at, and `to_xml_with_transaction_id`
takes the operator's own when there is one (D74).

The `context` label follows the same reading. It is a statement about the whole
data set, so a record carrying `TX=B` **and** `TX=E` — the `MR` configuration,
and the shape of the eBZ LD3 reference record — is neither half of a transaction
and is labelled neither. The attribute is optional and the reference samples omit
it for exactly that shape.

### Reading one back

The export is half of `[MessEG §33]`. The other half arrives when a driver
disputes a bill and sends the file back: an operator then has to parse it, check
the records against **its own registry**, and say whether the key inside it is
the key the station was provisioned with.

```rust
let values = transparency::from_xml(&their_file)?;
```

`TransparencyValue::claimed_key` is deliberately not called "the key". It is a
claim made by the artefact under examination, and verifying a record against a
key the same file supplied proves only that whoever wrote the file owned a
private key — the binding travels out of band `[OCMF §Relation of Serial
Numbers]`. What it is good for is the comparison: a file whose key differs from
the registered one is a dispute with an answer.

The reader is strict on purpose — no comments, no CDATA, no namespaces, no
entity it does not know. A transparency file is machine-generated, and a
verifier that silently accepts a shape it half-understands is the failure the
file exists to prevent, pointed inward.

## Tested against meters that exist

Every other fixture on this page is signed at test time with a key the test also
holds, which proves the code agrees with itself — not the question anybody is
asking. So the suite runs records from four sources `emob` did not write, each
checked against the key it is published with:

```rust
// an eBZ LD3 from the S.A.F.E. reference data set
assert_eq!(evidence.billable_energy().unwrap().to_string(), "0.268 kWh");
assert!(!evidence.is_billable_for_time());   // its clock is only informative
```

Three of the four cost a defect:

| Record | What it broke |
|---|---|
| eBZ LD3 (secp192r1) | a curve the build refused, and a DER wrapper it rejected — both below (D34–D35) |
| DZG DVH4013 / Nano (secp256k1) | a signature with `s` in the **high half** of the curve order, which `k256` refuses on Bitcoin's malleability rule and plain ECDSA allows (D82) |
| DZG + TwinCharger Pro | `RV` written as a **quoted string**, padded with spaces (D83) |
| TwinCharger Pro | `FV` and `CT` written as **numbers** where the tables say String (D83) |

The secp256k1 case is the sharpest. A self-signed fixture uses a signer that
naturally produces low `s`, so no amount of self-signed corpus contains the
high-`s` case — and the failure it produces is `SignatureMismatch`, the
diagnostic for tampering, on a meter that has done nothing wrong.

Three defects from four records is not a rate that suggests the list is
finished, which is why every format this workspace claims to read carries at
least one third-party sample in its tests.

## What the customer is paying for, when it is not only electricity

`[REA 6-A §3.2]` permits a DC station to meter on the **AC side, before the
rectifier**. The rectification losses are then inside the number the customer is
billed for — and the regulation allows it on three conditions, each of which a
real fleet can quietly fail:

1. the station was **placed on the market before 2018** and is rated **at most
   50 kW**. Note the direction: AFIR's fast-charger duties begin *at* 50 kW
   counting up, this allowance still applies *at* 50 kW counting down, and a
   50 kW station is on the strict side of one rule and the permissive side of
   the other;
2. the rectification "kann einem einzelnen Ladevorgang ausschließlich und
   eindeutig zugeordnet werden" — which a **multi-outlet cabinet sharing one
   rectifier does not**, and shared rectifiers are the normal way such cabinets
   are built;
3. the customer is told: "Die von einem Messwert oder einer Rechnung Betroffenen
   sind in geeigneter Weise darauf hinzuweisen, dass die … Energie für die
   Gleichrichtung … Bestandteil des angegebenen Messwerts ist."

The third is the one this workspace has no excuse for missing. A platform whose
central claim is that the customer can **check** the value owes them the fact
that part of it is loss — a number nobody can interpret is a number nobody can
check.

"Placed on the market" is also not commissioning: a station sold in 2017 may
have been commissioned in 2019, and the allowance turns on the first. Where it
is unknown the profile falls back to commissioning, which is the later of the
two — so the exemption fails rather than being granted on a date nobody stated.

## A span too short to resolve is not a span to bill

`[REA 6-A §3.1]` sets an error limit on a station's clock and then a floor
under it:

> Die kürzest mögliche Zeitspanne, für die diese Fehlergrenze erfüllt wird, darf
> nicht mehr als 60 Sekunden betragen … **Messwerte unterhalb der kürzest
> möglichen Zeitspanne werden nicht für Abrechnungszwecke verwendet.**

So a thirty-second session billed per minute is billing a number the clock
cannot defend. It is the mirror of the unsynchronised-clock rule above, arriving
from the other end: there the clock cannot be *placed*, here the span cannot be
*resolved*, and both leave a duration an invoice may not use while the register
is untouched.

The manufacturer states the real figure in the instructions and it may be far
below sixty seconds. A platform that does not have it has not been told the
device is better than the worst case the regulation permits, so
`ClockResolution` defaults to the cap and a station that knows better says so —
the same reasoning that makes an unevaluable tariff restriction never match.

## Curves, and the meter that named the gap

`[OCMF Tab. 22]` names seven algorithms. **Four** are verified here — secp256r1
(the default since OCMF 0.4), secp384r1, secp256k1 and secp192r1 — and the three
that are not each say *why*, because the reasons differ and an operator needs to
know which one they have hit: the two brainpool curves because RustCrypto's
`bp256`/`bp384` gate their arithmetic behind `wip-arithmetic-do-not-use`, and
secp192k1 because no pure-Rust implementation is published at all.

secp192r1 is on the list because it was missing. The **eBZ LD3** data set the
S.A.F.E. Transparenzsoftware ships as a reference sample — ordinary German
charging hardware — is signed `ECDSA-secp192r1-SHA256`, and this build
recognised the algorithm and refused it. Every session from such a fleet would
have been unverifiable, and therefore unbillable. A curve list assembled from
what looks modern rather than from what is deployed fails on the day it matters.

## Real signatures are not canonical DER

`SM` says the signature is a DER-encoded ASN.1 `SEQUENCE { INTEGER r, INTEGER
s }`, and DER requires each integer to be minimal and signed. Meters do not
oblige. The eBZ firmware pads both to a fixed 24 bytes regardless of sign, so
its `r` begins `e1` with no `0x00` in front — which a strict reader must treat
as a **negative number** — and its `s` carries a leading `0x00` it does not
need.

The signature is perfectly good; the wrapper is not. Rejecting it rejects a
record the reference verifier accepts, for a reason that has nothing to do with
whether the meter value was tampered with. So each integer's contents are read
as an unsigned magnitude and re-emitted minimally before verification: lenient
about **encoding**, strict about **structure**.

One subtlety worth knowing if you implement this yourself: OCMF pairs
**SHA-256 with every curve it names**, and only two of them have a 32-byte
field. The usual typed digest APIs require the hash to be exactly the field
width, so neither a 32-byte digest against secp384r1's 48-byte field nor the
same digest against secp192r1's 24-byte field even compiles. Verification goes
through the prehash path instead, which applies the X9.62 conversion in both
directions.

## What is not here yet 📐

The retention windows per artefact — MessEV and PTB give them and the reading is
not settled — and the seam into `emob-billing` where the verified energy becomes
an EN 16931 invoice line. The verification half, the money that comes off it and
the file the customer checks it with are all done.
