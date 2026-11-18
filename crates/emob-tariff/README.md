# emob-tariff

EV charging tariffs for charge point operators and e-mobility providers: one
tariff object rates the session, produces the price the driver is shown, and
checks itself against AFIR and the German price-indication rules.

[![crates.io](https://img.shields.io/crates/v/emob-tariff.svg)](https://crates.io/crates/emob-tariff) [![docs.rs](https://img.shields.io/docsrs/emob-tariff)](https://docs.rs/emob-tariff) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-tariff.svg)](#license)

Most stacks compute a charging price in several places — the station display, the
invoice, the roaming feed — and reconcile none of them. Here `describe()` and
`rate()` read the same `PriceComponent` values off the same `Tariff`
(`[AFIR Art. 5(4)]`, `[PAngV]`), so the price shown cannot drift from the price
charged. Rating is period by period with tiers cut exactly at their thresholds,
the VAT breakdown is the one EN 16931 e-invoices need, and every rounding,
adjustment and unpriced quantity is stated on the result.

```console
cargo add emob-tariff
```

## Example

```rust
use emob_tariff::{Chargeable, Dimension, PriceComponent, Tariff, TariffKind, describe, rate};
use emob_core::{Currency, Energy, TimeZone};

let tariff = Tariff::simple(
    "ad-hoc".parse()?,
    Currency::EUR,
    TariffKind::AdHoc,
    TimeZone::new("Europe/Berlin")?,
    vec![
        PriceComponent::new(Dimension::Flat, dec("0.50")),
        PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
    ],
);

// What the driver sees — per kWh first, whatever order it was written in.
assert_eq!(describe(&tariff, at).one_line(), "0.49 EUR / kWh · 0.50 EUR / session");

// What the driver pays, from the same numbers.
let session = Chargeable::energy_only(
    Energy::from_kwh(dec("29.500"))?,
    at,
    at + time::Duration::minutes(30),
)?;
let rated = rate(&tariff, &session);
assert_eq!(rated.gross().to_string(), "14.96 EUR");   // 29.500 × 0.49 + 0.50
assert_eq!(rated.tax().to_string(), "2.31 EUR");      // 19 % of the electricity
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What it guarantees

- **Shown price = charged price.** A test walks every dimension and asserts the
  displayed price equals the price that rated it; `describe()` and `rate()`
  select the applicable element through the same predicate
  (`matching_component`), and so does the OCPP 2.1 projection in `emob-ocpp`.
- **The price does not depend on how the session was sliced.** Property tests
  generate two thousand tariffs and sessions, rate each at three resolutions,
  and assert one price, one accounted quantity per dimension, and lines that
  explain their own amounts.
- **The specification's own worked examples pass.**
  `tests/the_specification_states_its_own_answers.rs` holds the sessions OCPI
  2.3.0 walks through, with the totals its tables publish.
- **Every term of the total is visible**: one `Line` per distinct price, at most
  one `Adjustment` (minimum or maximum), and a `RatingNote` for everything else.
- **Rounding happens once**, at `total()` or the tax breakdown; lines stay exact.
- **No input takes the process down.** `Decimal` panics on overflow, so prices
  and limits that could overflow a downstream sum leave their dimension unbilled
  with a `RatingNote::PriceOutOfRange` / `LimitOutOfRange` note.
- **No I/O, no clock.** Every instant is an argument, so a display can answer
  "what will this cost at 22:00?".

## One tariff, four readers

| Reader | Through | Duty |
|---|---|---|
| the driver, at the point, before starting | `describe()` → OCPP 2.1 `SetDefaultTariff` (`emob-ocpp`) | `[AFIR Art. 5(4)]` |
| the session being invoiced | `rate()` | the money |
| the roaming partner | `emob-roam` → OCPI 2.3.0 | what two companies settle |
| the national access point | `emob-poi` → DATEX II | `[AFIR Art. 20(2)(c)]` |

## Rating: periods, tiers and thresholds

"The first 10 kWh at 0.39, the rest at 0.59" restricts on energy delivered *so
far*. Judged against the session total, a 30 kWh session would reprice all
thirty at 0.59. `rate` therefore walks the periods in order, carrying cumulative
energy and duration:

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

The quarter-hour slots `emob-session` produces are the right periods, so the
split that conserves energy is the input that prices it.

**Every threshold is a cut point.** Energy, duration, wall-clock and weekday
(local midnight) restrictions cut any period that crosses them — energy exactly
at an energy threshold, time exactly at a clock threshold, the other quantity
in proportion via `emob_core::apportion`. One period of 30 kWh, three of ten, or
ninety-six quarter hours give the same total:

```rust
assert_eq!(rate(&tiered, &session).exact_total().amount(), dec("15.70"));

// One period 21:00→23:00 under "0.30 from 22:00" is the same money as two cut at 22:00.
assert_eq!(rate(&night, &coarse).exact_total(), rate(&night, &fine).exact_total());
```

**Average power is the exception.** It is not cumulative, and a period carries
no information about the power inside it, so the total is a function of the
resolution the periods carry. `RatingNote::PowerJudgedPerPeriod` explains why a
partner's coarser document differs:

```rust
// 60 kWh in one hour, under "below 50 kW at 0.30, otherwise 0.60".
rate(&t, &one_period).total()    // 36.00 EUR — one window, averaging 60 kW
rate(&t, &two_halves).total()    // 34.50 EUR — 55 kWh then 5, averaging 110 and 10
```

### Charging, parked and withheld time

`Period::activity` is a stated fact (from the session history), not inferred
from zero energy — a full battery can deliver `0.000 kWh` while still charging.
OCPI 2.3.0 defines `PARKING_TIME` as time the vehicle is not requesting power
`[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`:

| `emob_core::Activity` | priced as | energy transferred |
|---|---|---|
| `Charging` | `TIME` | ✅ |
| `Parked` — the vehicle stopped asking | `PARKING_TIME` | |
| `Withheld` — the point stopped offering | *nothing* | |

`Withheld` (OCPP `SuspendedEVSE`: a zero charging profile, an `[EnWG §14a]`
dimming, a fault) is priced by neither dimension, and
`RatingNote::WithheldNotPriced` reports the seconds:

```rust
// Same duration, energy and tariff. One driver left a full car on the post;
// the other was curtailed by the operator.
assert_eq!(full_battery.total().to_string(), "12.00 EUR");
assert_eq!(curtailed.total().to_string(),   "10.00 EUR");
```

### Durations below the clock's resolution

Measured values below the shortest resolvable span may not be billed
`[REA 6-A §3.1]`. `Chargeable` carries the clock resolution (60 s unless the
station's type approval states better); a time dimension whose whole span falls
below it is dropped with `RatingNote::DurationBelowResolution`:

```rust
let rated = rate(&occupancy_tariff, &session);        // 30 s of occupancy, 60 s clock
assert_eq!(rated.amount_for(Dimension::ParkingTime), None);
assert!(rated.amount_for(Dimension::Energy).is_some());

let rated = rate(&occupancy_tariff, &session.with_clock(ten_second_clock));
assert_eq!(rated.amount_for(Dimension::ParkingTime), Some(dec("0.05")));
```

### `step_size`: once per session

`step_size` rounds a dimension up to a block (500 Wh, fifteen minutes). OCPI
applies it once per session for `ENERGY` and once for `TIME` and `PARKING_TIME`
combined, with the block and price of the last relevant component
`[OCPI 2.3.0 §mod_cdrs_step_size]`. Rounded per price instead, the
specification's own tiered example costs €1.31 instead of €1.18. The quantity is
rounded to `APPORTIONED_SCALE` before the ceiling, so two slicings of one
session never differ by a block. Each rounding produces a
`RatingNote::RoundedToBlock`, in base units (kWh, whole seconds). OCPI 3.0
removes `step_size`.

### Exact arithmetic

Time accumulates in whole seconds and divides last: 35 minutes at €6.00 an hour
is `6.00 × 2100 / 3600 = 3.50`, not `3.4999…`. A `Line` therefore carries both
`unit_price` (per hour, as written) and `base_quantity` (seconds):

```rust
assert!(rated.lines_sum_to_total());
assert!(rated.lines_reconcile());   // base_quantity × unit_price / 3600 == amount
```

`Rated::accounted_quantity_for` is what a dimension was billed for, less the
`step_size` block, plus any quantity no element priced (OCPI's way of writing
"the first 10 kWh are free" `[OCPI 2.3.0 §Tariff]`). `emob_cdr::validate`
compares it with the record. Notes serialise, because they are what a settlement
dispute turns on.

## Restrictions and time zones

Time-of-day, date, energy, duration, power and weekday restrictions are read on
the wall clock of `Tariff::time_zone`, an IANA zone: a `22:00` night rate is
local civil time at the charge point
`[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, and OCPI carries the zone
on the Location `[OCPI 2.3.0 §mod_locations_location_object]`. Partner sessions
usually arrive in UTC, so an offset-based reading would misprice them:

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

- **Daylight-saving gaps and folds are cut**: in an autumn fold, a window ending
  at `02:30` ends twice.
- **A window that wraps midnight** (`22:00`–`06:00`) is a night tariff, not an
  empty range.
- **An `end_time` of `00:00` is the end of the day**, as the specification tells
  authors to write it.
- **An unevaluable restriction never matches.** `Restrictions::unevaluable`
  carries what a wire adapter parsed but this crate cannot judge (OCPI's
  `min_current`/`max_current`, partner extensions); the element is skipped with a
  note rather than applied under unchecked conditions.

## VAT: a breakdown, not a number

EN 16931 e-invoices — mandatory between German businesses from 2027
`[UStG §14]` — need the taxable amount per rate:

```rust
for line in rated.tax_summary() {
    println!("{} %: net {} + tax {} = {}", line.rate, line.net, line.tax, line.gross);
    // 7 %:  net 1.00 + tax 0.07 = 1.07
    // 19 %: net 42.02 + tax 7.98 = 50.00
}
assert_eq!(rated.net().amount() + rated.tax().amount(), rated.gross().amount());
```

`TaxIncluded::Yes` means component prices are gross; `No` means net.
`Tariff::vat_basis()` answers "what is this tariff's rate?" with three cases —
`VatBasis::Rate`, `Unstated` and `Mixed` — because a gross bound with no stated
rate is publishable while one with two rates has no single taxable amount.

## Price limits: two ceilings, not one

OCPI's `min_price` and `max_price` each have a before-tax and after-tax figure,
and "they both apply" `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]`. With mixed
VAT rates the two are not proportional; keeping only one lets a total reach
€11.05 gross under a published €11.00 maximum. `PriceLimit` carries both, and
OCPP 2.1's `Price` carries them as `exclTax` and `inclTax`.

- All four limbs bound one number. The minimum gives a floor, the maximum a
  ceiling, and the adjustment is the smallest movement inside both. If they
  contradict, `RatingNote::LimitsContradict` is raised and the maximum (what the
  driver was shown) wins.
- A maximum below zero is clamped at zero (`RatingNote::AdjustmentClampedAtZero`).
- An unreachable bound is named (`RatingNote::BoundUnreachable`); an unusable
  VAT rate (≤ −100 %) is named (`RatingNote::VatRateNotUsable`).
- A maximum larger than one VAT category is spread across several, largest
  first (`Rated::adjustment_parts`, `RatingNote::AdjustmentSpread`), because an
  EN 16931 allowance carries one category and a negative taxable amount is not
  fileable.
- The bound is decided on the exact total, not the per-rate rounded breakdown.
  `Rated::total` rounds once and is what a cap promises; `Rated::gross` rounds
  per category and can sit one minor unit above it.

## Tariff versions and fingerprints

A tariff id is a name, and names get reused. `Tariff::fingerprint()` is a
SHA-256 over a canonical encoding of everything that can change a price —
bounds, validity window, every element in order with its restrictions — so a
record names the exact tariff it was priced with:

```rust
assert!(cdr.was_priced_with(&tariff));   // not "does the id match"
```

Scale is part of the fingerprint (`0.49` ≠ `0.490` as a displayed price), enums
go in through declared tokens, and weekday sets go in sorted.

The price must be known *before* the session starts `[AFIR Art. 5(4)]`, so a
session is priced by the version in force at its start:

```rust
let cdr = CdrBuilder::from_session(&session, Direction::Import)?
    .rated_with_history(&history)?      // the version in force at the start
    .build()?;
```

`TariffHistory` refuses overlapping versions, reports gaps, uses half-open
`[from, until)` windows, and refuses a version boundary off the quarter-hour
grid `[PTB-A 50.7 §3.1.7.2]`, naming the *next* boundary. Publishing a version
to its readers before it takes effect is the job of the `tarifd` service,
which publishes the estate's tariffs (its push clients are still to come).

## Reservations

OCPI prices a reservation through a restriction (`RESERVATION`,
`RESERVATION_EXPIRES`) with `FLAT` and `TIME` components over a window that ends
before charging begins `[OCPI 2.3.0]`. `rate_reservation` rates it separately,
sharing the session's arithmetic, so reservation minutes and charging minutes do
not compete for one `TIME` dimension. From the specification's worked examples:

- An expired reservation is priced by the reservation elements only (tariff 18
  bills €9.00, not €9.50).
- On expiry both kinds apply, in list order.
- `min_price` and `max_price` do not apply — they bound a charging session.

A reversed window prices no minutes, still owes a `FLAT` fee, and carries
`RatingNote::ReservationWindowReversed`. The clock-resolution floor
`[REA 6-A §3.1]` does not apply: no meter measured the reservation.
`Restrictions::describe` puts "reservation" first so a display cannot read a
hold fee as a charging rate.

## Display: order, per-minute prices and the Gesamtpreis

**Order.** Below 50 kW AFIR prescribes the order: per kWh, per minute, per
session, then anything else. `Dimension` is declared in that order and derives
`Ord`, so sorting components complies with the article.

**Per minute.** OCPI stores time prices per hour; AFIR displays per minute. Some
hourly rates have no exact per-minute decimal:

```rust
assert_eq!(price_per_minute(dec("6.00")), Some(dec("0.10")));
assert_eq!(price_per_minute(dec("2.50")), None);   // 0.041666…
```

`check_afir` reports that as a breach (remedy: an hourly rate divisible by
three), and `emob-ocpp` refuses the OCPP 2.1 crossing.

**Tiers.** "All its price components" `[AFIR Art. 5(4)]` means one `Tier` per
element, with conditions in words:

```rust
assert_eq!(describe(&tariff, at).full_disclosure(), vec![
    "0.29 EUR / kWh (22:00–06:00)",
    "0.49 EUR / kWh",
]);
```

**Gesamtpreis.** German law requires consumers to be shown the tax-inclusive
total price `[PAngV §14(2)]`, `[PAngV §2 Nr. 1]`. A net tariff is grossed per
component, as `[PAngV §3(3)]` asks for a broken-out price:

```rust
let mut tariff = Tariff::simple(/* … */ vec![
    PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
]);
tariff.tax_included = TaxIncluded::No;

assert!(!describe(&tariff, at).is_gesamtpreis());   // what a settlement partner reads
assert_eq!(describe_gross(&tariff, at)?.one_line(), "0.5831 EUR / kWh");
```

## Conformance: AFIR and PAngV checks

At 50 kW and above, the ad-hoc price "shall be based on the price per kWh", with
a per-minute occupancy fee permitted in addition `[AFIR Art. 5(4)]`. The lawful
component sets on a fast charger are `{kWh}` and `{kWh, occupancy}`; a session
fee is named only for points below 50 kW. Germany's `[PAngV §14(2)]` has
required an energy price per kWh `[PAngV §14(4)]` at any power since 28.05.2022.

```rust
let by_the_minute = Tariff::simple(/* … */ vec![
    PriceComponent::new(Dimension::Time, dec("6.00")),   // 0.10 a minute
]);
assert!(check_afir(&by_the_minute, dec("22")).is_lawful());    // fine on a 22 kW post
assert!(!check_afir(&by_the_minute, dec("150")).is_lawful());  // unlawful at 150 kW
assert!(!check_pangv(&by_the_minute).is_lawful());             // unlawful in Germany at any power

let with_session_fee = Tariff::simple(/* … */ vec![
    PriceComponent::new(Dimension::Energy, dec("0.49")),
    PriceComponent::new(Dimension::Flat, dec("0.50")),
]);
assert!(check_afir(&with_session_fee, dec("22")).is_lawful());
assert!(!check_afir(&with_session_fee, dec("150")).is_lawful());
```

`check_afir` also reports: an energy price plus a charge for *charging* time
(the article permits occupancy, not transfer dressed up as time); a minimum above
the maximum; an element shadowed by an unrestricted one; a VAT rate of −100 %
(no taxable amount `[UStG §14]`); a per-minute price with no exact decimal; and,
for ad-hoc tariffs, an unevaluable condition. `check_pangv` also refuses a net
price list with no stated VAT rate. They are two functions because their
objections go to different authorities. Contract tariffs fall under
`[AFIR Art. 5(5)]`, assessed in `emob-core` against a `ProviderProfile`.

## API at a glance

| Item | Purpose |
|---|---|
| `Tariff`, `TariffElement`, `PriceComponent`, `Restrictions`, `PriceLimit` | The tariff model |
| `Dimension`, `TariffKind`, `TaxIncluded`, `VatBasis` | Its vocabulary |
| `Chargeable`, `Period` | A session as rating input |
| `rate`, `rate_reservation`, `matching_component` | Rating |
| `Rated`, `Line`, `Adjustment`, `TaxLine`, `RatingNote` | The result, fully explained |
| `describe`, `describe_gross`, `PriceDescription`, `Tier`, `price_per_minute` | What the driver is shown |
| `check_afir`, `check_pangv`, `Conformance` | Shape checks against the law |
| `TariffHistory`, `TariffFingerprint` | Versions over time |

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `serde` | yes | `Serialize`/`Deserialize` for every public type |

## Standards and sources

AFIR (EU 2023/1804) Art. 5(4), 5(5), 20(2); PAngV §§ 2, 3, 14; OCPI 2.3.0
tariffs and CDRs; OCPP 2.1 Tariff and Cost; EN 16931; UStG § 14; REA 6-A;
PTB-A 50.7.

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. See
[tariffs and price transparency](https://hupe1980.github.io/emob/docs/pricing/)
on the documentation site and the
[root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
