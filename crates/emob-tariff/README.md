# emob-tariff

EV charging tariffs where the price shown to the driver is *derived from* the
tariff that rates the session — so the two cannot drift — with components
ordered as AFIR prescribes and a conformance check that knows a per-minute-only
tariff is unlawful above 50 kW.

```console
cargo add emob-tariff
```

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
let rated = rate(&tariff, &Chargeable::energy_only(kwh("29.500"), at));
assert_eq!(rated.total().to_string(), "14.96 EUR");
```

A test walks every dimension and asserts that each displayed price equals the
price that rated it.

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

## A tariff can be unlawful on its own

At 50 kW and above, `[AFIR Art. 5(4)]` says the ad-hoc price "shall be based on
the price per kWh", with an occupancy fee per minute permitted **in addition**.

```rust
let by_the_minute = Tariff::simple(/* … */ vec![
    PriceComponent::new(Dimension::Time, dec("0.10")),
]);

assert!(check_afir(&by_the_minute, dec("22")).is_lawful());   // fine on a 22 kW post
assert!(!check_afir(&by_the_minute, dec("150")).is_lawful()); // unlawful beside it
```

`check_afir` also catches the subtler shape: an energy price *plus* a charge for
**charging time** passes the first test and still breaches, because the article
permits a fee for occupancy — sitting there once charging has finished — not a
rate for the transfer dressed up as time.

Contract tariffs are not judged by this rule. Art. 5(4) regulates the ad-hoc
price; a provider's own contract price falls under Art. 5(5), which is a
disclosure duty rather than a shape duty.

## Every term of the total is a line

```rust
let rated = rate(&tariff, &session);
assert!(rated.lines_sum_to_total());
```

One `Line` per component that applied, with its quantity, unit price and
amount. The total is their sum and nothing else — unless a minimum or maximum
moved it, and then `lines_sum_to_total()` returns `false` and a `RatingNote`
says which. There is no term in the total that is not a line or a note.

Rounding up to a `step_size` block produces a note too, because it is always
against the customer — and OCPI 3.0 removes the field, advising `step_size = 1`,
so a tariff relying on it is one that will have to change.

## Exactness

Rounding happens **once**, at `total()`. Every line is computed and kept exact,
because rounding per line and then summing gives a different answer, and which
is correct is a tax question rather than an arithmetic one.

Seconds become hours by exact decimal division, not by `seconds as f64 / 3600.0`.

## Restrictions, including the one everybody gets wrong

Time-of-day, energy, duration and weekday restrictions select which element
applies. A window that wraps midnight — `22:00` to `06:00` — is handled as the
night tariff it is, rather than as the empty range a naïve comparison makes of
it.

`describe()` takes the instant too, so a display can answer *what will this cost
at 22:00* and not only *what does it cost now*. And when a tariff has more than
one element, `varies_by_condition` is set — because showing a single set of
numbers for a time-varying tariff is how a driver arriving at 21:58 is quoted
the day rate for a session billed at the night one.

## License

MIT OR Apache-2.0.
