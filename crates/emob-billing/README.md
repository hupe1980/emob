# emob-billing

Turns rated charging records into invoices, VAT decisions, EN 16931 e-invoices, SEPA direct debits and balanced bookkeeping postings — for charge point operators (CPOs) and e-mobility service providers (eMSPs) who bill in the EU.

[![crates.io](https://img.shields.io/crates/v/emob-billing.svg)](https://crates.io/crates/emob-billing) [![docs.rs](https://img.shields.io/docsrs/emob-billing)](https://docs.rs/emob-billing) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-billing.svg)](#license)

A CDR (charge detail record) is a claim about what happened; an invoice is a
demand a tax authority reads, a partner pays against and an auditor asks for.
This crate makes the four decisions between the two: where the amounts are
rounded, who owes the VAT and where, whether the e-invoice passes the standard's
own rules, and how the money is collected and booked. It reads no clock, prices
nothing and names no accounts, so a billing run replayed two years later
produces the same bytes.

```console
cargo add emob-billing
```

## Example

A German CPO bills a French eMSP for a month of roaming sessions:

```rust
use emob_billing::{Counterparty, InvoiceBuilder, TaxStatus, en16931, postings};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::date;

let ledger: emob_cdr::CdrLedger = load_the_month();   // your rated records

let cpo = Counterparty::new(
    "Stadtwerke Musterstadt GmbH",
    "Musterstadt",
    TaxStatus::business("DE", "DE123456789"),
);
// An eMSP buys sessions to resell them, so the place of supply moves to
// France and the VAT is the recipient's [UStG §3g].
let emsp = Counterparty::new("Mobilité SAS", "Lyon", TaxStatus::reseller("FR", "FR12345678901"));

let crossing = InvoiceBuilder::new(
        "R-2026-0007",
        date!(2026 - 07 - 01),                              // issue date
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),     // billing period, BG-14
        cpo,
        emsp,
    )
    .supplied_from("DE", Decimal::from_str("19")?)          // where the points stand, and its rate
    .ledger(&ledger)                                        // reads `CdrLedger::live`
    .due_on(date!(2026 - 07 - 15))
    .build()?;

let invoice = crossing.value;
assert!(invoice.reconciles());

// The document and the verdict on it, before anything is sent.
let crossed = en16931::to_en16931(&invoice, en16931::Specification::Core)?;
assert!(crossed.value.is_valid());

// The books, balanced before a single account is named.
assert!(postings::postings_for(&invoice).balances());
```

`build` returns a `Crossing`: the invoice plus the account of what it
approximated, naming each record the document does not reproduce exactly by JSON
Pointer into the invoice.

## What it guarantees

- **Rounding happens once, at the line, and is stated.** EN 16931's totals are
  sums of line amounts (`BT-106 = Σ BT-131`), so rounding anywhere else produces
  a document whose lines do not add up to its subtotals.
  `Invoice::rounding_residual` states the gap between the exact records and the
  printed figures.
- **Every line and the document reconcile.** `InvoiceLine::reconciles` and
  `Invoice::reconciles` hold for every invoice the builder returns.
- **The e-invoice is judged before it is sent.** `en16931::to_en16931` returns the
  semantic document and its report against 223 syntax-independent rules plus the
  national usage specification. `en16931::write` refuses with
  `BillingError::NotCollectable` rather than hand back a document its profile
  rejects.
- **VAT follows the parties, not the station.** `TaxTreatment::decide` applies
  the place-of-supply and reverse-charge rules per invoice (see below).
- **Only billable records are billed.** A record `emob_cdr::validate` blocks is
  refused as `BillingError::NotSettleable`; a record offered together with the
  correction that supersedes it as `BillingError::SupersededRecord`. This covers the two paths that bypass
  `CdrBuilder`: a partner's document and anything a service deserialised.
- **A German kilowatt-hour needs signed evidence behind it.** `[MessEG §33(3) Nr. 1]`
  requires invoices based on measured values to let the recipient check them, and
  §33(1) allows a measured value in commerce only where it is traceable to the
  measurement. A per-kWh or per-minute line measured in Germany with no signed
  record is refused (`BillingError::NotVerifiable`); a per-session fee needs
  nothing. The test is where the meter is, not the place of supply.
- **A V2G discharge is never billed to the driver.** A discharge is a supply in
  the other direction — in Germany usually a self-billed *Gutschrift*
  `[UStG §14]` — so it is refused by name (`BillingError::ExportNotBillable`).
- **No clock.** Issue, due and collection dates and the pain.008 timestamp are
  all arguments. `sepa` would otherwise default some of them from the system
  clock, and a collection file that differs between two runs is one no bank can
  reconcile. A test asserts identical inputs give identical XML.

## How it works

### Invoice lines: net prices, two supplies, time in seconds

- **Net item price.** BT-146 is exclusive of VAT. A gross tariff's lines are
  converted at the rate of their own price component (or the supply's rate when
  the component states none), so every line reproduces itself and
  `PEPPOL-EN16931-R120` holds.
- **One rate per line.** The VAT category belongs to the document's supply; the
  rate belongs to the line, because energy (19 %) and a service fee (7 %) can
  differ. `invoice.tax` holds one subtotal per rate, as `BR-S-08` and `BR-S-09`
  require. Under a category that levies no tax the gross is still stripped at the
  quoted rate, and the stated rate is zero (`BR-AE-5`).
- **Reservation and session.** A CDR can carry two ratings, the session and the
  reservation before it `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. Both become lines
  in one sequence (`1.1`, `1.2`), each with its own dates in BT-134/BT-135 and
  its own dimension name.
- **Time lines.** OCPI prices time per hour, and 25 minutes is `0.41666…` h. A
  time line therefore states whole seconds (BT-129) against the hourly price, with
  BT-149 base quantity 3600, so `1500 × 6.00 ÷ 3600 = 2.50` exactly.
- **Notes that concern the payer.** `[REA 6-A §3.2]` lets a DC station meter on
  the AC side and requires the operator to tell whoever receives a measured value
  or an invoice that the losses are included. The compensated loss goes into
  BT-127 from the record's signed evidence. Rating notes that
  `emob_tariff::RatingNote::concerns_the_payer` selects — a quantity billed
  differently from how it was measured, which `[AFIR Art. 5(4)]` and `[PAngV]`
  let a driver reconcile — cross as BG-1 notes coded `AAI`.
- **The driver's receipt.** A line whose session has a receipt page carries it
  (`InvoiceLine::receipt`), appended to BT-127 as "Beleg: <link>", so the payer
  can open the signed evidence behind each line.

### Price caps and minimums are allowances, not lines

A tariff's `max_price` or `min_price` `[OCPI 2.3.0 §Tariff]` changes the total
without changing what was delivered. As a line, a cap would be a negative BT-146,
which `BR-27` refuses. So it becomes a document-level allowance or charge (BG-20
/ BG-21), one per VAT category it draws from, split as
`emob_tariff::Rated::adjustment_parts` states. The amount is derived from the
document's rounded figures, so the gross total lands on the cap exactly. A
deserialised `Rated` whose adjustment exceeds a category is refused as
`BillingError::AdjustmentExceedsCategory`.

### VAT on a roaming settlement

Recharging an EV is a single supply of goods — electricity (CJEU C‑282/22).
Under C‑60/23 (*Digital Charging Solutions*, 17.10.2024) the roaming chain is a
commission structure: CPO → eMSP → driver, two supplies, with the eMSP a
reseller. That decides three things:

| Situation | Treatment | Why |
|---|---|---|
| CPO sells to an EU eMSP not established where the CPO is | reverse charge (`AE`), place of supply = eMSP's country | `[UStG §3g]` / Art. 38: electricity to a reseller is supplied where the reseller is established; Art. 195 / `[UStG §13b]` shifts the liability |
| CPO also established in the eMSP's country (`TaxStatus::also_established_in`) | standard rate of that country | the reverse charge applies only to a seller **not** established there |
| Ad-hoc driver at the point | standard rate where the point stands | a driver is not a reseller, so `[UStG §3g]` never engages |
| eMSP outside the EU (e.g. Switzerland) | outside scope (`O`) | no member state's VAT arises; `O` states no rate (`BR-O-05`), forbids VAT ids (`BR-O-02`), so the seller's legal registration BT-30 becomes mandatory (`BR-CO-26`) |

Rates come from a `VatRates` table keyed by country, because the place of
supply need not be where the points stand. A standard-rated supply whose place
of supply has no rate is refused (`BillingError::NoVatRate`) rather than taxed
at zero or at the wrong rate.

```rust
let rates = VatRates::new().at("FR", dec("20")).at("DE", dec("19"));
let settlement = TaxTreatment::decide(&cpo, &german_emsp, "FR", &rates)?;  // DE, 19 %
let ad_hoc     = TaxTreatment::decide(&cpo, &driver,      "FR", &rates)?;  // FR, 20 %
```

**Subscriptions are a separate service.** C‑60/23 also holds that a periodic
fee charged *"regardless of whether the user actually purchased electricity"* is
a separate supply of services, taxed where the supplier sits for a private
customer `[UStG §3a(1)]`. Add it with `InvoiceBuilder::subscription` and give
the supplier's rate with `InvoiceBuilder::vat_rate_in`; the document then carries
two tax subtotals.

### Specification and syntax are explicit

`Specification` sets BT-24 and the rule set in one argument, so a document can
never claim XRechnung after being checked against the core only.
`Specification::Core` is plain EN 16931, which `[UStG §14]` requires for B2B.
`Specification::XRechnung` adds the German public-sector `BR-DE-*` rules (Leitweg-ID,
seller contact, payment instructions) and would refuse lawful B2B invoices if
used for everything. `Syntax` picks UBL or CII, the two syntaxes CEN/TS 16931-2
makes mandatory; which one is a fact about the recipient.

### Crediting one invoice line

`Invoice::line_credit` issues a monetary adjustment against one numbered line. Supply the net and VAT already credited on that line. The credit preserves the source tax category and references the preceding invoice. If a split cannot reconcile the VAT cents, the method returns an error. [Credit-note lines](https://docs.peppol.eu/poacc/billing/3.0/syntax/ubl-creditnote/cac-CreditNoteLine/) · [VAT rounding rule](https://docs.peppol.eu/poacc/billing/3.0/rules/ubl-tc434/BR-CO-17/)

### Cancellations (*Stornorechnung*)

`Invoice::cancellation` builds a credit note (BT-3 = 381) referencing the
original in BG-3, with the same positive figures — EN 16931 carries the
direction in the document type, and a negated line breaks `BR-27`. Then
`postings::postings_for` reverses every side, and `payment::instruct` refuses
the credit note (`PaymentError::NotAnInvoice`) so a driver is never debited
twice.

### Postings by role, not by account

`postings::postings_for` produces movements addressed by `Role` — receivable,
energy revenue, service revenue, VAT payable at a rate — balanced before an
account is named. Under a reverse charge there is no VAT posting. Charts of
accounts (SKR03, SKR04, IFRS) differ, and a journal needs a calendar and a
database, so mapping roles onto accounts and posting into a ledger is a
service's job — in emob, `billd`'s. A role a chart cannot place must be refused
there, never dropped.

### Logs never show a full IBAN

`Debug` of `Mandate`, `Creditor` and `PaymentDetails` masks the IBAN as
`DE…3000` and names as `…` (`payment::mask_iban`), so a log line or a panic
message does not carry bank details. `Display` of an IBAN is untouched: the
documents need it.

## API at a glance

| Item | Purpose |
|---|---|
| `InvoiceBuilder` | assemble an invoice from records or a `CdrLedger`, with parties, period, rates, subscriptions |
| `Invoice` | the document: lines, adjustments, tax subtotals, totals, `reconciles`, `rounding_residual`, `cancellation`, `line_credit` |
| `Counterparty`, `TaxStatus` | seller/buyer identity and VAT status (`consumer`, `business`, `reseller`, `also_established_in`) |
| `TaxTreatment::decide`, `TaxTreatment::decide_service` | VAT category, place of supply and rate for electricity / for a service fee |
| `VatRates` | standard rates by country |
| `en16931::to_en16931` | semantic EN 16931 document plus validation report |
| `en16931::write` | UBL or CII XML, only if the profile accepts it |
| `payment::instruct` | SEPA pain.008 direct debit for an invoice |
| `postings::postings_for` | balanced, role-addressed postings |
| `BillingError`, `PaymentError` | every refusal, by name |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | `Serialize`/`Deserialize` on every public type |

## Standards and sources

- EN 16931 (semantic model and its 317 business rules), CEN/TS 16931-2 (UBL, CII), XRechnung
- VAT Directive 2006/112/EC Art. 14, 38, 195; `[UStG §3g]`, `[UStG §3a(1)]`, `[UStG §13b]`, `[UStG §14]`; CJEU C‑282/22 and C‑60/23
- `[MessEG §33(3) Nr. 1]`, `[REA 6-A §3.2]` — measured values on invoices
- `[AFIR Art. 5(4)]`, `[PAngV]` — price transparency to the driver
- `[OCPI 2.3.0 §Tariff]`, `[OCPI 2.3.0 §mod_cdrs_cdr_object]`
- SEPA pain.008 via `sepa`; EN 16931 via `en16931` and `en16931-formats`

The test `tests/the_invoice_adds_up.rs` checks, over 250 generated months with
mixed VAT rates and price bounds, that every document reconciles at every level,
passes the standard's 317 rules, bounds its rounding residual by one minor unit
per line, and bills each record exactly once.

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices. This crate
sits after `emob-tariff` (rating) and `emob-cdr` (records) and before `billd`,
the daemon that closes a month. The reasoning and regulation are on the site
under [Settlement and billing](https://hupe1980.github.io/emob/docs/settlement/);
see also the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
