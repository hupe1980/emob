# billd

The service that closes a month: **when** an invoice is issued, **what number**
it carries, **which one** it supersedes, and **which account** each posting lands
in — and never what a document says.

```console
cargo run -p billd
```

## What it decides, and what it must not

`emob-billing` turns rated records into a document. The rounding that happens
once at the line, the VAT treatment derived from the parties, the EN 16931
semantic invoice and the verdict on it, the pain.008 that draws it, and postings
addressed by **role** — everything that could be *wrong about a document* is
there and is tested there.

Four things are not, and none of them is a property of any document.

## 1. What number it carries

`[UStG §14(4) Nr. 4]` requires *"eine fortlaufende Nummer mit einer oder mehreren
Zahlenreihen, die zur Identifizierung der Rechnung vom Rechnungsaussteller
**einmalig** vergeben wird"*. Unique, and issued by the person issuing the
invoice — which is a fact about a **counter**, not about a month's records. Two
closings that each produce `R-2026-0001` is the failure, and no crate can see it.

```rust
Numbering::series("R", 2026).resuming_after(6)   // the store said 0006
```

**One or more series** is the part worth modelling: the statute explicitly
permits a separate run per year, per branch or per document kind, so an operator
running `R-2026-0001` beside `S-2026-0001` is doing what the paragraph allows
rather than something clever.

And the number is spent **before** the document is built, and stays spent
whatever happens next. A counter that rewound on a rejection would hand the
corrected document a number the refused one already carried.

## 2. When a period is closed, and that it closes once

A month re-closed against the same records is not a second month. It is either
the same invoice — in which case the second closing is the first one's number
spent for nothing — or a **correction**, and which of those it is decides whether
money moves twice. `Billd::issue` refuses; `Billd::rebill` is the second reading,
stated rather than guessed.

## 3. Which invoice supersedes which

A re-rated month cancels the one it replaces, and the order is the same one
`[OCPI 2.3.0 §mod_cdrs]` gives a CDR correction: the reversal first.

```rust
let (credit, replacement) = billd.rebill(
    "R-2026-0001",
    date!(2026 - 07 - 20),
    "re-rated: the June tariff was published at 0.39 and billed at 0.49",
    closing,
)?;
```

Both are numbered from the same series and both are returned, because the
cancellation is not a bookkeeping detail — it is a document the recipient has to
receive before the replacement makes sense. `Invoice::cancellation` builds the
*Stornorechnung* and `postings_for` reverses its books; neither knows whether the
original was ever issued, which is why this is here.

**The reason travels on the document**, as a BT-22 note, rather than into a log
on this side. A recipient's accounts-payable clerk holding a credit note that
does not say why has to telephone for it.

## 4. Which account a role lands in

`emob-billing` addresses a posting by what it is *for* — a receivable, energy
revenue, VAT payable in a named country at a named rate — and stops. That is not
squeamishness: a chart of accounts is an operator's own, it differs between
SKR 03 and SKR 04 before it differs between companies, and a domain crate that
hard-coded `1400` would be wrong for every second deployment while looking
authoritative.

| Role | Default path | SKR 03 |
|---|---|---|
| `Receivable` | `assets:receivable` | `1400` |
| `EnergyRevenue` | `income:energy` | `8400` |
| `ServiceRevenue` | `income:service` | `8400` |
| `VatPayable { rate, place_of_supply }` | `liabilities:vat:DE:19` | `1776` |

The defaults are spelled as **paths** rather than numbers because a path says
what an account is for in the same vocabulary the role does. A deployment that
wants SKR 04 supplies its own with `ChartOfAccounts::mapping`.

### A VAT liability has a creditor

The rate is not the account. C-60/23 (*Digital Charging Solutions*) makes the
network-access fee a **separate and independent** supply of services, so one
document can owe tax in two places at once — the electricity where the point
stands `[UStG §3g]`, the fee where the supplier sits `[UStG §3a(1)]`. A single
`liabilities:vat` account nets a French return into a German one and is right for
neither authority, so the place of supply is in the account path.

```rust
billd.balance_of("liabilities:vat:FR:20", "EUR")?;   // the French filing
billd.balance_of("liabilities:vat:DE:19", "EUR")?;   // and the German one
```

This is the only manifest in the workspace that declares [`doubleentry`]: it
takes `uuid`/`v7`, and therefore a clock, so no crate promising replay may carry
it in its graph — which `cargo xtask check-graph` enforces.

## A document that was not accepted was not issued

`[UStG §14(1)]` lets an electronic invoice be transmitted only in a structured
format meeting the European norm, and a German public buyer's platform **answers**
a submission rather than swallowing it.

```rust
billd.issue("2026-06", closing)?;                    // numbered, not booked
billd.accepted("R-2026-0001", on, Some(reference))?; // the platform took it
billd.book("R-2026-0001")?;                          // …and only now the books
```

That ordering is the whole reason posting is a separate act. A platform that
books on issue and submits afterwards has a trial balance that disagrees with
what the recipient holds, and the disagreement is invisible until somebody
reconciles.

Two second bookings are two different things, and get two answers. A caller that
books the same number twice is repeating themselves, and is told so. A *process*
that crashed between the journal's write and its own flag is not: the entry is
keyed on the invoice number, so the replay finds what it already wrote rather
than posting the month again.

## No I/O in the library

Nothing in the library opens a socket or reads a clock. The journal is in memory
and persisting it is a deployment's job; every date is an argument, so two runs of
one closing produce one set of postings and one set of bytes.

The submission is the one leg CI cannot run: a *Rechnungseingangsplattform* is
somebody else's endpoint behind a registration, exactly as `csmsd`'s WebSocket and
the Mobilithek subscription are.

## Configuration

| Variable | Default | What |
|---|---|---|
| `BILLD_HTTP_BIND` | `127.0.0.1:9584` | the readiness and health endpoint |
| `BILLD_SERIES` | `R` | the number series' prefix |

## License

MIT OR Apache-2.0.
