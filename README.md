# emob

**The open-source e-mobility operating stack** — the CPO and EMP halves of a
charging business in one Rust workspace: the CSMS a station connects to, the
roaming node a partner peers with, the Eichrecht evidence chain a signed meter
value survives in, and the driver contract all of it turns into an invoice.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

> 🚧 **Status: early.** Four domain crates are real, tested and green —
> [`emob-core`](crates/emob-core), [`emob-eichrecht`](crates/emob-eichrecht),
> [`emob-session`](crates/emob-session) and [`emob-cdr`](crates/emob-cdr) —
> with **184 tests** and an end-to-end test that drives a genuinely signed
> session from the meter to a settled record. Everything else is designed and
> not yet built, and this README marks which is which rather than blurring the
> two.

## Why

Open source stops at the charging station. [CitrineOS] is a CSMS, [SteVe] is a
1.6 CSMS, [EVerest] is the station firmware. Nobody open ships the other half —
the e-mobility provider, the roaming node, the calibration-law evidence chain,
the billing — as one system that agrees with itself about what a kilowatt-hour
was worth.

That other half is what this workspace is, and it is built on protocol stacks
that already exist as siblings rather than re-implemented: [`ocpp-kit`],
[`ocpi-kit`], [`oicp-kit`], [`iso15118`], [`eebus`].

[CitrineOS]: https://lfenergy.org/projects/citrineos/
[SteVe]: https://github.com/steve-community/steve
[EVerest]: https://everest.github.io
[`ocpp-kit`]: https://github.com/hupe1980/ocpp-kit
[`ocpi-kit`]: https://github.com/hupe1980/ocpi-kit
[`oicp-kit`]: https://github.com/hupe1980/oicp-kit
[`iso15118`]: https://github.com/hupe1980/iso15118
[`eebus`]: https://github.com/hupe1980/eebus

## The four properties that decide quality

### A value that does not verify does not bill

German calibration law lets you bill a measured value only if the customer can
check it, long after the session — `[MessEG §33]`, `[PTB-A 50.7]`. Every closed
platform asserts this; no open one implements it. Here it is an invariant of the
type system: the only way to obtain a billable quantity is
`Evidence::billable_energy()`, and it returns `None` whenever anything is wrong.

```rust
let evidence = Evidence::assemble(&records, &registry, session_start);

match evidence.billable_energy() {
    Some(energy) => println!("bill {energy}"),        // 29.500 kWh
    None => for reason in evidence.reasons() {
        eprintln!("blocked: {reason}");                // → an operator, not an invoice
    },
}
```

Getting there means parsing OCMF **without destroying it**. The signature covers
the payload bytes as written, so a parser that deserialises and re-serialises to
verify has already lost — key order, whitespace and number formatting each
change the hash. And signatures alone cannot see a *deletion*: drop the middle
records of a session and every remaining signature still verifies, which is why
pagination and the transaction-marker chain are checked as their own question.

### Compliance is a query, not a consulting engagement

```rust
let report = assess(&point, date!(2027-01-01));

for finding in report.failing() {
    println!("{}  {}", finding.obligation.citation, finding.obligation.remedy);
    // [AFIR Art. 5(1)]  offer a contract-free payment path (card reader, or a web/QR flow)
    // [DA-656]          TLS 1.3 and the larger certificates of -20 usually mean a hardware refresh
}
```

AFIR, Delegated Regulation (EU) 2025/656, LSV 2026, MessEG/PTB-A and the
THG-Quote preconditions as dated, cited, executable rules. Every duty carries
its citation — `cargo xtask check-citations` fails the build if one names a
document the source index does not list — and its validity window, so a duty
cannot be applied a year before it exists. Applicability and satisfaction stay
separate questions: a private depot is not *failing* the ad-hoc payment duty,
the duty does not bind it.

### The quarter-hour split conserves energy exactly

Germany's pass-through model settles a session against the quarter hours it
touched `[A6 §IV.1]`. A session running 10:01 to 10:22 crosses a boundary two
thirds of the way through, and seven kilowatt-hours times two thirds does not
terminate.

Computing each slot independently leaves a sum that misses the total, and the
usual fix shoves the remainder into the last slot — misattributing energy to
whoever held 10:15. Instead, the cumulative value at each boundary is computed
once and differences are taken, so the sum **telescopes**: every interior
boundary cancels exactly, whatever it was rounded to.

```rust
let split = split::into_quarter_hours(&series)?;
assert!(split.conserves());        // exactly, for every session
assert!(!split.fully_measured());  // …and it says when it had to interpolate
```

A test runs 144 generated sessions chosen for awkward ratios and asserts it on
every one. Interpolation assumes constant power across a gap, which a tapering
charge curve does not deliver — so the assumption travels with the number, all
the way to the partner's copy of the record.

### Money is never a float, and scale is information

Every quantity here either is money or becomes money.
`cargo xtask no-floats` fails the build on an `f32` or `f64` anywhere in the
workspace — including `Decimal::from_f64`, because a float converted late is
still a float that was wrong early.

```rust
let start = Energy::from_kwh(Decimal::from_str("2935.600")?)?;
let end   = Energy::from_kwh(Decimal::from_str("2965.100")?)?;
assert_eq!((end - start)?.to_string(), "29.500 kWh");
```

Note the trailing zeros. A register reporting `2935.600 kWh` is stating three
decimals of resolution, and OCMF forbids transforming that representation "since
this would change … the number of valid digits". So scale survives every
operation — `Energy::from_wh` even converts by moving the decimal point rather
than dividing, so `29500 Wh` becomes `29.500 kWh` and not `29.5`.

## Layout

| Crate | What it holds | State |
|---|---|---|
| [`emob-core`](crates/emob-core) | Identifiers in both grammars, text-preserving; exact energy/money; the charge-point profile; the obligation calendar | ✅ |
| [`emob-eichrecht`](crates/emob-eichrecht) | OCMF parse/verify, the key registry, the session chain, the evidence record | ✅ |
| [`emob-session`](crates/emob-session) | Authorisation paths, cumulative meter series, and the quarter-hour split | ✅ |
| [`emob-cdr`](crates/emob-cdr) | The record and its builder, idempotent acceptance, pre-flight validation | ✅ |
| `emob-tariff` | CPO/EMP tariffs, ad-hoc pricing, display strings derived from the rating tariff | 📐 |
| `emob-roam` | Canonical ↔ wire translation and its cost notes; partner registry | 📐 |
| `emob-pnc` | Plug & Charge contracts, OPCP pools, multi-PKI | 📐 |
| `emob-poi` | Locations, LSV/AFIR registry state, DATEX II export | 📐 |
| `emob-smart` | Load management, OCPP charging profiles, DER control, § 14a guard, V2G | 📐 |
| `emob-billing` | Rating → EN 16931 e-invoice → SEPA → double-entry postings | 📐 |

Services (`csmsd`, `roamd`, `empd`, `pncd`, `poid`, `tarifd`, `billd`, `opsd`,
`agentd`, `sited`) are 📐 throughout.

## Development

```console
just            # list every recipe
just ci         # fmt, clippy, purity, tests, guards, deny, docs
just test       # cargo test --workspace --all-features
just guards     # no-floats, check-citations, check-manifests
just purity     # no clock, no I/O, no unsafe in the domain crates
```

The domain crates take time and keys as arguments and never read a clock, so a
dispute about a two-year-old session is replayed exactly as it happened — and
`just purity` fails the build if that stops being true.

## Sources

Every regulatory claim in the code cites a document, section and page from
`specs/`, which is gitignored (the documents are third-party and copyrighted)
and indexed with retrieval URLs in `specs/README.md`, so the corpus can be
rebuilt from a fresh clone.

## License

MIT OR Apache-2.0, at your option.
