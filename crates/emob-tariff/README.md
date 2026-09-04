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
them against the invoice. The two that drift first are the top two: the screen
reads a CMS field somebody typed and the invoice reads the tariff engine, and
one of them was updated. Here `describe()` and `rate()` read the same
`PriceComponent` values off the same `Tariff` (`[AFIR Art. 5(4)]`, `[PAngV]`),
and neither can quote a number the other does not use.

```rust
let tariff = Tariff::simple(
    "ad-hoc".parse()?,
    Currency::EUR,
    TariffKind::AdHoc,
    TimeZone::new("Europe/Berlin")?,
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
22:00 is 22:00; and the other quantity in proportion, to the second. An energy
threshold is cut even in a period with no second to hold it — a second of a
350 kW charge is a tenth of a kilowatt-hour — and the slice it opens is then
degenerate in time and exact in kilowatt-hours. Such a slice has no average
power, so an element restricting on one cannot price it, and the energy is
reported as an `Unpriced` note rather than folded into a tier it merely began
in:

```rust
// One period of 30 kWh, three of ten, or ninety-six quarter hours:
assert_eq!(rate(&tiered, &session).exact_total().amount(), dec("15.70"));

// …and one period 21:00→23:00 under "0.30 from 22:00" is the same money
// as two periods cut at 22:00.
assert_eq!(rate(&night, &coarse).exact_total(), rate(&night, &fine).exact_total());
```

The pieces are differences of cumulative values, so they telescope back to the
period's own total to the last digit — the same construction, and the same
function (`emob_core::apportion`), the quarter-hour split uses.

The claim is a **property**, not a list of examples: `tests/` generates two
thousand tariffs and sessions, rates each at three resolutions and asserts one
price, one accounted-for quantity per dimension, and one line that explains its
own amount.

A clock threshold is read on the wall clock of the tariff's own zone — never
the offset the timestamps happen to carry, see below — and on every day the
period spans: an overnight session crosses `22:00` and `06:00` on two different
dates.

### Charging time and occupancy are stated, not inferred

A period that moved energy is charging time. One that did not may or may not be
occupancy, and guessing is how a **taper** gets billed at the parking rate: a
car at 100 % state of charge can leave a quarter hour at exactly `0.000 kWh`
while the session's own state machine says it was charging. `Period::activity`
carries the answer rather than deriving it, and the CDR takes it from the
session history.

### …and "not charging" is two facts, which is why it is not a boolean

`[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]` corrected its own definition of
`PARKING_TIME` — from "time not charging" to "time during which the **vehicle is
not requesting power**" — and said why: under the old reading drivers "would be
exposed to penalizing loitering fees … when the EVSE is not offering energy to
the vehicle while the vehicle is still requesting power".

So `emob_core::Activity` has three values, and only one of them owes a fee:

| | priced as | energy transferred |
|---|---|---|
| `Charging` | `TIME` | ✅ |
| `Parked` — the vehicle stopped asking | `PARKING_TIME` | |
| `Withheld` — the point stopped offering | *nothing* | |

`Withheld` is a charging profile at zero, a `[EnWG §14a]` dimming, a grid limit,
a fault; OCPP names it `SuspendedEVSE`. Priced by **neither** dimension — OCPI
has only "time charging" and "time the vehicle was not requesting power" — and
`RatingNote::WithheldNotPriced` reports the seconds, so a session whose clock ran
an hour and whose invoice prices forty minutes explains itself.

```rust
// Same duration, same energy, same tariff. One driver left a full car on the
// post; the other was curtailed by the operator.
assert_eq!(full_battery.total().to_string(), "12.00 EUR");
assert_eq!(curtailed.total().to_string(),   "10.00 EUR");
```

### A span the clock cannot resolve is not a line

`[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen Zeitspanne werden
nicht für Abrechnungszwecke verwendet." A duration is a measured value, and the
measuring instrument's resolution is a fact about the session rather than the
tariff — so `Chargeable` carries it, at the regulation's sixty-second cap unless
the station's type approval states better, and a time dimension whose whole
measured span falls below it is dropped with `RatingNote::DurationBelowResolution`:

```rust
let rated = rate(&occupancy_tariff, &session);        // 30 s of occupancy, 60 s clock
assert_eq!(rated.amount_for(Dimension::ParkingTime), None);
assert!(rated.amount_for(Dimension::Energy).is_some());  // the kilowatt-hours bill

let rated = rate(&occupancy_tariff, &session.with_clock(ten_second_clock));
assert_eq!(rated.amount_for(Dimension::ParkingTime), Some(dec("0.05")));
```

Judged on the measurement as a whole rather than per period, because a session
sampled every second still measured its minutes.

### The specification's own answers, as a test file

`tests/the_specification_states_its_own_answers.rs` holds the sessions
`[OCPI 2.3.0]` walks through in prose, with the totals its own breakdown tables
publish: the `step_size` transitions, the power and duration bands, the two-rate
breakdowns, the price limits, the reservations, and the rounding at a VAT rate
with a decimal in it.

Every other test here asserts what this engine does; these assert what the
document two companies settle against says the answer **is**. A change that keeps
the others green and moves one of these has changed what emob charges a driver.

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

*Which* version governs is decided here; *telling* the four readers is
[`tarifd`](../../services/tarifd)'s, and it publishes **before** a version takes
effect rather than when it does — for the same reason the article gives.

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

## A price bound is two ceilings, not one ceiling in two spellings

`[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` gives `min_price` and `max_price` a
figure **before** taxes and a figure **after** them, and prints the same NOTE
under each:

> As the taxes on a Charging Session might be different for different parts of
> the Session, there might be situations where the maximum cost after taxes is
> reached earlier or later than the maximum price before taxes. So as a rule,
> **they both apply**.

One figure with the other derived works while the tariff has one VAT rate. A
session fee at the standard rate beside energy at a reduced one — the shape the
specification's own `max_price` example uses — makes the two ceilings
non-proportional, and keeping one lets the total past the other: **€11.05 gross**
under a published maximum of €11.00.

So `PriceLimit` carries both. Both are optional here and `before_taxes` is
mandatory on the wire, so the **crossing** derives it at the tariff's own rate,
where the residual is a note. OCPP 2.1's `Price` carries both, in `exclTax` and
`inclTax`.

### …and the two limits bind together

The same sentence is true one level up. Answering whichever limit was asked about
first gets each right alone and the pair wrong: a minimum can lift a total
**above** the maximum, and a maximum can cut one **below** the minimum — which is
invisible before the cut, because a floor only shows itself once the ceiling has
been taken.

Every limb of both limits states a target for a total, and every total is the
lines' own plus the movement times a constant, so all four are bounds on **one
number**. The minimum's give a floor, the maximum's a ceiling, and the answer is
the movement closest to zero inside the interval they leave. An empty interval is
`RatingNote::LimitsContradict`, and the **maximum** is applied, because a
published ceiling is what the driver was shown.

A maximum deeper than the session's own total is held at zero — a cap below
nothing is a payment to the driver — and an adjustment larger than everything
charged at its own VAT rate is `RatingNote::AdjustmentExceedsCategory`, because
EN 16931 gives an allowance one category and that category's taxable amount would
go negative.

### …and it is decided on the exact total

Reaching the limb the lines are *not* quoted in needs the other basis's total,
and the VAT breakdown is the wrong place to read it: it states one taxable amount
per rate and **rounds each**, which is right for a document and wrong for a
computation whose output is a term of the exact total. Half a cent of rounding
becomes a cent of price — and, since an apportioned energy's last digits depend
on how finely the session was sliced, a price that depends on the slicing.

`Rated::exact_bases` is what the bound reads. The consequence is stated rather
than chased: `Rated::total` rounds once and is the figure a cap is a promise
about; `Rated::gross` rounds per category and can sit a minor unit above it, and
is the figure a document reconciles against.

## A reservation is priced, and it is not a period of the session

`[OCPI 2.3.0]` prices a reservation through a **restriction** rather than a
dimension — `TariffRestrictions.reservation`, either `RESERVATION` or
`RESERVATION_EXPIRES` — with `FLAT` and `TIME` components, *"where `TIME` is for
the duration of the reservation"*. Its window *"starts when the reservation is
made, and ends when the driver starts charging on the reserved EVSE/Location, or
when the reservation expires"*, so it has already run before the session begins.

`rate_reservation` is its own entry point over that window, sharing the
session's arithmetic. Rated as a period it would collide: a tariff whose
unrestricted element prices `TIME` and whose reservation element prices `TIME`
would have the two competing for one dimension, and the per-dimension rule drops
one of them silently.

Three rules come from the worked examples rather than the prose:

- **An expired reservation is priced by the reservation elements and nothing
  else** — tariff 18 bills € 9.00, not € 9.50: there is no session for the
  session fee to be the fee of.
- **On an expiry both kinds apply**, in list order, so `RESERVATION_EXPIRES`
  takes a dimension it states and `RESERVATION` supplies the rest.
- **`min_price` and `max_price` do not** — they bound *"a Charging Session"*.

The cost travels in `total_reservation_cost` and inside `total_cost`, which is
what makes a partner's own sum of the parts close, and it becomes its own group
of lines on the EN 16931 invoice `emob-billing` builds — the seam it was missing
from, and the one place a missing term is silent, because a document that omits
it still adds up to itself (D250). It reaches no document that has no reservation
in it: OCPP 2.1's *Tariff and Cost* block refuses the element by name,
`[DATEX-II-Profil]`'s `EnergyRate` omits it with a note, the
`[AFIR Art. 5(4)]` shape check reads only the session elements, and
`Restrictions::describe` puts the word *reservation* first in a tier's condition
so a driver's display cannot read a hold fee as a charging rate.

A window that ends before it starts is the one input `rate_reservation` refuses,
and it refuses it by **collapsing** rather than by returning nothing: no minutes
are priced, a `FLAT` fee with no duration in it is still owed, and
`RatingNote::ReservationWindowReversed` travels with the record. A silent zero on
a visibly broken document is the answer nobody queries.

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

## `step_size` is a property of the session, not of a price

`step_size` is the block a dimension is billed in — 500 Wh, fifteen minutes — and
the obvious implementation rounds each line up to its own.
`[OCPI 2.3.0 §mod_cdrs_step_size]` says otherwise:

> When calculating the cost of a charging session, `step_size` SHALL only be
> taken into account **once per session** for the `TariffDimensionType` `ENERGY`
> and **once for `PARKING_TIME` and `TIME` combined**.

Two families. Within one, the total is rounded with the block of the **last
relevant Price Component** and the surplus billed at that component's **price**:
25 minutes at €1.20 and 20 at €2.40, not 30 and 30. And where both time
dimensions are used, only the **total parking duration** is rounded — 21 minutes
charging then 16 parked, ten-minute blocks on both, bills 21 + 20.

Rounded per price instead, the specification's own tiered energy example costs
€1.31 where it should cost €1.18: an eleven per cent over-charge, in the
direction `[AFIR Art. 5(4)]` and `[PAngV]` exist to police.

The ceiling is taken on a quantity **rounded first**, to `APPORTIONED_SCALE`. It
is the only operation here that is not continuous: the same session summed from a
coarser and a finer set of periods differs in `Decimal`'s twenty-seventh place,
and 64.821 kWh is exactly 1271 blocks of 51 Wh one way and 1272 the other — 51 Wh
the driver did not take, for a difference no meter could state. A nanowatt-hour
is the scale at which this crate already declares two slicings of one session to
be the same session, so the block count is computed there and the billed total is
exactly the whole number of blocks the note names.

## …and the one restriction a period carries nothing to cut on

Cutting works because a period *contains* the fact about where a threshold was
crossed: one period of 15 kWh says by construction that the tenth kilowatt-hour
fell inside it. Energy, duration and the wall clock all accumulate.

Average power does not — it is `energy / duration` over whatever window is asked
about, and a period carries **no** information about the power inside it:

```rust
// 60 kWh in one hour, under "below 50 kW at 0.30, otherwise 0.60".
rate(&t, &one_period).total()    // 36.00 EUR — one window, averaging 60 kW
rate(&t, &two_halves).total()    // 34.50 EUR — 55 kWh then 5, averaging 110 and 10
```

Neither is an arithmetic error, and no cut recovers the second from the first.
The finer answer is a **better measurement**; what the engine cannot do is make a
low-resolution input behave like a high-resolution one. So the total is a
function of the session *at the resolution its periods carry* — rate the periods
the meter produced, which is what `Session::split` hands over, and
`RatingNote::PowerJudgedPerPeriod` says why a partner's coarser document differs.

## Restrictions, including the three everybody gets wrong

Time-of-day, date, energy, duration, power and weekday restrictions select which
element applies — and each of them is also a **cut point**, so the answer never
depends on how finely the caller sliced the session. That includes the local
midnight a weekday restriction turns on, which is the one threshold the tariff
never names: without it a session running Friday 23:00 to Saturday 01:00 arrives
as one period and is priced for both hours at Friday's rate.

They are read against the **wall clock of the tariff's own zone** —
`Tariff::time_zone`, an IANA name. A `22:00` night rate is local civil time at
the charge point `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, and OCPI
carries that zone on the Location, where it is mandatory
`[OCPI 2.3.0 §mod_locations_location_object]`.

Not the UTC offset the timestamps carry. An offset is what a clock was written
with; a zone is the rule that decides it. Judged against the offset, one physical
session under a German night tariff costs €6.00 stamped `+01:00` and €9.00
stamped `Z` — and the second is the ordinary case, because every session an eMSP
re-rates from a roaming partner arrives in UTC.

```rust
// The same two hours, three spellings, one price.
for (from, to) in [
    (datetime!(2026-01-02 21:00 +0), datetime!(2026-01-02 23:00 +0)),
    (datetime!(2026-01-02 22:00 +1), datetime!(2026-01-03 00:00 +1)),
    (datetime!(2026-01-03 06:00 +9), datetime!(2026-01-03 08:00 +9)),
] {
    let session = Chargeable::energy_only(kwh("20"), from, to)?;
    assert_eq!(rate(&night, &session).total().to_string(), "6.00 EUR");
}
```

The zone also places the cuts, and a civil time is not always an instant: a
spring gap swallows an hour, so the clock passes `02:30` once; an autumn fold
repeats one, so it passes `02:30` **twice** and a window ending there ends twice.
Both are cut.

The database is compiled in rather than read — nothing opens
`/usr/share/zoneinfo` or looks at `TZ`, `Cargo.lock` pins its version, and a tzdb
release moves future offsets rather than the frozen ones a settled session has.

**A window that wraps midnight** — `22:00` to `06:00` — is a night tariff, not
the empty range a naïve `start <= t && t < end` makes of it.

**An `end_time` of `00:00` is the end of the day, not the start of it.**
`end_time` is exclusive everywhere else, and the specification tells authors to
write exactly this: "to stop at end of the day use: `00:00`". Read as an
exclusive instant it closes the window before it opens — the element matches
nothing, prices nothing, and the whole session comes back `Unpriced`, as a note
rather than an error. It hides behind the common shape, because
`{start_time: "20:00", end_time: "00:00"}` takes the wrap-around arm and comes
out right by accident.

**A restriction this crate cannot evaluate is not an absent restriction.**
`Restrictions::unevaluable` carries what a wire adapter parsed and this crate
cannot judge — OCPI's `reservation`, `min_current`/`max_current`, a partner
extension. An element holding one **never matches**, and the rating says so.
Treating it as absent applies a price under conditions nobody checked.

**A window that wraps midnight** — `22:00` to `06:00` — is the night tariff it
is, rather than the empty range a naïve comparison makes of it.

**A restriction this crate cannot evaluate is not an absent restriction.**
`Restrictions::unevaluable` carries anything a wire adapter parsed and this crate
cannot judge — OCPI's `min_current`/`max_current`, a partner extension. An element
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
