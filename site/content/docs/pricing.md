+++
title = "Tariffs and price transparency"
weight = 4
description = "Why the price a driver is shown and the price they are charged come from one object, why AFIR prescribes the order they are listed in, and why a per-minute-only tariff is unlawful above 50 kW."
+++

# Tariffs and price transparency ✅

`[AFIR Art. 5(4)]` regulates the ad-hoc price at a public charge point more
tightly than most implementations realise. Three rules, each a checkable
property of a tariff rather than an opinion about one.

## The price shown is the price charged

A charging tariff has two jobs that platforms normally implement twice: it
**rates** a finished session, and it is **displayed** before the session starts.
When those come from two places they drift — the screen reads a field somebody
typed into a CMS, the invoice reads the tariff engine, and one of them was
updated. That is the price-transparency breach the article exists to prevent,
and it is almost never malice.

Here `describe()` and `rate()` read the same `PriceComponent` values off the
same `Tariff`:

```rust
let tariff = Tariff::simple(id, Currency::EUR, TariffKind::AdHoc, vec![
    PriceComponent::new(Dimension::Flat, dec("0.50")),
    PriceComponent::new(Dimension::Energy, dec("0.49")),
]);

// What the driver sees, before anything happens.
assert_eq!(describe(&tariff, at).one_line(), "0.49 EUR / kWh · 0.50 EUR / session");

// What the driver pays, from the same numbers.
assert_eq!(rate(&tariff, &session).total().to_string(), "14.96 EUR");
```

Neither can quote a number the other does not use. A test walks every dimension
and asserts that each displayed price equals the price that rated it.

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

Contract tariffs are not judged by this rule. Art. 5(4) governs the ad-hoc
price; a provider's own contract price falls under Art. 5(5), which is a
disclosure duty rather than a shape duty, and binds the provider rather than the
point.

## Every term of the total is a line

```rust
assert!(rated.lines_sum_to_total());
```

One line per component that applied, with its quantity, unit price and amount.
The total is their sum and nothing else — unless a minimum or maximum moved it,
and then `lines_sum_to_total()` is `false` and a note says which. There is no
term in the total that is not a line or a note.

Rounding a quantity up to a `step_size` block produces a note as well, because
it is always against the customer. OCPI 3.0 removes the field and advises
setting it to 1, so a tariff relying on it is one that will have to change, and
`check_afir` says so without calling it unlawful.

## Rounding happens once

Every line is computed exactly and kept exact; only the total rounds, to the
currency's minor unit, half away from zero. Rounding per line and then summing
gives a different answer, and which of the two is correct is a tax question
rather than an arithmetic one — so the exact figures survive and the caller can
do either.

Seconds become hours by exact decimal division, never `seconds as f64 / 3600.0`.

## The restriction everybody gets wrong

A tariff window that wraps midnight — `22:00` to `06:00` — is a night tariff,
not the empty range a naïve `start <= t && t < end` makes of it.

```rust
assert_eq!(rate(&t, &at_23_00).lines[0].unit_price, night);
assert_eq!(rate(&t, &at_03_00).lines[0].unit_price, night);
assert_eq!(rate(&t, &at_noon).lines[0].unit_price, day);
```

`describe()` takes the instant too, so a display can answer *what will this cost
at 22:00* rather than only *what does it cost now*. And when a tariff has more
than one element, `varies_by_condition` is set — because showing one set of
numbers for a time-varying tariff is how a driver arriving at 21:58 is quoted
the day rate for a session billed at the night one.

## What is not here yet 📐

VAT is carried on each component and not yet applied — that belongs with
`emob-billing`, where the EN 16931 invoice knows the regime. Reservation and
booking restrictions are OCPI concepts this crate does not model, and a tariff
carrying them is not silently treated as unrestricted.
