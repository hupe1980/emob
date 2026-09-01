+++
title = "Tariffs and price transparency"
weight = 4
description = "Why the price a driver is shown and the price they are charged come from one object, why rating has to walk the session's periods for tiers to mean anything, and why a per-minute-only tariff is unlawful above 50 kW."
+++

# Tariffs and price transparency ✅

`[AFIR Art. 5(4)]` regulates the ad-hoc price at a public charge point more
tightly than most implementations realise. Four rules, each a checkable property
of a tariff rather than an opinion about one.

## The price shown is the price charged

A charging tariff has two jobs that platforms normally implement twice: it
**rates** a finished session, and it is **displayed** before the session starts.
When those come from two places they drift — the screen reads a field somebody
typed into a CMS, the invoice reads the tariff engine, and one of them was
updated. That is the price-transparency breach the article exists to prevent,
and it is almost never malice.

Here `describe()` and `rate()` read the same `PriceComponent` values off the
same `Tariff`, **and select the applicable element through the same predicate**:

```rust
let tariff = Tariff::simple(id, Currency::EUR, TariffKind::AdHoc, vec![
    PriceComponent::new(Dimension::Flat, dec("0.50")),
    PriceComponent::new(Dimension::Energy, dec("0.49")),
]);

// What the driver sees, before anything happens.
assert_eq!(describe(&tariff, at).one_line(), "0.49 EUR / kWh · 0.50 EUR / session");

// What the driver pays, from the same numbers.
assert_eq!(rate(&tariff, &session).gross().to_string(), "14.96 EUR");
```

Two implementations of "which element applies" would be exactly the drift this
crate exists to prevent, one level down — so `describe` asks the same question
`rate` asks about a session's first period, and the tier shown is the tier the
first kilowatt-hour is billed at.

## A session is a sequence of periods

This is the one that decides whether tiers mean anything.

"The first 10 kWh at 0.39, the rest at 0.59" is a restriction on how much has
been delivered **so far**. Rate the session against one element chosen from its
*total* and a 30 kWh session reprices all thirty at 0.59 — including the ten the
driver was quoted at 0.39. The tariff is legal; the invoice is wrong.

So `rate` walks the periods in order, carrying the cumulative energy and
duration, and asks which element applies at the start of each one:

```rust
let session = Chargeable::new(vec![
    Period::charging(t0, t1, kwh("10")),
    Period::charging(t1, t2, kwh("10")),
    Period::charging(t2, t3, kwh("10")),
])?;

let rated = rate(&tiered, &session);
assert_eq!((rated.lines[0].unit_price, rated.lines[0].quantity), (dec("0.39"), dec("10")));
assert_eq!((rated.lines[1].unit_price, rated.lines[1].quantity), (dec("0.59"), dec("20")));
assert_eq!(rated.quantity_for(Dimension::Energy), dec("30"));  // and nothing is lost
```

The quarter-hour slots the [settlement layer](@/docs/settlement.md) already
produces are exactly the right periods. **The split that conserves energy is the
input that prices it** — and a session that crosses into a night rate is priced
on both sides of the boundary rather than wholly at whichever rate it started
under, which a whole-session reading cannot express at all.


### …and the period is cut at the threshold

Walking the periods is not enough on its own. Asking only at the **start** of
each one leaves the tier boundary wherever the caller's periods happened to
land: hand `rate` one period of 15 kWh under "the first 10 kWh at 0.39" and all
fifteen are charged at 0.39, while the same session as three periods of five is
charged correctly. A price that depends on the granularity of the input is not a
price.

So the thresholds themselves become the cut points. Every energy, duration and
**wall-clock** restriction in the tariff is applied to every period that crosses
one — **energy exactly at an energy threshold**, because energy is what is being
tiered and what is being settled; **time exactly at a clock threshold**, because
22:00 is 22:00; and the other quantity in proportion, to the second:

```rust
// One period of 30 kWh, three of ten, or ninety-six quarter hours:
assert_eq!(rate(&tiered, &session).exact_total().amount(), dec("15.70"));

// …and one period 21:00→23:00 under "0.30 from 22:00" is the same money
// as two periods cut at 22:00.
assert_eq!(rate(&night, &coarse).exact_total(), rate(&night, &fine).exact_total());
```

The pieces are differences of cumulative values, so they telescope back to the
period's own total to the last digit — the same construction the quarter-hour
split uses.

Which cut binds depends on where the periods come from. A quarter-hour-split
session is already on the settlement grid, and a lawful price change is on that
grid too `[PTB-A 50.7 §3.1.7.2]`, so its slots never span a clock boundary. Every
other caller needs it: a price quote rating a whole session as one period, and a
partner's CDR sliced however the sender chose.

### Charging time and occupancy are stated, not inferred

A period that moved energy is charging time. One that did not may or may not be
occupancy, and guessing is how a **taper** gets billed at the parking rate: a
car at 100 % state of charge can leave a quarter hour at exactly `0.000 kWh`
while the session's own state machine says it was charging. `Period::charging`
carries the answer rather than deriving it, and the CDR takes it from the
session history.

## The order is the regulation's, not a designer's

Below 50 kW the article prescribes it in as many words:

> The applicable price components shall be presented in the following order:
> — price per kWh; — price per minute; — price per session; and — any other
> price component that applies.

`Dimension` is declared in exactly that order and derives `Ord`, so **sorting
the components is complying with the article**. A list in any other order is not
a styling choice, it is a breach — and because the rating sorts the same way, an
invoice and a price display show the same things the same way round without
either knowing about the other.

Note *per minute*. Tariffs are stored per hour because that is what OCPI
carries; the conversion happens once, in the display layer, so nobody keeps the
same rate twice in two units.

## A tariff with tiers cannot be described by one set of numbers

The same subparagraph asks for "all its price components", and a tariff that
charges the first ten kilowatt-hours at one price has two. Showing only the one
that applies at the moment of asking is how a driver arriving at 21:58 is quoted
the day rate for a session billed at the night one.

```rust
assert_eq!(describe(&tariff, at_noon).full_disclosure(), vec![
    "0.29 EUR / kWh (22:00–06:00)",
    "0.49 EUR / kWh",
]);
```

Each `Tier` carries its conditions in words and says whether it `applies_now`.
`describe()` takes the instant too, so a display can answer *what will this cost
at 22:00* rather than only *what does it cost now*.

## A tariff can be unlawful on its own

At 50 kW and above:

> the ad hoc price charged by the operator **shall be based on the price per
> kWh** for the electricity delivered. In addition, the operators of those
> recharging points **can charge an occupancy fee as a price per minute** to
> discourage long occupancy.

So the same tariff is lawful on one post and unlawful on the next:

```rust
let by_the_minute = /* Dimension::Time only */;

assert!(check_afir(&by_the_minute, dec("22")).is_lawful());   // fine on a 22 kW post
assert!(!check_afir(&by_the_minute, dec("150")).is_lawful()); // unlawful beside it
```

`check_afir` catches the subtler shape too. An energy price *plus* a charge for
**charging time** passes the "is it energy-based" test and still breaches,
because the article permits a fee for *occupancy* — sitting there after charging
has finished — not a rate for the transfer dressed up as time.

And the trap inside the same article: it **does** list "price per session" — in
the subparagraph that governs points *below* 50 kW. At 50 kW and above the
station must show "the ad hoc price per kWh and any possible occupancy fee
expressed in price per minute", which is two components and no more, so a
per-session fee could not lawfully be displayed — and a charge the driver could
not have been shown before starting defeats the comparison the whole article
exists to enable. The lawful component set on a fast charger is exactly `{kWh}`
or `{kWh, occupancy}`:

```rust
let with_session_fee = Tariff::simple(/* … */ vec![
    PriceComponent::new(Dimension::Energy, dec("0.49")),
    PriceComponent::new(Dimension::Flat, dec("0.50")),
]);

assert!(check_afir(&with_session_fee, dec("22")).is_lawful());   // named below 50 kW
assert!(!check_afir(&with_session_fee, dec("150")).is_lawful()); // and not above it
```


### …and unlawful in a unit rather than a shape

The article asks for a **price per minute**. OCPI carries time prices per hour.
Sixty has a factor of three, so an ordinary occupancy fee of €2.50 an hour is
€0.041666… a minute and has no exact decimal spelling at all — and `describe`
was handing a station twenty-eight digits to show a driver.

Rounding it shows a price the tariff does not charge, which is the
display-versus-bill drift this crate exists to make unrepresentable; not
rounding it shows nobody anything. Neither is a price "known to end users before
they initiate a recharging session", so it is a breach with the remedy in the
message: quote an hourly rate divisible by three. €6.00 an hour is €0.10 a
minute (D77).

A VAT rate of exactly −100 % is refused for the same kind of reason one layer
along: a gross amount is `net × (1 + rate/100)`, so at that rate there is no net
at all and an invoice under the tariff could state no taxable amount
`[UStG §14]`. It is also the one value that would divide by zero inside the
rating engine (D75).

It also reports what is merely incoherent: a minimum above the maximum (no
session total satisfies both), and an element that sits behind an unrestricted
one and can therefore never apply — lawful, and almost always a mistake.

Contract tariffs are not judged by this rule. Art. 5(4) governs the ad-hoc
price; a provider's own contract price falls under Art. 5(5), which is a
disclosure duty rather than a shape duty, and which the [obligation
calendar](@/docs/compliance.md) assesses against a provider profile.

## Every term of the total is a line

```rust
assert!(rated.lines_sum_to_total());
```

One line per *distinct price* that applied, with its quantity, unit price and
amount — so a tiered session shows both tiers, which is what a tiered invoice
has to do. The total is their sum plus at most one `Adjustment`, the minimum or
maximum, which is a term a reader can see rather than a number that moved. There
is no term in the total that is not one of those two things.

Rounding a quantity up to a `step_size` block produces a note as well, because
it is always against the customer. It applies once, to what was billed for a
price — rounding every quarter hour up to a block would bill a two-hour session
eight times over. OCPI 3.0 removes the field and advises setting it to 1, so a
tariff relying on it is one that will have to change, and `check_afir` says so
without calling it unlawful.

Notes serialise. "This total was rounded up to a block" and "this element could
not be evaluated" are exactly the facts a settlement dispute turns on, and a note
that stays behind in the process that produced it is a note nobody can invoke.

## A tariff has an identity over time

A tariff id is a name, and names get reused. A CPO that edits a tariff in place
keeps the id, so a record naming only the id names something that no longer
exists — and a partner re-rating it gets a different total and cannot tell an
honest price change from a restated one.

The same answer the evidence chain gives one layer down: **name it by content**.

```rust
assert!(cdr.was_priced_with(&tariff));   // not "does the id match"
```

`Tariff::fingerprint()` is a SHA-256 over a canonical encoding of everything
that can change a price — the bounds, the window, and every element with its
restrictions and components **in order**, because the first matching element
prices the period. Scale is part of it: `0.49` and `0.490` are numerically equal
and are two different prices to show a driver.

And `[AFIR Art. 5(4)]` settles which version governs a session. The price must
be "known to end users **before they initiate** a recharging session", so a CPO
that raises its price at 10:15 has not raised it for the driver who plugged in
at 10:00:

```rust
let cdr = CdrBuilder::from_session(&session, Direction::Import)?
    .rated_with_history(&history)?      // the version in force at the start
    .build()?;
```

`TariffHistory` refuses overlapping versions at construction — an instant with
two prices has no rule for choosing — and reports gaps, because an interval
nothing can be priced in is worth knowing about before a session lands in one.
Windows are half-open, `[from, until)`, so consecutive versions partition the
timeline exactly.

The window is also constrained: `[PTB-A 50.7 §3.1.7.2]` says "Ein Tarifwechsel
ist erst mit dem Beginn der nächsten Messperiode durchzuführen", so a version
that begins or ends off the quarter-hour grid is **refused at construction**. A
change landing mid-period would put one settlement slot under two tariffs, and
the error names the *next* boundary rather than the nearest, because a price may
not start applying earlier than it was published.


## Tax is a breakdown, not a number

Electricity and a service fee can sit in different VAT categories, and EN 16931 —
mandatory between German businesses from 2027 `[UStG §14]` — wants the taxable
amount per rate. A total stating only the gross cannot be checked.

```rust
for line in rated.tax_summary() {
    println!("{} %: net {} + tax {} = {}", line.rate, line.net, line.tax, line.gross);
    // 7 %:  net 1.00 + tax 0.07 = 1.07
    // 19 %: net 42.02 + tax 7.98 = 50.00
}
assert_eq!(rated.net().amount() + rated.tax().amount(), rated.gross().amount());
```

`TaxIncluded::Yes` means the component prices are gross and the tax is stripped
out of them; `No` means they are net and it is added on. `total()` reports the
basis the tariff states; `gross()` reports what the driver pays.

A minimum charge that tops the total up is taxed at the rate of the largest line.
That is a choice rather than a derivation, so it is a **field on the adjustment**
that a reader can see and an accountant can argue with, rather than an assumption
buried inside a sum.

## Rounding happens once, and division happens last

Every line is computed exactly and kept exact; only the total and the tax
breakdown round, to the currency's minor unit, half away from zero. Rounding per
line and then summing gives a different answer, and which of the two is correct
is a tax question rather than an arithmetic one — so the exact figures survive
and the caller can do either.

And the order of operations matters. 35 minutes at €6.00 an hour is

```text
6.00 × 2100 / 3600   = 3.50                              ✅
6.00 × (2100 / 3600) = 3.4999999999999999999999999998    ❌
```

Time therefore accumulates in whole seconds and converts to hours once, after
the multiplication by the price.

Which is why a line carries **two** quantities. `unit_price` is per hour, because
that is how a tariff is written, and twenty-five minutes is `0.41666…` hours — so
`quantity × unit_price` cannot be the amount. `base_quantity` is the whole
seconds, and it is what the amount comes from:

```rust
assert!(rated.lines_reconcile());   // base_quantity × unit_price / 3600 == amount
```

A billing layer that needs a quantity and a price whose product is the line total
quotes the seconds; the hours figure is what a driver reads.

## The clock a time-of-day restriction is read against

"0.30 from 22:00" is local civil time, and an `OffsetDateTime` carries a **UTC
offset, not a time zone** — so the only frame the rating can judge it in is the
one each period states. That is exact for every session this workspace assembles,
across a clock change included, as long as the periods either side carry the
offsets their readings did.

A session assembled from a partner's timestamps can carry two, and then every cut
lands in the first period's frame. `rate` reports that as `MixedUtcOffsets`.
Nothing consults a time-zone database: a replayed rating that depended on the
installed `tzdata` is the one thing a two-year-old dispute cannot afford.

## The two restrictions everybody gets wrong

**A window that wraps midnight** — `22:00` to `06:00` — is a night tariff, not
the empty range a naïve `start <= t && t < end` makes of it.

```rust
assert_eq!(rate(&t, &at_23_00).lines[0].unit_price, night);
assert_eq!(rate(&t, &at_03_00).lines[0].unit_price, night);
assert_eq!(rate(&t, &at_noon).lines[0].unit_price, day);
```

**A restriction this crate cannot evaluate is not an absent restriction.**
`Restrictions::unevaluable` carries anything a wire adapter parsed and this crate
cannot judge — an OCPI `reservation` condition, a partner extension. An element
holding one **never matches**, and the rating says so in a note:

```rust
assert!(rated.reasons().any(|r| r.contains("cannot evaluate")));
```

Silently treating an unknown condition as absent applies a price under
conditions nobody checked — the same mistake as billing on an unverified
signature, one layer up. For an *ad-hoc* tariff `check_afir` calls it a breach
outright, because a price whose conditions cannot be checked cannot be shown
before the session either.

Beyond those, time of day, calendar date, cumulative energy, cumulative duration,
average power and weekday all select which element applies.

## What is not here yet 📐

The EN 16931 invoice itself — the breakdown above is the input it needs, and
`emob-billing` is where it becomes an XRechnung or ZUGFeRD document, a SEPA
mandate and a double-entry posting. Reservation and booking are OCPI concepts
this crate carries verbatim rather than evaluating.
