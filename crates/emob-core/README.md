# emob-core

The domain model every other [`emob`](https://github.com/hupe1980/emob) crate is
written against: the identifiers the e-mobility market runs on, the quantities
that become money, and the regulatory calendar a charge point is judged against.

```console
cargo add emob-core
```

## Identifiers: two grammars, one identity, and the text that arrived

`DE*AB7*E840*6487` and `DEAB7E8406487` are the same charge point. They compare
equal, hash the same — and each writes itself back **exactly as it arrived**:

```rust
use emob_core::EvseId;

let separated: EvseId = "DE*AB7*E840*6487".parse()?;
let packed: EvseId = "DEAB7E8406487".parse()?;

assert_eq!(separated, packed);                          // one charge point…
assert_eq!(separated.to_string(), "DE*AB7*E840*6487");  // …each written back verbatim
assert_eq!(packed.to_string(), "DEAB7E8406487");
assert_eq!(separated.operator_id(), "AB7");             // a hub's routing rule, for free
# Ok::<(), emob_core::IdError>(())
```

Normalising on ingest is the single most common way a roaming integration fails
in production: Hubject compares the identifier in a URL against the TLS client
certificate **as text**, and answers a mismatch with `017 Unauthorized Access`.
So this crate refuses to do it. `canonical()` is there for anyone who wants the
normalised form deliberately.

The same holds for [`Emaid`], which accepts both the ISO 15118 and DIN SPEC
91286 spellings of a contract id and remembers which it was given.

## Quantities: exact, and direction is a field

Every number here either *is* money or *becomes* money, so none of them is a
binary float — `cargo xtask no-floats` fails the build on an `f32` or `f64`
anywhere in the workspace.

```rust
use emob_core::Energy;
use rust_decimal::Decimal;
use std::str::FromStr;

// The subtraction OCMF prescribes for a session's energy, done exactly.
let start = Energy::from_kwh(Decimal::from_str("2935.600")?)?;
let end   = Energy::from_kwh(Decimal::from_str("2965.100")?)?;

assert_eq!((end - start)?.to_string(), "29.500 kWh");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Note the trailing zeros. A register reporting `2935.600 kWh` is stating three
decimals of resolution, and rewriting it as `2935.6` discards a claim the meter
made about its own accuracy — which OCMF forbids in as many words. `Energy` and
`Money` never strip scale, and `Energy::from_wh` converts by moving the decimal
point rather than dividing, so `29500 Wh` becomes `29.500 kWh`.

`Energy` is a non-negative magnitude and `Direction` says which way it flowed.
Netting the two inside a billing period would let a V2G discharge cancel a draw,
and both would leave their supplier's balance group unaccounted for.

## The obligation calendar

"Are we AFIR-ready for 2027?" is normally a consulting engagement. Here it is a
query:

```rust
use emob_core::obligation::assess;
use emob_core::station::ChargePointProfile;
use time::macros::date;

let point = ChargePointProfile::bare("DE*AB7*E840*6487".parse()?, date!(2026-06-01));
let report = assess(&point, date!(2027-01-01));

for finding in report.failing() {
    println!("{}  {}", finding.obligation.citation, finding.obligation.remedy);
    // [AFIR Art. 5(1)]  offer a contract-free payment path (card reader, or a web/QR flow)
    // [DA-656]          TLS 1.3 and the larger certificates of -20 usually mean a hardware refresh
    // …
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

AFIR, Delegated Regulation (EU) 2025/656, LSV 2026, MessEG/PTB-A and the
THG-Quote preconditions, as dated, cited, executable rules. Three properties
make it trustworthy:

**Every duty carries its citation.** `cargo xtask check-citations` fails the
build if a citation names a document `specs/README.md` does not index, so a rule
can always be followed to a file, a section and a retrieval URL.

**Every duty carries its window.** Asking the calendar about a date is the only
way to use it, so a duty cannot be applied a year before it exists.

**Applicability and satisfaction are separate questions.** A private depot is
not *failing* the ad-hoc payment duty — the duty does not bind it. Merging the
two produces reports that cry wolf, which is how compliance dashboards come to
be ignored. `starting_between` answers the planning question: what lands on the
fleet between now and 2028.

## No I/O, no clock

Nothing in this crate reads a clock, a socket or a file. Every function that
needs "now" takes it as an argument, so a compliance question about a date two
years out is the same call as one about today.

## License

MIT OR Apache-2.0.
