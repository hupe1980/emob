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

The same holds for [`Emaid`], which accepts every spelling of a contract id and
remembers which it was given.

`-` is a separator in a contract id and **not** in an EVSE id, because the eMI3
grammar does not define one. Eating it would make `DE-AB7-E840` parse as a charge
point and compare equal to a real one.

## …and a contract id's check digit is checked

An `Emaid` accepts all three contract grammars — ISO 15118-1, EMI3 and
DIN SPEC 91286 — and tells them apart by shape, because their instance sections
are different lengths and **their check-digit algorithms are different
algorithms**.

```rust
use emob_core::ids::{ContractGrammar, Emaid};

let iso: Emaid = "NL-TNM-000122045-U".parse()?;
let packed: Emaid = "NLTNM000122045".parse()?;   // written without the digit

assert_eq!(iso, packed);                          // one contract
assert_eq!(packed.check_digit(), 'U');            // …computed, not invented

// One character wrong is refused rather than routed.
assert!("NL-TNM-000122045-X".parse::<Emaid>().is_err());
# Ok::<(), emob_core::IdError>(())
```

The digit exists to catch a transcription error — a card read wrong, a character
lost in a support form, a column shifted in a partner's export — and an
identifier that has lost one still parses, still routes, and bills a session to
somebody else's contract. A guard nobody evaluates is a character that makes an
identifier one longer, so this one is evaluated.

A DIN card also has an EMI3 spelling, and the conversion is explicit rather than
implicit: `NL-TNM-012204-5` and `NL-TNM-C00122045-K` are one contract written
for two worlds, and `to_emi3()` / `to_din()` say so. They are **not** equal on
their own, because an ISO instance that merely happens to start `C0` would then
collide with a DIN contract nobody issued.

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

Rounding follows the currency's own minor unit rather than a hard-coded two.
ISO 4217 gives the yen no minor unit and the dinar three, so
`round_to_minor_unit` reads `Currency::minor_unit_digits` — a total rounded to
two decimals in yen invents a hundredth of a unit that does not exist.

## One scale, two claims about it

`IdentificationStrength` lives here rather than in either crate that reads it,
because the whole point is that the two can be compared: the **session** knows
which mechanism authorised it, the **signed meter record** states how the
signature component identified the user `[OCMF Tab. 11]`, and when they disagree
the one with a signature behind it wins.

OCMF's error levels are deliberately *not* on the scale. `MISMATCH` and
`INVALID` are failures, not weak assignments, and putting them at the bottom of
an ordered scale would make "the certificate was rejected" compare as slightly
worse than an RFID UID.

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
    // [AFIR Art. 5(1)]  offer a contract-free path at the point, or make the point free of charge
    // [AFIR Art. 5(7)]  connect the point to a CSMS: without it neither the data
    //                   nor the smart-charging duties can be met
    // [LSV26 §4]        file the electronic Inbetriebnahme notice within two weeks
    // …
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Thirty-three duties from AFIR, Delegated Regulation (EU) 2025/656, LSV 2026,
MessEG/PTB-A and the THG-Quote preconditions, as dated, cited, executable rules.
Four properties make it trustworthy:

Three of them come from `[REA 6-A]`, the Regelermittlungsausschuss's e-mobility
rulebook, and all three bind a DC station that meters on the **AC side, before
the rectifier** — a legacy arrangement in which the rectification losses sit
inside the number the customer pays for. It is permitted only below 2018 and at
most 50 kW (the same threshold as AFIR's, pointing the other way), only where
the rectification belongs to one session — which a **shared rectifier in a
multi-outlet cabinet does not** — and only if the customer is **told**. A
platform whose claim is that the customer can check the value owes them the fact
that part of it is loss.


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

**Every duty is assessable against something.** `[AFIR Art. 5(5)]` binds the
*mobility service provider*, not the point, so it is judged against a
`ProviderProfile` rather than stubbed out to report "different scope" for ever:

```rust
use emob_core::obligation::{assess_provider, Verdict};
use emob_core::station::ProviderProfile;
use emob_core::PartyId;
use time::macros::date;

let mut provider = ProviderProfile::bare(PartyId::new("DE", "MSP")?);
provider.surcharges_cross_border_roaming = true;

// The article forbids a cross-border surcharge outright, not merely caps it.
assert_eq!(assess_provider(&provider, date!(2026-09-01)).verdict(), Verdict::Failing);
# Ok::<(), Box<dyn std::error::Error>>(())
```

In Germany one company usually wears both hats, and the provider half is the
half nobody checks. A test asserts that every duty in the calendar is answerable
by exactly one of the two profiles, so none can go quietly unassessable.

## The dates the text actually gives

Three wordings appear in Article 5 and they are not interchangeable:

| Wording | Reads | Duties |
|---|---|---|
| "deployed from 13 April 2024" | `commissioned_on` | 5(1), 5(4)¶1–2 |
| "built after … **or renovated after** …" | both, with different dates | 5(8) |
| "installed **or renovated** from …" | `installed_or_renovated_on()` | `[DA-656 Anh. 2.1]` |

A renovation is not a deployment. Treating it as one drags untouched 2019
hardware into duties written for new equipment.

The same care applies to the DA-656 exemption for legacy points. It is a
**date** — "installed or renovated from 8 January 2026" — not a technology, so a
PWM-only point installed in 2026 fails the duty rather than escaping it because
it fails it.

## The wire is a format, not a round trip

`time`'s derived `Serialize` writes an instant as `[2026, 2, 10, 0, 0, 0, 1, 0, 0]`
— its own internal fields — and a date as `[year, ordinal]`. A `Currency([u8; 3])`
under `#[serde(transparent)]` writes `[69, 85, 82]`. All of it round-trips
perfectly, which is why a `from_str(to_string(x)) == x` test never notices: that
holds for any encoding, including one nobody else can read.

`emob_core::wire` pins the spellings every partner actually uses — RFC 3339 for
an instant, `YYYY-MM-DD` for a date, `HH:MM:SS` for a time of day, whole seconds
for a duration, the day names for a weekday — and `Currency`, `QuarterHour` and
`ClockResolution` write themselves. Reading runs through the validating
constructors, so a bad currency code, a clock resolution above `[REA 6-A §3.1]`'s
cap and a quarter hour off the settlement grid are refused on the way in.

Every identifier here reads back the way it writes, `PartyId` included. A type
with a `Display` and no `FromStr` cannot round-trip, and every caller that reads
a party out of a configuration file, a URL segment or a partner's onboarding
form ends up splitting the string its own way — which is how `DE*ABC` and
`DEABC` come to be two entries in one routing table. Both separators the field
uses are accepted, and so is the packed spelling.

## The spelling of a protocol generation is not ours to choose

`V2gCommunication` states which vehicle-communication generations a point
implements, because `[DA-656 Anh. 2.1.1–2.1.3]` binds a point by exactly that.
Writing it down is where the market goes wrong: `[DATEX-II-Profil Tab. A.130]`
spells its literal `iso15118`, with no generation, *defines* it as -20, and has
no literal for -2 at all. A CPO mapping "we do ISO 15118" onto it publishes a
claim of compliance with a duty that phases in on 01.01.2027 and that its estate
does not meet — in the official record, in a document no schema validator will
object to.

So `protocol_names()` emits the names the [`iso15118`] crate owns — `din70121`,
`iso15118-2`, `iso15118-20` — and `tests/the_kits_agree.rs` asserts they *are*
its names. That crate is a **dev**-dependency and only that: nothing here
decides money with a protocol implementation in its tree, and the agreement is
still a build failure rather than a hope.

[`iso15118`]: https://github.com/hupe1980/iso15118

## No I/O, no clock

Nothing in this crate reads a clock, a socket or a file. Every function that
needs "now" takes it as an argument, so a compliance question about a date two
years out is the same call as one about today.

## License

MIT OR Apache-2.0.
