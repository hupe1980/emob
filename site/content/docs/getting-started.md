+++
title = "Getting started"
weight = 1
description = "Install the four crates that exist, verify a charging session under German calibration law, split it across quarter hours, build a CDR, and ask the obligation calendar whether a charge point is ready for 2027."
+++

# Getting started

Four crates are built from this workspace today. Everything on this page runs. ✅

```console
cargo add emob-core emob-eichrecht emob-session emob-cdr
```

## Verify a charging session

A station emits OCMF records — signed meter values — at the start and end of a
session, and often at quarter-hour boundaries in between. Turning them into a
number you may invoice takes four checks, and `Evidence::assemble` runs all of
them in the order that makes each one meaningful.

```rust
use emob_eichrecht::{ComponentRef, Evidence, KeyRegistry, PublicKey, RegisteredKey, ocmf};
use emob_eichrecht::ocmf::KeyType;

// The key binding arrives out of band — from the station's type approval or
// your provisioning system. Never from the record you are about to check.
let mut registry = KeyRegistry::new();
registry.insert(
    ComponentRef::Meter { serial: "BQ27400330016".into() },
    RegisteredKey::unbounded(
        PublicKey::from_hex(KeyType::Secp256r1, station_public_key)?,
        "type approval 2026-01",
    ),
);

let records = raw_records
    .iter()
    .map(|r| ocmf::parse(r))
    .collect::<Result<Vec<_>, _>>()?;

let evidence = Evidence::assemble(&records, &registry, session_start);

match evidence.billable_energy() {
    Some(energy) => println!("bill {energy}"),
    None => for reason in evidence.reasons() {
        eprintln!("blocked: {reason}");
    },
}
```

`billable_energy()` returns `None` whenever *anything* is wrong, and
`reasons()` says what. There is no other way to reach the number, which is the
point: the rule "a value that does not verify does not bill" is enforced by the
type rather than remembered by a developer.

### What blocks a session

Each of these is a real message the workspace can produce, and each corresponds
to a check some production platform has been found not to make:

```text
record 2: the signature does not match the payload
pagination jumped from 1 to 3: a record is missing or duplicated
the chain does not open with TX=B
the meter was in state Substitute at record 2, which may not be billed
record 2 flags its energy value as unusable (EF contains 'E')
the register ran backwards: 2965.100 then 2935.600
the opening and closing readings are on different registers
the signing component changed mid-session: BQ1 then BQ2
no public key is registered for signing component "meter:BQ1"
```

The pagination one deserves attention. Every signature in that session is
genuine — dropping records does not invalidate the ones that remain. Only the
chain check sees it.

## Split a session across quarter hours

```rust
use emob_session::split;

let split = split::into_quarter_hours(&series)?;

assert!(split.conserves());        // exactly, for every session
assert!(split.fully_measured());   // …and every boundary had a reading on it
```

The sum equals the session total to the last digit, whatever the ratios work out
to — see [Sessions and settlement](@/docs/settlement.md) for why that is by
construction rather than by reconciliation.

## Build a CDR

```rust
use emob_cdr::{CdrBuilder, CdrLedger, Acceptance};

let cdr = CdrBuilder::from_session(&session, Direction::Import)?
    .key(party, "cdr-1".parse()?)
    .evidence(evidence_ref)
    .build()?;

let mut ledger = CdrLedger::new();
assert_eq!(ledger.accept(cdr.clone()), Acceptance::Stored);
assert_eq!(ledger.accept(cdr), Acceptance::Duplicate);  // a retry, not a second invoice
```

## Ask whether a charge point is compliant

```rust
use emob_core::obligation::{assess, Verdict};
use emob_core::station::{AdHocPayment, ChargePointProfile, V2gCommunication};
use time::macros::date;

let mut point = ChargePointProfile::bare("DE*AB7*E840*6487".parse()?, date!(2026-06-01));
point.ad_hoc_payment = AdHocPayment::CardReader;
point.v2g = V2gCommunication::both_generations();

let report = assess(&point, date!(2027-01-01));

for finding in report.failing() {
    println!("{}  {}", finding.obligation.citation, finding.obligation.remedy);
}
assert_eq!(report.verdict(), Verdict::Failing);  // the data duties are still open
```

`ChargePointProfile::bare` starts every flag at its **non-compliant** value on
purpose. A fixture that starts compliant hides the obligation it was written to
exercise.

### Planning ahead

```rust
use emob_core::obligation::starting_between;
use time::macros::date;

// What lands on the fleet between now and the end of 2027?
for obligation in starting_between(date!(2026-09-01), date!(2027-12-31)) {
    println!("{}  {}  {}", obligation.applies_from, obligation.citation, obligation.title);
}
```

## Identifiers that survive a round trip

```rust
use emob_core::EvseId;

let separated: EvseId = "DE*AB7*E840*6487".parse()?;
let packed: EvseId = "DEAB7E8406487".parse()?;

assert_eq!(separated, packed);                          // one charge point
assert_eq!(separated.to_string(), "DE*AB7*E840*6487");  // written back verbatim
assert_eq!(separated.operator_id(), "AB7");
```

Hubject compares the identifier in a URL against your TLS client certificate as
*text*, and answers a mismatch with `017 Unauthorized Access`. So equality is
canonical and `Display` is verbatim, and the two never get confused.

## Building the workspace

```console
just            # list every recipe
just ci         # fmt, clippy, purity, tests, guards, deny, docs
just guards     # no-floats, check-citations, check-manifests
just purity     # no clock, no I/O, no unsafe in the domain crates
```

`just ci` is what CI runs, in CI order.
