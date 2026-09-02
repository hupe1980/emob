# emob-tariff

EV charging tariffs where the price shown to the driver is *derived from* the
tariff that rates the session — so the two cannot drift — rated period by period
with periods cut at the tariff's own thresholds so tiers tier at any
granularity, with the VAT breakdown EN 16931 needs, components ordered
as AFIR prescribes, and a conformance check that knows a per-minute-only tariff
is unlawful above 50 kW.

```console
cargo add emob-tariff
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Tariffs and price transparency](https://hupe1980.github.io/emob/docs/pricing/)**.
The signatures are on [docs.rs](https://docs.rs/emob-tariff).


## One tariff, four readers

A price leaves this crate four ways, and the whole design is that it is one
number:

| Reader | Through | Duty |
|---|---|---|
| the driver, at the point, before starting | `describe()` → OCPP 2.1 `SetDefaultTariff` (`emob-ocpp`) | `[AFIR Art. 5(4)]` |
| the session being invoiced | `rate()` | the money |
| the roaming partner | `emob-roam` → OCPI 2.3.0 | what two companies settle |
| the national access point | `emob-poi` → DATEX II | `[AFIR Art. 20(2)(c)]` |

Almost every stack computes that number in four places and reconciles none of
them against the invoice.

## One tariff, two readers

A charging tariff has two jobs that platforms normally implement twice: it
**rates** a finished session, and it is **displayed** before the session starts
(`[AFIR Art. 5(4)]`, `[PAngV]`). When those come from two places they drift —
the screen reads a CMS field somebody typed, the invoice reads the tariff
engine, and one of them was updated.

Here `rate()` and `describe()` read the same `PriceComponent` values off the
same `Tariff`. Neither can quote a number the other does not use.

```rust
let tariff = Tariff::simple(
    "ad-hoc".parse()?,
    Currency::EUR,
    TariffKind::AdHoc,
    vec![
        PriceComponent::new(Dimension::Flat, dec("0.50")),
        PriceComponent::new(Dimension::Energy, dec("0.49")),
    ],
);

// What the driver sees — per kWh first, whatever order it was written in.
assert_eq!(describe(&tariff, at).one_line(), "0.49 EUR / kWh · 0.50 EUR / session");

// What the driver pays, from the same numbers.
let session = Chargeable::energy_only(kwh("29.500"), at, at + Duration::minutes(30))?;
let rated = rate(&tariff, &session);
assert_eq!(rated.gross().to_string(), "14.96 EUR");
```

A test walks every dimension and asserts that each displayed price equals the
price that rated it. `describe()` and `rate()` also pick the applicable element
through the **same predicate**, so the tier shown is the tier the first
kilowatt-hour is billed at — two implementations of "which element applies"
would be exactly the drift this crate exists to prevent, one level down. So does
the OCPP 2.1 crossing in `emob-ocpp`: OCPP orders prices *inside* each dimension
and picks the first whose conditions match, which is OCPI's per-dimension rule
read from the other side, so the projection is the one `matching_component`
already performs and the charge point selects the component the invoice is built
from.

## A session is a sequence of periods, and that is not a detail

Rating the whole session against one tariff element is the reading almost every
implementation starts with, and it is wrong on tiers. "The first 10 kWh at 0.39,
the rest at 0.59" is a restriction on how much has been delivered *so far*.
Judged against the session total instead, a 30 kWh session reprices all thirty
at 0.59 — including the ten the driver was quoted at 0.39.

So `rate` walks the periods in order, carries the cumulative energy and duration,
and asks which element applies **at the start of each one**:

```rust
let session = Chargeable::new(vec![
    Period::charging(t0, t1, kwh("10")),
    Period::charging(t1, t2, kwh("10")),
    Period::charging(t2, t3, kwh("10")),
])?;

let rated = rate(&tiered, &session);
assert_eq!(rated.lines[0].unit_price, dec("0.39"));   // 10 kWh
assert_eq!(rated.lines[1].unit_price, dec("0.59"));   // 20 kWh
```

The quarter-hour slots `emob-session` already produces are exactly the right
periods, so **the split that conserves energy is the input that prices it** —
and a session that crosses into a night rate is priced on both sides of the
boundary rather than wholly at whichever rate it started under.


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

A clock threshold is read in the session's own UTC offset, because that is the
frame the restrictions are matched in, and on every day the period spans: an
overnight session crosses `22:00` and `06:00` on two different dates.

### Charging time and occupancy are stated, not inferred

A period that moved energy is charging time. One that did not may or may not be
occupancy, and guessing is how a **taper** gets billed at the parking rate: a
car at 100 % state of charge can leave a quarter hour at exactly `0.000 kWh`
while the session's own state machine says it was charging. `Period::charging`
carries the answer rather than deriving it, and the CDR takes it from the
session history.

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

The encoding borrows nothing — not another crate's `Display`, and not a derived
`Debug`. A fingerprint that moved because `time` reformatted a date would split
one tariff into two across a dependency bump; one that moved because a variant
was renamed would do it across a refactor. Every enum goes in through a declared
token (`Dimension::as_str` and its siblings), spelled the way OCPI and OCPP
spell it. And a *set* of weekdays goes in sorted, because `[Mon, Tue]` and
`[Tue, Mon]` are one restriction and one price.

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
amount per rate. A total with no breakdown cannot be checked.

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

### …and "no rate" is not "no answer"

A whole tariff's rate is asked wherever a gross price bound has to be stated
before tax — OCPI's `PriceLimit`, OCPP 2.1's `Price` — and the question has
**three** answers rather than two. `Tariff::vat_basis` gives all three:

```rust
match tariff.vat_basis() {
    VatBasis::Rate(rate) => …,   // every component agrees on one
    VatBasis::Unstated   => …,   // none states a rate at all
    VatBasis::Mixed      => …,   // they disagree — no single taxable amount
}
```

`rate()` is what arithmetic needing a divisor asks, and reads `Unstated` as zero
per cent — exactly what `tax_summary` already does with an absent rate.
`stated()` is what a record that has to *write* a rate down asks, and gives
nothing for either absence. Only `Mixed` has no answer.

Collapsing the first two into one `None` made an ordinary gross price list with a
`min_price` and no VAT rate anywhere unpublishable — to a roaming partner and to
a charge point alike — with a diagnostic about a second rate it did not have.

## The order is the regulation's, not a designer's

Below 50 kW the article prescribes it in as many words:

> The applicable price components shall be presented in the following order:
> — price per kWh; — price per minute; — price per session; and — any other
> price component that applies.

`Dimension` is declared in exactly that order and derives `Ord`, so **sorting
the components is complying with the article** — on the display and on the
invoice alike. A list in any other order is not a styling choice, it is a
breach.

Note *per minute*. Tariffs are stored per hour because that is what OCPI
carries; `describe()` converts once, in one place, so nobody is tempted to keep
the same rate twice in two units.

And sixty has a factor of three, so the conversion does not always terminate: an
ordinary occupancy fee of €2.50 an hour is €0.041666… a minute and has no exact
decimal spelling. Rounding it would show a price the tariff does not charge, so
`check_afir` refuses it and names the remedy — an hourly rate divisible by three.
€6.00 an hour is €0.10 a minute.

## A tariff can be unlawful on its own

At 50 kW and above, `[AFIR Art. 5(4)]` says the ad-hoc price "shall be based on
the price per kWh", with an occupancy fee per minute permitted **in addition**.

```rust
let by_the_minute = Tariff::simple(/* … */ vec![
    PriceComponent::new(Dimension::Time, dec("6.00")),   // 0.10 a minute
]);

assert!(check_afir(&by_the_minute, dec("22")).is_lawful());   // fine on a 22 kW post
assert!(!check_afir(&by_the_minute, dec("150")).is_lawful()); // unlawful beside it
```

`check_afir` also catches the subtler shape: an energy price *plus* a charge for
**charging time** passes the first test and still breaches, because the article
permits a fee for occupancy — sitting there once charging has finished — not a
rate for the transfer dressed up as time.

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

It also reports a minimum above the maximum (no total satisfies both), an element
that sits behind an unrestricted one and can therefore never apply, a VAT rate of
−100 % (a gross amount is `net × (1 + rate/100)`, so there is no net at all and
an invoice could state no taxable amount `[UStG §14]`), and — for an ad-hoc
tariff — an element whose conditions cannot be evaluated, because a price whose
conditions cannot be checked cannot be shown before the session either.

Contract tariffs are not judged by this rule. Art. 5(4) regulates the ad-hoc
price; a provider's own contract price falls under Art. 5(5), which is a
disclosure duty rather than a shape duty — and `emob-core` assesses that one
against a `ProviderProfile`.

## Every term of the total is a line

```rust
let rated = rate(&tariff, &session);
assert!(rated.lines_sum_to_total());
```

One `Line` per *distinct price* that applied, with its quantity, unit price and
amount — so a tiered session shows both tiers, which is what a tiered invoice
has to do. The total is their sum plus at most one `Adjustment`, the minimum or
maximum, which is a term a reader can see rather than a number that moved. There
is no term in the total that is not one of those two things.

Rounding up to a `step_size` block produces a `RatingNote`, because it is always
against the customer — and OCPI 3.0 removes the field, advising `step_size = 1`,
so a tariff relying on it is one that will have to change. The rounding applies
once, to what was billed for a price, not once per period: rounding every
quarter hour up to a block would bill a two-hour session eight times over.

Notes serialise, deliberately. "This total was rounded up to a block" and "this
element could not be evaluated" are exactly the facts a settlement dispute turns
on, and a note that stays behind in the process that produced it is a note
nobody can invoke.

## Exactness

Rounding happens **once**, at `total()` or in the tax breakdown. Every line is
computed and kept exact, because rounding per line and then summing gives a
different answer, and which is correct is a tax question rather than an
arithmetic one.

And the arithmetic divides last. 35 minutes at €6.00 an hour is
`6.00 × 2100 / 3600 = 3.50` exactly; `6.00 × (2100 / 3600)` is
`3.4999999999999999999999999998`. Time accumulates in whole seconds and converts
once, after the multiplication.

Which is why a `Line` carries **two** quantities. `unit_price` is per hour,
because that is how a tariff is written, and twenty-five minutes is `0.41666…`
hours — so `quantity × unit_price` is not the amount. `base_quantity` is the
whole seconds, exact, and it is the figure the amount came from:

```rust
assert!(rated.lines_reconcile());   // base_quantity × unit_price / 3600 == amount
```

## Restrictions, including the two everybody gets wrong

Time-of-day, date, energy, duration, power and weekday restrictions select which
element applies — and each of them is also a **cut point**, so the answer never
depends on how finely the caller sliced the session. That includes the local
midnight a weekday restriction turns on, which is the one threshold the tariff
never names: without it a session running Friday 23:00 to Saturday 01:00 arrives
as one period and is priced for both hours at Friday's rate.

They are read against the **UTC offset the period carries**, because an
`OffsetDateTime` knows an offset and not a time zone. Every session
`emob-session` assembles carries one per period, so that is exact — including
across a clock change, where the periods either side carry the offsets their
readings did. A session assembled from a partner's timestamps can carry two, and
then `rate` reports `MixedUtcOffsets` rather than placing an hour of night rate
on the wrong side of a boundary in silence.

**A window that wraps midnight** — `22:00` to `06:00` — is the night tariff it
is, rather than the empty range a naïve comparison makes of it.

**A restriction this crate cannot evaluate is not an absent restriction.**
`Restrictions::unevaluable` carries anything a wire adapter parsed and this crate
cannot judge — an OCPI `reservation` condition, a partner extension. An element
holding one **never matches**, and the rating says so in a note. Silently
treating it as unrestricted applies a price under conditions nobody checked.

## Per minute, and the hour that has no such price

`[AFIR Art. 5(4)]` asks for a price per **minute**. OCPI carries time prices per
hour. Sixty has a factor of three, so an ordinary occupancy fee of €2.50 an hour
is €0.041666… a minute and has no exact decimal spelling at all:

```rust
assert_eq!(price_per_minute(dec("6.00")), Some(dec("0.10")));
assert_eq!(price_per_minute(dec("2.50")), None);
```

`Option`, not a division. Rounding shows a price the tariff does not charge;
not rounding shows twenty-eight digits. Neither is "known to end users before
they initiate", so `check_afir` calls it a breach with the remedy in the
message — quote an hourly rate divisible by three — and `emob-ocpp` **refuses**
the crossing outright, because OCPP 2.1's field is per minute and there is
nothing honest to write in it. One function answers for all three.

## A tariff with tiers cannot be described by one set of numbers

`[AFIR Art. 5(4)]` asks for "all its price components", and a tariff that charges
the first ten kilowatt-hours at one price has two. `describe()` therefore returns
a `Tier` per element, each with its conditions in words:

```rust
assert_eq!(describe(&tariff, at).full_disclosure(), vec![
    "0.29 EUR / kWh (22:00–06:00)",
    "0.49 EUR / kWh",
]);
```

It takes the instant too, so a display can answer *what will this cost at 22:00*
and not only *what does it cost now* — because showing a single set of numbers
for a time-varying tariff is how a driver arriving at 21:58 is quoted the day
rate for a session billed at the night one.

## License

MIT OR Apache-2.0.
