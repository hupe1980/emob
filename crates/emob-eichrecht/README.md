# emob-eichrecht

German calibration law (*Eichrecht*) for EV charging, in Rust: OCMF signed meter
values parsed **without destroying the bytes the signature covers**, verified,
chain-validated, billable only when every check holds — and exported as the file
the customer checks the bill with.

```console
cargo add emob-eichrecht
```

## The rule this crate enforces

A customer may be billed for a measured value only if they can verify it, long
after the session — `[MessEG §33]`, `[PTB-A 50.7]`, `[REA 6-A]`. Every
closed platform treats this as a checkbox; every open-source CSMS ignores it.
Here it is the invariant everything hangs from:

> **A value that does not verify does not bill.**

And it is a property of the types rather than a convention: the only way to
obtain a billable quantity is `Evidence::billable_energy()`, which returns
`None` whenever anything at all is wrong.

```rust
use emob_eichrecht::{Evidence, KeyRegistry, ocmf};

let records = raw.iter().map(|r| ocmf::parse(r)).collect::<Result<Vec<_>, _>>()?;
let evidence = Evidence::assemble(&records, &registry, session_start);

match evidence.billable_energy() {
    Some(energy) => println!("bill {energy}"),          // 29.500 kWh
    None => for reason in evidence.reasons() {
        eprintln!("blocked: {reason}");                  // → an operator queue, not an invoice
    },
}
```

## Why parsing is the hard part

The signature covers the payload section **exactly as written** — "between
signing and validation, the payload section must not be manipulated (removing
and adding white spaces), otherwise positive validation is not possible."

A parser that deserialises the JSON and re-serialises it to verify has already
lost: key order, whitespace and number formatting are each free to change, and
every one of them changes the hash. So this parser keeps the payload's raw byte
span, and `signed_bytes()` returns that span rather than anything reconstructed.

The same reasoning reaches inside the JSON. `RV` is read as an exact decimal
from the token's own text, so `2935.600` keeps three decimal places: OCMF says
the representation "must not be transformed … since this would change the
representation of the physical quantity and thus potentially the number of valid
digits."

```rust
let record = ocmf::parse(raw)?;
assert_eq!(record.payload.readings[0].value.unwrap().to_string(), "2935.600");
// One added space anywhere in the payload and the signature no longer matches —
// which is correct, and which a re-serialising parser cannot tell you.
```

### An omitted field is unchanged, not absent

"For the readings, fields that have an identical value to the previous reading
are omitted. However, this only applies within a signed record"
`[OCMF Tab. 7 preamble]`. The rule is over **fields** — `RI` and `TX` are its
examples, not its list — so `RU`, `RT`, `ST` and `EF` carry forward on the same
footing.

```rust
// The first reading is flagged `E`, the second omits `EF`. Still flagged.
assert!(record.payload.readings[1].error_flags.energy_unusable);
```

`EF` is the one that decides money: reading the omission as "no fault" would
clear something the station signed. On the *first* reading there is nothing to
carry, so an absent `EF` is no flags and an absent `ST` is still an error.

## The questions, kept apart

Conflating these is how a "verified" session turns out to be a signed fragment
of a session somebody edited:

| Question | Answered by |
|---|---|
| Did *this key* produce *these bytes*? | `ocmf::verify` |
| Is this key *this charge point's* key? | `registry::KeyRegistry` |
| Are any records missing from the session? | `chain::validate` |
| May these readings be billed at all? | `chain::validate`, via `MeterState` |
| May the **duration** be billed too? | `chain::validate`, via `TimeStatus` |
| Which way did the energy go? | `chain::validate`, via `ObisCode` |
| Who was charging, and did the assignment hold? | `chain::validate`, via `IdentificationLevel` |
| Can the **customer** repeat all of that? | `transparency::to_xml` |

**Signatures alone cannot see a deletion.** Drop the middle records of a session
and every remaining signature still verifies. The specification assigns that
check to a "check component": pagination must be contiguous, the first record
must open the transaction and the last must close it. `chain::validate` is that
component, and it also enforces what a signature says nothing about — that the
meter was in state `G`, that no error flag disqualified the energy, that the
register ran forwards, that both readings are on the same OBIS register, and
that every record naming a signing component names the **same** one
`[OCMF §Relation of Serial Numbers]`.

That last one takes its reference from the first record that *names* a component,
not from the first record: OCMF permits a station to omit `MS`, and an anchor the
assembler chooses is an anchor they can use to switch the check off (D112).

**A substitute value is not a measurement.** `ST=S` means the meter produced a
number because it could not measure one. Perfectly legitimate telemetry, and
never a basis for an invoice.

**The user assignment can *fail*.** `MISMATCH`, `INVALID`, `OUTDATED`,
`UNKNOWN` `[OCMF Tab. 11]` are not weak assignments — the UIDs did not match,
the certificate did not check out, the trust anchor had expired. The energy was
measured and there is nobody provably behind it, so they block. They are
deliberately **not** on the ordered strength scale: putting them at the bottom
would make "the certificate was rejected" compare as slightly worse than an RFID
UID, and some `>=` would then bill it.

**The key binding travels out of band.** A verifier that takes the public key
from the record it is checking has verified nothing. `KeyRegistry` is populated
from type approval documents or provisioning — never from a record — and keys
carry validity windows, so a meter exchanged in June does not make January's
sessions unverifiable.

Those windows are **half-open**, `[from, until)`. With two inclusive bounds a
meter exchanged at midnight has two keys covering that instant, and the registry
answers with whichever was inserted first — so the same session verifies or does
not depending on insertion order. Half-open windows partition the timeline
exactly, which is what a key history is. A *gap* between two windows stays a gap:
a record from a month with no registered key fails to resolve rather than
falling through to a neighbouring one.

The partition is **enforced**, not assumed: `insert` is fallible and refuses a
window that overlaps one the component already holds — two unbounded keys for one
meter being the ordinary form of the mistake.

## Four quantities, not one

OCMF distinguishes them and so does this. `EF` flags energy (`E`) and time (`t`)
separately, `TM` states how far the clock can be trusted `[OCMF Tab. 19]`, `IL`
states how the user was identified, and the OBIS code states which way the energy
went. Collapsing them into one boolean throws away exactly the distinctions the
format was shaped to carry:

```rust
let report = chain::validate(&records);

// Same session, clock status `U` — unsynchronised.
assert_eq!(report.billable_energy.unwrap().to_string(), "29.500 kWh");
assert!(!report.is_billable_for_time());
```

A session on an unsynchronised clock has perfectly good energy and a duration
nobody can defend — so a per-kWh tariff bills it and a per-minute occupancy fee
`[AFIR Art. 5(4)]` must not. A faulty register is the mirror image: no energy,
and the car was still plugged in for twenty minutes.

`ChainFinding::disqualifies()` says which quantity each finding takes away, and
a test asserts that every finding takes away at least one — a finding that
disqualified nothing would be a finding that changes no answer.

Three more the chain catches and signatures cannot: a chain with **two** `TX=B`
markers is two charging processes concatenated, and the subtraction spans both;
an identification level that changes mid-session is two records disagreeing
about who was charging; and a `TX=X` marker means "time and/or energy are no
longer usable **from this reading (incl.)**" `[OCMF Tab. 7]` — the transaction
carries on, the numbers may not be billed across it.

## The OBIS code is read, not carried

`[OCMF Tab. 25]` reserves a range of OBIS codes that say exactly what a
charging session's registers mean:

| C field | Meaning |
|---|---|
| `B0` / `B1` | Total import — at the meter / at the vehicle |
| `B2` / `B3` | **Transaction** import — at the meter / at the vehicle |
| `C0` / `C1` | Total export |
| `C2` / `C3` | **Transaction** export |

So the signed register itself states the direction:

```rust
let code = ObisCode::new("01-00:C2.08.00*FF");
assert_eq!(code.direction(), Some(Direction::Export));
assert_eq!(code.scope(), Some(RegisterScope::Transaction));
assert!(code.is_accumulation_register());
```

A crate whose central claim is that import and export **never net** cannot
afford to hold that as an opaque string and take the direction from somewhere
else — a session recorded as a draw whose register says `C2` is a V2G discharge
billed as consumption. `emob-cdr` refuses exactly that.

Ordinary IEC 62056 codes still state a direction (`1.8.0` drawn, `2.8.0` fed
back) and nothing about the scope. **Anything else states no direction at all** —
not import by default, because a caller that needs one should have to get it from
elsewhere and know that it did.

The table also reserves `B4`–`BF` and `C4`–`C7` **for future use**, and a code
from there is a third case again — it blocks the energy:

```rust
assert!(ObisCode::new("01-00:B4.08.00*FF").is_reserved_for_future_use());
```

An unrecognised manufacturer register is still evidence and still bills. A
reserved code is a billing-relevant quantity the specification has claimed and
not published — the same argument the unknown error flag makes about a
character.

Reading the `D` field is also what makes the cable-loss rules checkable: `CL`
may be reported "only when RI is indicating an accumulation register reading",
and it "must be reset at TX=B" `[OCMF Tab. 7]`. A transaction opening on a
non-zero cumulated loss is carrying compensation from the previous session into
this one. The loss itself is never subtracted — the compensation is already
inside `RV` — but it is carried onto the report, because a partner disputing the
energy will ask how much of it was cable.

And it comes back as an `Energy`, not a bare decimal: `CL` "is given in the same
unit as RV which is specified in RU" `[OCMF Tab. 7, CL]`, and `RU` is `Wh` on
ordinary German hardware. Reported raw beside a `billable_energy` in kWh, a
420 Wh cable loss reads as 420 kWh — a figure a thousand times larger than the
session it exists to explain, in the middle of a dispute about that session.

## The transparency file

`[MessEG §33]` does not require a measured value to be *correct*. It requires
the affected party to be able to **check** it — so a platform that verifies
internally and reports "verified" has satisfied nobody.

```rust
use emob_eichrecht::transparency;

let xml = transparency::to_xml(&evidence);   // hand this to the driver
```

That is the XML container the S.A.F.E. Transparenzsoftware reads: one `<value>`
per record, each carrying the record **verbatim** — a reassembled one hashes
differently — beside the public key it was checked against.

Every `<value>` of one session carries **one** `transactionId`, because that is
what the verifier groups by rather than what it numbers records with: its
`MainView` collects `getValues(currentTransactionId)` and hands the whole list
to `verifyTransaction`, which is where the begin/end pairing and the energy
difference happen. Writing each record's pagination counter there instead is
schema-valid and degrades one session into N single-record transactions the
driver cannot pair. `to_xml_with_transaction_id` takes the operator's own
number, which is what makes the driver's file line up with the driver's invoice.

The `context` label is a statement about the whole data set, so a record
carrying `TX=B` **and** `TX=E` — the `MR` configuration, and the shape of the
eBZ LD3 reference record — is neither half of a transaction and is labelled
neither.

It takes an `Evidence` rather than a pile of records on purpose. A file that
took keys as arguments could be exported with the key that makes it verify
rather than the key the station was registered with, and the whole binding
travels out of band. A record that did not verify has no key binding and
therefore no `<value>`; `reasons()` is what explains that to the operator.

A session that does **not** bill still gets a file. The customer's right to
check does not depend on the answer, and a dispute is precisely when it matters.

`transparency::from_xml` reads one back, because the export is only half of the
duty: the other half arrives when a driver disputes a bill and sends the file
back, and an operator has to check its records against **its own** registry.
`TransparencyValue::claimed_key` is named for what it is — the file's claim, not
a binding — because verifying a record against a key the same file supplied
proves only that whoever wrote the file owned a private key. The comparison
against the registered key is the check worth making.

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

## The two ways a duration stops being billable

`[OCMF Tab. 19]` says how far a station's clock can be trusted, and this crate
turns that into `Evidence::billable_duration()`. `[REA 6-A §3.1]` adds the other
half — "Messwerte unterhalb der kürzest möglichen Zeitspanne werden nicht für
Abrechnungszwecke verwendet" — and `emob-cdr` enforces it where the tariff is
known.

The two are the same rule from opposite ends: there the clock cannot be
**placed**, here the span cannot be **resolved**. Both leave a session whose
register an invoice may use and whose duration it may not, which is why the two
quantities were separated in the first place.

## Tested against a meter it did not write

Every other fixture here is signed at test time with a key the test also holds,
which proves the crate agrees with itself. The suite therefore also runs an
**eBZ LD3** record — ordinary German charging hardware — from the reference data
set the S.A.F.E. Transparenzsoftware ships, against the key it is published
with:

```rust
assert_eq!(evidence.billable_energy().unwrap().to_string(), "0.268 kWh");
assert!(!evidence.is_billable_for_time());   // its clock is only informative
```

It found three things a self-signed fixture cannot reach: an unimplemented
curve, a non-canonical DER signature, and a framing assumption about pipes.

## Scope

OCMF 1.4 in full, plus the chain rules the specification assigns to a verifier.

**High `s` is not a forgery.** For every ECDSA signature `(r, s)` the pair
`(r, n − s)` verifies the same message, and plain ECDSA — all `[OCMF Tab. 22]`
names — accepts both. Bitcoin requires the low half, and `k256` enforces it
inside `verify`; a DZG DVH4013 in the reference corpus signs with the high half,
`openssl` accepts it, and this build answered `SignatureMismatch` — the
diagnostic for tampering. Signatures are normalised before verification on every
curve, which accepts exactly what plain ECDSA accepts and nothing more.

**Curves.** `[OCMF Tab. 22]` names seven algorithms and four are verified here:
secp256r1 (the OCMF default), secp384r1, secp256k1 and **secp192r1** — which is
not a legacy curiosity, since it is what the eBZ reference record signs with.
The other three are recognised and refused **by name**, each for its own reason:
the brainpool pair because RustCrypto gates `bp256`/`bp384`'s arithmetic behind
`wip-arithmetic-do-not-use`, secp192k1 because no pure-Rust implementation is
published at all. A wrong answer is worse than none, so none of them is
approximated with a neighbouring curve.

**Signature encoding.** `SM` calls for DER, and real meters do not emit
canonical DER: the eBZ firmware pads both integers to a fixed 24 bytes
regardless of sign, so its `r` reads as negative to a strict parser and its `s`
carries a `0x00` it does not need. Each INTEGER's content is read as an unsigned
magnitude and re-emitted minimally before verification — lenient about
**encoding**, strict about **structure**: anything that is not a SEQUENCE of
exactly two INTEGERs is refused.

**Hashing.** OCMF pairs SHA-256 with *every* curve it names, and only two of
them have a 32-byte field. The usual typed-digest APIs expect the hash to be
exactly the field width, so verification goes through the prehash path, which
applies the X9.62 conversion in both directions — left-padding a short hash,
truncating a long one.

## No I/O, no clock

Nothing here opens a socket, reads a file or asks the time. The key registry is
handed in already populated and every instant is an argument, so a whole fleet's
verification runs as a deterministic unit test — and a dispute from two years
ago is replayed exactly as it happened.

## License

MIT OR Apache-2.0.
