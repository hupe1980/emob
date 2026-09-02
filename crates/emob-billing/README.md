# emob-billing

Rated charging records turned into money: invoice lines whose **rounding is
stated rather than discovered**, the German VAT treatment of a roaming
settlement as an executable rule, an EN 16931 e-invoice with the verdict on it
before anything is sent, a SEPA collection that reads no clock, and double-entry
postings that balance before a single account is named.

```console
cargo add emob-billing
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Settlement and billing](https://hupe1980.github.io/emob/docs/settlement/)**.
The signatures are on [docs.rs](https://docs.rs/emob-billing).

## A record is a claim; an invoice is a demand

Everything upstream answers *what happened* — a meter signed a register, a chain
held up, a split conserved, a tariff priced the periods the split produced.
`emob-cdr` ends with a record that carries its own energy and its own price and
can prove both.

An invoice is the document a tax authority reads, a partner pays against and an
auditor asks for. Four things have to happen between the two, and each is a
decision rather than a mapping.

| | |
|---|---|
| `invoice` | the exact, unrounded amounts become figures in a currency's minor unit — **once, at the line** — and the difference is stated |
| `tax` | who owes the VAT, and in which country. For a roaming settlement that is **not** where the charge point stands `[UStG §3g]` |
| `en16931` | the document, and the verdict on it: 223 syntax-independent rules plus the national usage specification, run before anything is sent |
| `payment` · `postings` | the collection, and the books |

## The rounding happens once, at the line, and says so

`29.500 kWh × 0.49` is `14.45500`, and `emob-tariff` keeps every digit on
purpose: rounding per line and then summing gives a different answer from
summing and then rounding, and which is correct is a tax question rather than an
arithmetic one. This is the layer that answers it.

**Which basis.** EN 16931's line amount (BT-131) is always net, so a gross
tariff's lines are converted first — at the rate its own component carries, or at
the supply's where the component states none.

**At which rate.** The **category** is a property of the document — a supply is a
reverse charge or it is not — and the **rate** is a property of the *line*,
because electricity and a service fee can sit in different categories. An invoice
that taxed both at one rate would over-declare on one of them, and `BR-S-08` and
`BR-S-09` are the standard's own rules that say so.

```rust
invoice.lines[0].vat_rate;   // 19 — the energy component's own
invoice.lines[1].vat_rate;   //  7 — the service fee's
invoice.tax.len();           //  2 — one taxable amount per rate
```

Under a category that does not levy tax the gross is still stripped at the rate
the tariff quoted — a price with 19 % in it is not a price the recipient of a
reverse charge owes 19 % on — and the rate the document *states* is zero, which
`BR-AE-5` and its siblings require.

**Where.** Per line, and nowhere else. The standard's totals are sums of the
line amounts — `BT-106 = Σ BT-131`, and per category `BT-116 = Σ BT-131` — so
rounding anywhere else produces a document whose own lines do not add up to its
own subtotals.

**And the difference is on the document.** `Invoice::rounding_residual()` is the
gap between what the records came to exactly and what the invoice states; the
tax follows from the rounded figure, so that residual is the whole of what the
document approximates. The `Crossing` the builder returns names each record the
document does not reproduce, by JSON Pointer into the invoice.

```rust
let crossing = InvoiceBuilder::new("R-2026-0001", issued, period, cpo, driver)
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)      // `live`, never `iter` — see below
    .due_on(due)
    .build()?;

assert_eq!(crossing.value.taxable_total().to_string(), "26.08 EUR");
assert!(crossing.value.reconciles());
// …and what that cost, per record.
for reason in crossing.reasons() { println!("{reason}"); }
```

## The tax rule every platform gets wrong

Recharging an EV is a **single composite supply of goods** — the electricity —
not a bundle of services. The Court of Justice settled that in C‑282/22.

`[UStG §3g]`, Article 38 of the VAT Directive, then says that a supply of
electricity **to a reseller** is made where that reseller is established. An
e-mobility provider buying sessions through roaming is exactly a reseller: it
does not consume the electricity, it resells it. So a German CPO settling with a
French eMSP is not making a German supply at all — the place of supply is
France, German VAT does not arise, and the invoice states the reverse charge
with the partner's own VAT identifier on it `[UStG §13b]`.

The ad-hoc leg does not share it. A driver paying at the point is not a
reseller, so `[UStG §3g]` never engages and the supply is taxed where the
charge point stands, whatever passport the driver carries. **Two sessions at one
post, a minute apart, can carry different VAT** — which is why the treatment is
decided per invoice from the parties, and not per station from a configuration
field.

```rust
// Both identifiers, or there is no category — EN 16931's BR-AE-2 and BR-AE-3
// refuse the document anyway, and refusing it here names the missing one.
let treatment = TaxTreatment::decide(&seller.tax, &buyer.tax, "DE", dec("19"))?;
assert_eq!(treatment.category, VatCategory::ReverseCharge);
assert_eq!(treatment.place_of_supply, "FR");
```

And the books agree with the document: under a reverse charge there is **no VAT
posting**, because the liability is the recipient's. A platform that posts 19 %
and removes it from the invoice has a VAT return that reconciles against nothing
it sent.

## The verdict is the deliverable, not the XML

An invoice that serialises and does not validate is an invoice that comes back.
So `to_en16931` returns the semantic document *and* its report, and `xrechnung`
will not hand back a document its profile rejects — `Validated<XRechnung>` is a
type that cannot be constructed from an invalid invoice, which is the same
discipline `Evidence::billable_energy` applies to a kilowatt-hour one layer
down.

```rust
let crossed = en16931::to_en16931(&invoice, en16931::CEN_CORE)?;
assert!(crossed.value.is_valid());

// The German public buyer's document, or the terms that are missing.
match en16931::xrechnung(&invoice) {
    Ok(xml) => submit(&xml.value),
    Err(BillingError::NotCollectable { reason }) => eprintln!("{reason}"),
    Err(other) => return Err(other.into()),
}
```

`BR-CO-25` is asked at construction rather than at validation, for the same
reason: an invoice with something owing has to say when, the answer is a
commercial term the caller holds, and a finding that names a rule id sends them
looking for it in the standard.

## `live`, not `iter`

A correction is a *new* CDR that supersedes the old one, so a ledger holding a
session and its correction holds both. `InvoiceBuilder::ledger` reads
`CdrLedger::live` — and a caller that assembles the list by hand gets the same
check, because the fault is in the list rather than in where it came from.

## It reads no clock

Every date and instant is an argument: the issue date, the due date, the
collection date, the pain.008 timestamp. A billing run replayed two years later
produces the same bytes.

That matters more here than anywhere else in the workspace. `sepa` defaults
several of those fields off the system clock — `IsoDate::today()` for a
collection date, *now* for the message timestamp — and a collection file that
differs between two runs of one job is a file no bank reconciles and no auditor
can check. A test asserts the same inputs produce identical XML.

## It names no accounts

`postings_for` produces movements addressed by **role** — receivable, energy
revenue, service revenue, VAT payable at a rate — and `entry_for` turns them
into a `doubleentry` draft once a caller has supplied the mapping. SKR03 and
SKR04 disagree about the numbers and neither is a domain crate's business.

A role the caller's chart cannot place is a refusal, not a dropped posting: a
dropped posting is an entry that does not balance and a trial balance that is
quietly wrong.

```rust
let books = postings::postings_for(&invoice);
assert!(books.balances());          // before an account is named
let draft = postings::entry_for(&books, id, &invoice.number, |role| chart.get(role))?;
```

## And it prices nothing

`emob-tariff` rates a session and `emob-cdr` carries the result. A second engine
here that could produce a different number for the same session is precisely the
drift this workspace exists to make unrepresentable — which is why `en16931`'s
own `billing` adapter is deliberately not enabled.

## What it stands on

| Crate | For |
|---|---|
| `en16931` | the EN 16931 semantic model and its 317 rules, at the severities the authorities publish |
| `en16931-formats` | UBL, and the XRechnung flavour of it |
| `sepa` | pain.008, with IBAN, BIC and Creditor-Identifier validation |
| `doubleentry` | exact integer money and an entry that is balanced by construction |

## Licence

MIT OR Apache-2.0
