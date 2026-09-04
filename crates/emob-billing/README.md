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
| `tax` | who owes the VAT, in which country, and **who is liable for it** — two questions, not one. For a roaming settlement the place of supply is not where the charge point stands `[UStG §3g]`, and the reverse charge follows only where the seller is not established there (Art. 195) |
| `en16931` | the document, and the verdict on it: 223 syntax-independent rules plus the national usage specification, run before anything is sent |
| `payment` · `postings` | the collection, and the books |

## A record has two supplies

A CDR carries **two** ratings: the charging session, and the reservation that ran
before it `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. Both reach the document through
one function (D250):

```rust
invoice.lines[0].description;  // "… — reservation"    — it ran first
invoice.lines[1].description;  // "… — energy"
invoice.lines[0].id;           // "1.1"
invoice.lines[1].id;           // "1.2"  — one sequence across both parts
invoice.gross_total();         // == cdr.total_cost().unwrap()
```

Three things the two parts do not share, and each is a field on the document:

- **The window.** A reservation's line states the reservation's own dates in
  BT-134/BT-135, not the session's. A supply dated outside the period it happened
  in is a document a tax office reads differently.
- **The name of the dimension.** The same `TIME` is *charging time* in one part
  and *reservation* in the other, and a document that calls both the first is one
  the driver cannot check against what they were quoted.
- **Nothing else.** The VAT stripping, the rounding, the line numbering and the
  bound-as-allowance are one rule applied twice — two spellings of "turn a rating
  into lines" is the drift this crate exists to prevent.

A reservation priced in a currency the session was not is refused with every
other unsettleable shape: `emob_cdr::validate` blocks the record before this
layer sees it.

## What the document tells the payer

**What is inside the measured value.** `[REA 6-A §3.2]` lets a DC station meter
on the AC side of its rectifier and then obliges the operator to tell *"die von
einem Messwert **oder einer Rechnung** Betroffenen"* that the losses are part of
the number — the invoice, in as many words. So the figure goes in BT-127, on the
line carrying the measured value, from the record's own signed evidence (D253):

```rust
line.compensated_loss;   // Some(0.150 kWh) — from `EvidenceRef`, not from a flag
// BT-127: "CDR DE*ABC*1 — 0.150 kWh of this measured value is compensated
//          cable or rectification loss and is part of the stated value
//          [REA 6-A §3.2]"
```

**What the rating had to report.** Putting every note on an invoice is as wrong
as putting none. A note concerns the **payer** exactly when it says a quantity
was billed differently from how it was measured — a block rounding that charges
for kilowatt-hours nobody delivered, a dimension nothing priced, a line the
clock could not resolve, minutes the operator withheld — which is what
`[AFIR Art. 5(4)]` and `[PAngV]` entitle a driver to reconcile. Those cross as
BG-1 notes coded `AAI`, so a receiving system routes them.
`emob_tariff::RatingNote::concerns_the_payer` is the line. A fault in a document
the payer did not write — a VAT rate with no split, two bounds that contradict —
goes to the operator queue that reads the `Crossing`.

The tariff version stays **off** the document: a line names its record and the
record names the tariff by content, so a second identifier here would be a name
the recipient cannot resolve without the record anyway.

## A bound is not a line

`[OCPI 2.3.0 §Tariff]`'s `min_price` and `max_price` move a session's total
without changing what was delivered, and a maximum moves it **down**. Put on the
document as a line, a cap is a negative amount and a negative BT-146 — and
`BR-27` refuses that outright, so the whole invoice is invalid. EN 16931 has the
group for it:

```rust
invoice.lines.len();                    // 1 — the energy that was delivered
invoice.adjustments[0].kind;             // Allowance — BG-20
invoice.adjustments[0].amount;           // 3.75, a positive magnitude
invoice.line_total();                    // 12.15 — BT-106
invoice.taxable_total();                 // 8.40  — BT-109 = 106 − 107 + 108
invoice.gross_total();                   // 10.00 — the cap, exactly
```

The amount is derived from what the **document** states — the rounded lines less
the rounded target — rather than from the exact difference: rounding the two
independently landed a cent past the cap, and the cap is the one number the
driver was promised. A bound with nothing to adjust *is* the line, because
`BR-16` requires an invoice to have one and a charge has no document to be a
charge on.

And the target is built the way the lines are: **every amount stripped at its
own rate**. An allowance carries one VAT rate — BT-95 and BT-96 — so the bound is
converted on its own and added to the lines' own nets, rather than the record's
whole total being put through one factor. On a €100.00 session made of energy at
19 % beside a session fee at 7 %, the single-factor reading divided the 7 % half
by 1.19 as well and billed **€98.80**.

One shape this cannot carry: an adjustment larger than everything charged at its
rate would state a negative taxable amount under a positive invoice.
`emob-tariff` names it as `RatingNote::AdjustmentExceedsCategory` before the
document is built, so the sign never reaches a document unexplained.

The books move the revenue that bound belongs to, chosen from the largest line
**of that record** — the same scope `emob_tariff::Adjustment::vat` uses for the
rate. Asked of the whole document instead, a month of energy sessions plus one
capped occupancy session books the cap against energy revenue for a session that
delivered none (D214).

## The item price is the net one

BT-146 is defined **exclusive of VAT**. A gross tariff's own figure put there
states a price that does not produce the line — `29.500 × 0.49` is `14.455`
where the line says `12.15`, off by the whole rate, which
`PEPPOL-EN16931-R120` refuses at a hundred times its tolerance. Both the amount
and the price are stripped at the same rate, and every line reproduces itself:

```rust
assert!(invoice.lines.iter().all(InvoiceLine::reconciles));   // and so does
assert!(invoice.reconciles());                                 // the document
```

## A time line is stated in seconds, against a price per hour

OCPI quotes time per hour, and 3600 has two factors of three: twenty-five
minutes is `0.41666…` h, and a line whose quantity is rounded no longer
reproduces its own amount. EN 16931 has the field for exactly this — BT-149,
the item price base quantity — so a time line carries whole seconds at the
tariff's own hourly price:

```rust
line.quantity;        // 1500        — BT-129, in SEC
line.unit_price;      // 6.00        — BT-146, per hour
line.base_quantity;   // 3600        — BT-149: "6.00 EUR per 3600 SEC"
line.net;             // 2.50        — BT-131 = 1500 × 6.00 ÷ 3600, exactly
```

Energy and session lines carry a base quantity of one and are written without
it. A renderer that ignores BT-149 shows a price per second 3600 times too
high, and the crossing's account says so.

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
    .supplied_from("DE", dec("19"))   // the country the points stand in, and its rate
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

C‑60/23 (*Digital Charging Solutions*, 17.10.2024) settles the three-party shape
every roaming session has: where the driver contracts with an e-mobility provider
rather than with the operator of the point, the chain is a **commission
structure** under Article 14(2)(c) — two successive supplies of goods, CPO to
eMSP and eMSP to driver — and the Court held so despite the eMSP controlling
neither when, where nor how much is drawn. That is what makes an eMSP a *taxable
dealer*, and it is the premise everything below rests on.

`[UStG §3g]`, Article 38 of the VAT Directive, then says that a supply of
electricity **to a taxable dealer** is made where that dealer is established. An
e-mobility provider buying sessions through roaming is exactly one: it does not
consume the electricity, it resells it. So a German CPO settling with a French
eMSP is not making a German supply at all — the place of supply is France, and
German VAT does not arise.

### Where the supply is taxed and who pays it are two questions

Article 195 shifts the liability to the recipient — the reverse charge
`[UStG §13b]` — only "if the supplies are carried out by a taxable person **not
established within that Member State**". A CPO with a branch or a VAT
registration in the buyer's country is making an ordinary **local** supply there,
at that country's rate, and a reverse charge on it drops tax that was due. So
establishment is stated rather than inferred from the two countries (D211):

```rust
let cpo = TaxStatus::business("DE", "DE123456789");
TaxTreatment::decide(&cpo, &french_emsp, "DE", &rates)?;    // AE, place FR

let cpo = cpo.also_established_in(["FR"]);
TaxTreatment::decide(&cpo, &french_emsp, "DE", &rates)?;    // S at 20 %, place FR
```

The ad-hoc leg does not share it. A driver paying at the point is not a
reseller, so `[UStG §3g]` never engages and the supply is taxed where the
charge point stands, whatever passport the driver carries. **Two sessions at one
post, a minute apart, can carry different VAT** — which is why the treatment is
decided per invoice from the parties, and not per station from a configuration
field.

```rust
// Both identifiers, or there is no category — EN 16931's BR-AE-2 and BR-AE-3
// refuse the document anyway, and refusing it here names the missing one.
let rates = VatRates::new().at("DE", dec("19"));
let treatment = TaxTreatment::decide(&seller.tax, &buyer.tax, "DE", &rates)?;
assert_eq!(treatment.category, VatCategory::ReverseCharge);
assert_eq!(treatment.place_of_supply, "FR");
```

### The rate belongs to the place of supply, not to the charge point

Which is why the rates are a small table rather than one number. `[UStG §3g]`
moves the place of supply, and it moves it to a country that need not be the one
the points stand in: a German operator running chargers in France and settling
with a German eMSP is taxed in Germany, at 19 %, on kilowatt-hours drawn under a
20 % regime — the seller is established there, so the supply is local and the
rate is the place of supply's own.

```rust
let rates = VatRates::new().at("FR", dec("20")).at("DE", dec("19"));

// The roaming leg — taxed where the reseller is established.
let settlement = TaxTreatment::decide(&cpo, &german_emsp, "FR", &rates)?;
assert_eq!((settlement.place_of_supply.as_str(), settlement.rate), ("DE", dec("19")));

// The ad-hoc leg at the same posts — taxed where they stand.
let ad_hoc = TaxTreatment::decide(&cpo, &driver, "FR", &rates)?;
assert_eq!((ad_hoc.place_of_supply.as_str(), ad_hoc.rate), ("FR", dec("20")));
```

A standard-rated supply whose place of supply has no stated rate is **refused**,
because the two silent alternatives — using the rate that happened to be
supplied, or using zero — are an invoice that over-declares its VAT and one that
under-declares it.

### And a reseller outside the Union is `O`, not `G` — which changes the document

Article 38 moves the place of supply to where the reseller is established. For a
Swiss eMSP that is outside the Union, so **no member state's VAT arises at all**:
the transaction is outside the scope. `G` — free export item — describes goods
that are within scope and zero-rated because they leave the customs territory,
which electricity consumed in the Union is not.

`O` is the only category in UNCL 5305 that **states no rate**, and that is not a
detail. `BR-O-05` refuses a line carrying BT-152 at all, and a rate of zero is
carrying it; `BR-O-02` allows no VAT identifier on either party; and once the
seller's is gone, `BR-CO-26` still wants the buyer to be able to identify its
supplier — so the legal registration BT-30 stops being optional on exactly that
document.

```rust
let cpo = Counterparty::new("Stadtwerke Musterstadt GmbH", "Musterstadt", seller_tax)
    .registered_as("HRB 12345", None);   // BT-30, and on this invoice not optional

let invoice = /* … to a Swiss reseller … */;
assert!(invoice.lines.iter().all(|line| line.vat_rate.is_none()));
assert_eq!(to_en16931(&invoice, Specification::Core)?.value.invoice.seller.vat_identifier, None);
```

The category type is **`en16931`'s own**: all ten codes with four predicates
generated from the CEN artefacts, of which `forbids_exemption_reason` and
`states_rate` are the two that decide the paragraph above. What stays here is the
part `en16931` cannot know — which category two *parties* produce.

### The fee that is not electricity

C‑60/23 also holds that a **periodic subscription** an eMSP charges its driver —
one that buys access rather than kilowatt-hours — is a *separate supply of
services*, under its own place-of-supply rule rather than Article 38 or 39.
Nothing here builds such a line: an invoice is assembled from rated CDRs and
every one of them is electricity. A document carrying both needs a VAT
**category** per line where this crate has one per document, so it is `empd`'s.

And the books agree with the document: under a reverse charge there is **no VAT
posting**, because the liability is the recipient's. A platform that posts 19 %
and removes it from the invoice has a VAT return that reconciles against nothing
it sent.

## The verdict is the deliverable, not the XML

An invoice that serialises and does not validate is an invoice that comes back.
So `to_en16931` returns the semantic document *and* its report, and `write` will
not hand back a document its profile rejects — `Validated<P>` is a type that
cannot be constructed from an invalid invoice, which is the same discipline
`Evidence::billable_energy` applies to a kilowatt-hour one layer down.

```rust
let crossed = en16931::to_en16931(&invoice, Specification::Core)?;
assert!(crossed.value.is_valid());

// The German public buyer's document, or the terms that are missing.
match en16931::write(&invoice, Specification::XRechnung, Syntax::Ubl) {
    Ok(xml) => submit(&xml.value),
    Err(BillingError::NotCollectable { reason }) => eprintln!("{reason}"),
    Err(other) => return Err(other.into()),
}
```

## The specification and the syntax are two questions, and neither has a default

`Specification` is BT-24 **and** the rule set the document is judged by, in one
argument, because they are one decision: a document claiming `XRechnung` because
somebody typed the string, having been checked against the bare core, is the
commonest way an invoice passes local validation and is rejected on receipt.
`write` proves the invoice against the profile and lets the writer stamp BT-24
from the profile that proved it, so the two cannot disagree.

And `[UStG §14]` asks for a document conforming to Directive 2014/55/EU — which
is EN 16931 itself, `Specification::Core`. `XRechnung`'s `BR-DE-*` rules are a
**public-sector** usage specification: they want a Leitweg-ID, a seller contact
and payment instructions that an ordinary business customer neither has nor
needs. Writing every invoice as `XRechnung` refuses lawful B2B documents for want
of a routing identifier the recipient does not issue.

`Syntax` is the other half: UBL and CII are the two CEN/TS 16931-2 makes
mandatory, they carry the same semantic invoice, and which one an access point
takes is a fact about the **recipient**. Every ZUGFeRD payload is a CII document,
so the binding that writes one is the binding the hybrid PDF will want.

`BR-CO-25` is asked at construction rather than at validation, for the same
reason: an invoice with something owing has to say when, the answer is a
commercial term the caller holds, and a finding that names a rule id sends them
looking for it in the standard.

## `live`, not `iter`

A correction is a *new* CDR that supersedes the old one, so a ledger holding a
session and its correction holds both. `InvoiceBuilder::ledger` reads
`CdrLedger::live` — and a caller that assembles the list by hand gets the same
check, because the fault is in the list rather than in where it came from.

## A record its own validator blocks is not one this layer sends

`emob_cdr::validate` already asks everything that makes a record unsettleable —
overlapping periods that bill a minute twice, a line whose own numbers do not
produce its own amount, a price computed for a quantity the record does not
state, an authorisation stronger than the signature supports. `CdrBuilder`
refuses to *issue* such a record; this crate sends the demand, so it asks the
same question of every `Cdr` rather than accepting any that carries a `Cost`.

```rust
let err = InvoiceBuilder::new(…).record(&overlapping).build().unwrap_err();
assert!(matches!(err, BillingError::NotSettleable { .. }));   // …with the reasons
```

The ordinary path is unaffected: a record built by this workspace passes its own
validator by construction, over a thousand generated sessions. The two doors it
covers are the ones that never go through the builder — a partner's document,
which `emob_roam::ocpi::from_ocpi` assembles directly, and anything a service
deserialised.

## …and a German kilowatt-hour needs something behind it

`validate` grades a **missing signature** as a warning on purpose, and says why:
it is blocking for a German energy invoice and merely notable elsewhere, so the
decision belongs to the layer that knows which regime applies. This is that
layer.

`[MessEG §33(3) Nr. 1]` names invoices in as many words — *„Rechnungen, soweit
sie auf Messwerten beruhen"* have to be ones the recipient can simply follow in
order to check the values stated — and §33(1) permits a measured value to be used
in commercial dealings at all only where it is traceable to the measurement. A
line priced per kilowatt-hour or per minute rests on one; with no signed record
behind it there is nothing to check it against, because the transparency file is
made of exactly that evidence. A per-session fee rests on no measurement and
needs nothing.

Judged on where the **measurement** happened rather than on the place of supply —
a German operator settling with a French reseller has moved the place of supply
`[UStG §3g]` and has not moved its meters — and judged for Germany only, because
asserting one member state's metrology law over all of them would refuse lawful
invoices elsewhere.

## A discharge is not a line on the driver's invoice

`Direction` is a field rather than a sign everywhere in this workspace, so a V2G
discharge can never net against a draw. Every layer honours it — the OBIS
register the meter signed, the CDR builder, `to_ocpi` (which refuses an export
**by name**, because OCPI cannot express one), `emob-thg` (which skips it,
because `[38k §6]` counts electricity *withdrawn*). Without the same refusal
here, a 29.5 kWh discharge priced through the same tariff is a valid EN 16931
invoice demanding €14.46 from the person who **supplied** the energy — balanced
postings, collectible pain.008, nothing objecting anywhere.

```rust
let err = InvoiceBuilder::new(…).record(&discharge).build().unwrap_err();
assert!(matches!(err, BillingError::ExportNotBillable { .. }));
```

A discharge is not a smaller sale or a negative one. It is a supply in the other
direction: the driver supplies, the operator buys, which moves the party, the
place of supply and the VAT liability at once — and in Germany it is ordinarily a
self-billed *Gutschrift* `[UStG §14]`, a document with the parties reversed.
Which arrangement applies is a fact about a contract, not about a CDR, so this is
a refusal that names what the document would have to be rather than a guess.

## A cancellation is a kind, not a minus sign

An invoice that was wrong is not deleted and not edited. It is reversed by a
second document — a German *Stornorechnung* — and `Invoice::cancellation` is what
builds it:

```rust
let storno = invoice.cancellation("R-2026-0001-S", date!(2026 - 08 - 15))?;

assert_eq!(storno.kind, DocumentKind::CreditNote);      // BT-3 = 381
assert_eq!(storno.cancels.unwrap().number, "R-2026-0001");  // BG-3
assert_eq!(storno.gross_total(), invoice.gross_total());    // the same money
```

**Nothing is negated.** EN 16931 carries the direction in the *document type*
and states what is being credited as a positive figure. A reversed line would
carry a negative BT-146, which `BR-27` refuses outright — so a "negated" credit
note is not a document at all. It is the same argument that makes a tariff's cap
a document level allowance rather than a line, one layer further out.

The lines, the records and the totals are the invoice's own, which is what lets a
ledger pair the two and see a **reversal** rather than a second sale. UBL spells
the two documents with different root elements, in different namespaces, with
different names for the type code and the line, so a `kind` that did not reach
the writer would produce an `<Invoice>` claiming BT-3 = 381 — a document
`BR-CL-01` refuses on the way in and no schema catches on the way out.

Two things then behave differently, and both are the half a platform gets wrong
silently:

- **`postings_for` reverses every side.** Same accounts, same amounts, the other
  way. Booking a Storno like the invoice it cancels doubles the revenue and the
  VAT liability, and the books disagree with the two documents that were sent —
  at year end, in a reconciliation nobody runs until then.
- **`payment::instruct` refuses it by name.** A direct debit draws money *in* and
  a credit note is money owed *back*. Every figure on it is positive, so no
  arithmetic check would have objected, and the driver would have been debited
  twice.

What stays outside this crate is *which* document a re-rated month supersedes and
*when* — that belongs to the service that issued the first one. What a
cancellation **says**, and what it does to the books, does not.

## It reads no clock

Every date and instant is an argument: the issue date, the due date, the
collection date, the pain.008 timestamp. A billing run replayed two years later
produces the same bytes.

That matters more here than anywhere else in the workspace. `sepa` defaults
several of those fields off the system clock — `IsoDate::today()` for a
collection date, *now* for the message timestamp — and a collection file that
differs between two runs of one job is a file no bank reconciles and no auditor
can check. A test asserts the same inputs produce identical XML.

## It names no accounts, and it links no ledger

`postings_for` produces movements addressed by **role** — receivable, energy
revenue, service revenue, VAT payable at a rate — balanced before an account is
named:

```rust
let books = postings::postings_for(&invoice);
assert!(books.balances());
```

SKR03 and SKR04 disagree about the numbers, so the chart is a service's. So is
the **journal**: posting into one needs accounts, a calendar, a policy and a
database, and none of those can live in a crate that promises to read no clock
and open no socket. `mako` declares `doubleentry` in exactly one manifest —
`services/accountingd` — and in no crate; `billd` is where it belongs here.

It is not only a layering argument. A bookkeeping engine brings the clock in
through the door: `doubleentry` takes `uuid` with `v7`, and a v7 identifier is
generated from `SystemTime::now()`. `just purity` greps this workspace's own
source and cannot see into a dependency, so the promise that two runs of one
billing job produce one file is kept by what the manifests *do not* declare as
much as by what the code does not call.

What crosses the seam is `Postings`: a currency, a booking date, and a balanced
set of role-addressed movements. A role a chart cannot place is a refusal rather
than a dropped posting — a dropped posting is an entry that does not balance and
a trial balance that is quietly wrong.

## And it prices nothing

`emob-tariff` rates a session and `emob-cdr` carries the result. A second engine
here that could produce a different number for the same session is precisely the
drift this workspace exists to make unrepresentable — which is why `en16931`'s
own `billing` adapter is deliberately not enabled.

## The document is checked as a property, not as an example

`tests/a_month_closes.rs` closes one month from a real signature, and every
assertion in it was written after the failure that motivated it — so each is a
shape somebody already thought of. `tests/the_invoice_adds_up.rs` states what
those examples are instances of, over two hundred and fifty generated months
whose tariffs mix VAT rates and carry the minimum and maximum that become a
document level allowance or charge: the document reconciles at every level the
standard states one, the standard's own 317 rules accept it, the rounding
residual is bounded by a minor unit per line and stated rather than absorbed, and
every record supplied is billed exactly once.

## What it stands on

| Crate | For |
|---|---|
| `en16931` | the EN 16931 semantic model and its 317 rules, at the severities the authorities publish |
| `en16931-formats` | UBL and CII — the two syntaxes CEN/TS 16931-2 makes mandatory — and the XRechnung flavour of each |
| `sepa` | pain.008, with IBAN, BIC and Creditor-Identifier validation |

## Licence

MIT OR Apache-2.0
