# emob-eichrecht

German calibration law (*Eichrecht*) for EV charging, in Rust: OCMF signed meter
values parsed **without destroying the bytes the signature covers**, verified,
chain-validated, and billable only when every check holds.

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

## Four questions, kept apart

Conflating these is how a "verified" session turns out to be a signed fragment
of a session somebody edited:

| Question | Answered by |
|---|---|
| Did *this key* produce *these bytes*? | `ocmf::verify` |
| Is this key *this charge point's* key? | `registry::KeyRegistry` |
| Are any records missing from the session? | `chain::validate` |
| May these readings be billed at all? | `chain::validate`, via `MeterState` |

**Signatures alone cannot see a deletion.** Drop the middle records of a session
and every remaining signature still verifies. The specification assigns that
check to a "check component": pagination must be contiguous, the first record
must open the transaction and the last must close it. `chain::validate` is that
component, and it also enforces what a signature says nothing about — that the
meter was in state `G`, that no error flag disqualified the energy, that the
register ran forwards, that both readings are on the same OBIS register.

**A substitute value is not a measurement.** `ST=S` means the meter produced a
number because it could not measure one. Perfectly legitimate telemetry, and
never a basis for an invoice.

**The key binding travels out of band.** A verifier that takes the public key
from the record it is checking has verified nothing. `KeyRegistry` is populated
from type approval documents or provisioning — never from a record — and keys
carry validity windows, so a meter exchanged in June does not make January's
sessions unverifiable.

## Scope

OCMF 1.4 in full, plus the chain rules the specification assigns to a verifier.
Curves: secp256r1 (the OCMF default), secp384r1 and secp256k1. The two brainpool
curves in Table 22 are recognised and refused with a named error — no audited
pure-Rust implementation exists, and a wrong answer here is worse than none.

Note that OCMF pairs SHA-256 with *every* curve it names, including secp384r1.
That is not what the usual typed-digest APIs expect, so verification goes
through the prehash path, which applies the conversion the standard actually
specifies for a hash shorter than the field.

## No I/O, no clock

Nothing here opens a socket, reads a file or asks the time. The key registry is
handed in already populated and every instant is an argument, so a whole fleet's
verification runs as a deterministic unit test — and a dispute from two years
ago is replayed exactly as it happened.

## License

MIT OR Apache-2.0.
