# emob-poi

The charge point register and its national access point feed, for charge point
operators: a DATEX II AFIR Recharging publication generated from the same tariff
that prices the session.

[![crates.io](https://img.shields.io/crates/v/emob-poi.svg)](https://crates.io/crates/emob-poi) [![docs.rs](https://img.shields.io/docsrs/emob-poi)](https://docs.rs/emob-poi) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-poi.svg)](#license)

`[AFIR Art. 5(2)]` makes the price shown before a session the price that may be
charged, and `[AFIR Art. 20(2)(c)]` requires operators to publish that ad-hoc
price, free of charge, through the national access point — in Germany the
Mobilithek, in the DATEX II Recharging profile, from 14 April 2026
`[DATEX-II-Profil]`. Most stacks compute that number twice, once for billing and
once for the feed, and nobody reconciles the feed against an invoice. This crate
has no price model of its own: it publishes `emob_tariff::Tariff`, the value
`emob_tariff::rate` charges with, in exact decimal.

```console
cargo add emob-poi
```

## Example

```rust
use emob_poi::datex::{InformationStatus, Publisher};
use emob_poi::feed::Feed;
use emob_poi::rate;
use emob_poi::site::*;
use emob_core::{EvseId, PartyId, TimeZone};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use rust_decimal::Decimal;

let tariff = Tariff::simple(
    "ad-hoc".parse()?,
    emob_core::Currency::EUR,
    TariffKind::AdHoc,
    TimeZone::new("Europe/Berlin")?,
    vec![PriceComponent::new(Dimension::Energy, Decimal::from_str_exact("0.49")?)],
);
let (published_rate, notes) = rate::publish(&tariff, "rate-1");
assert!(notes.is_empty());

let point = ChargingPoint::new(
    Facility::new("point-1"),
    EvseId::parse("DE*ABC*E00001")?,
    Connector::new(ConnectorType::Iec62196T2Combo, Decimal::from(150)),
);
let site = Site::new(
    Facility::new("site-1"),
    "Musterstadt Nord",
    Coordinates {
        latitude: Decimal::from_str_exact("50.779599")?,
        longitude: Decimal::from_str_exact("6.104507")?,
    },
    Address::default(),
    TimeZone::new("Europe/Berlin")?,
    vec![Station::new(Facility::new("station-1"), PartyId::new("DE", "ABC")?, vec![point])],
);

let feed = Feed {
    publisher: Publisher {
        country: "DE".to_owned(),
        national_identifier: "DE-NAP-Example".to_owned(),
        language: "de".to_owned(),
    },
    information_status: InformationStatus::Test,
    table: Facility::new("table-1"),
    table_name: Some("Region Nord".to_owned()),
    sites: vec![site],
    rate_for: &|_| Some(published_rate.clone()),
};

let json = feed.table(time::macros::datetime!(2026-04-14 00:00 UTC))?.to_json()?;
assert!(json.contains(r#""value": 0.49"#));              // the tariff's own decimal
assert!(json.contains(r#""maxPowerAtSocket": 150000"#)); // watts, as the profile asks
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What it guarantees

- **The published price is the billed price**, carried as an exact decimal into
  the JSON number (`0.49`, not `0.49000000000000000222`).
- **A status the register contradicts cannot be built.** A decommissioned point
  cannot be reported `available`.
- **Status references resolve.** A status update that points at a table version
  the access point does not hold is an error, not a silently dropped update.
- **What the profile cannot express is reported** as a `RateNote`, never
  papered over.
- **Every emitted JSON path exists in the Mobilithek's reference instance**
  (`tests/the_profile.rs`).
- **No I/O, no clock.** The publication time is an argument, so an export
  replayed two years later produces the same bytes.

## Where the DATEX II profile falls short

`[DATEX-II-Profil Tab. A.116]` offers six price types: `basePrice`, `flatRate`,
`free`, `other`, `pricePerKWh`, `pricePerMinute`. Each gap is published as
truthfully as the profile allows and reported with a `RateNote`:

| Gap | What is published | `RateNote` |
|---|---|---|
| No per-hour type; €2.50/h has no exact per-minute decimal | hourly figure under `other`, unit in `additionalInformation` (rounding would be a price not charged `[PAngV]`) | `HourlyPriceHasNoExactMinute` |
| No occupancy-fee type, though `[AFIR Art. 5(4)]` permits one at 50 kW and above | `other` with a sentence; `combinationWithParkingFee` is only a boolean | `OccupancyFeeHasNoPriceType` |
| `minimumDeliveryFee` / `maximumDeliveryFee` carry no tax flag | the fee in the tariff's own basis | `DeliveryFeeHasNoTaxBasis` |
| `FacilityLocation.timeZone` is a fixed UTC offset, wrong half the year for German sites | the offset in force at publication | `DailyWindowHasOnlyAnOffset` |
| No reservation price — the profile covers delivered energy | the reservation element is omitted | `ReservationPriceNotPublished` |
| No power or weekday applicability | the price without its condition | `RestrictionNotPublished` |
| kWh and minute bounds are whole numbers | the band narrowed inward; a band under one unit omitted | `BoundNarrowedToWholeUnits`, `BandTooNarrowToPublish` |

## Connectors and the AFIR interface duty

A point's connectors are both published (`[DATEX-II-Profil Tab. A.88]`) and a
legal duty: `[AFIR Anh. II 1.1]` and `[AFIR Anh. II 1.2]` require at least
Type 2 on AC and CCS Combo 2 on DC `[AFIR Art. 21(1)]`, and the regulator may
forbid operating a point that breaches it `[LSV26 §5(3)]`. `ConnectorType` is
`emob-core`'s, and `ChargingPoint::profile` builds the compliance profile from
the inventory:

```rust
let profile = point.profile(commissioned_on, Accessibility::Public);
// A CHAdeMO-only DC post: ordinary hardware, and a closable breach.
assert_eq!(
    assess(&profile, on).status_of(ObligationId::AfirAnnexIiConnector),
    Some(Status::Failing),
);
```

Notice dates, metering posture and ownership are not guessed: they stay at
`ChargePointProfile::bare`'s defaults for the caller to state.

## Lifecycle, status and the register

`[LSV26 §4(1)]` makes commissioning, decommissioning and a change of operator
notifiable, and `[DATEX-II-Profil Tab. B.45]` has literals for two of them
(`planned`, `removed`). Decommissioned points published as `available` for months
are a common defect in European charging data, and a schema validator cannot see
it:

```rust
point.lifecycle = Lifecycle::Decommissioned;
point.report(PointStatus::Available)      // Err(StatusContradictsRegister)
point.report(PointStatus::Removed)        // Ok
```

`ChargingPoint::report` is the only way to build a status, and it reads the
lifecycle off the point rather than taking it as an argument.

## Table and status publications

A status message carries only versioned references (`targetClass`, `idG`,
`versionG`) into a separately sent table publication. A reference that does not
resolve is silently dropped by the consumer, and the point vanishes from every
map. `Feed` builds both publications from one inventory; `Feed::status` also
checks each update against the table the access point accepted, and `feed::check`
serves deployments whose table comes from another system.

## Other checks

- **Station power is bounded by its points.** `totalMaximumPower` must lie
  between the largest point and the sum of all points; load management makes
  anything in between legitimate.
- **`iso15118` in this profile means ISO 15118-20** `[DATEX-II-Profil Tab. A.130]`.
  A point speaking only ISO 15118-2 or DIN SPEC 70121 is published as `other`,
  so the feed does not claim readiness for the duty `[DA-656 Anh. 2.1.3]` phases
  in from 2027.

## API at a glance

| Item | Purpose |
|---|---|
| `Site`, `Station`, `ChargingPoint`, `Connector`, `Facility` | The register |
| `ChargingPoint::profile` | Compliance profile for `emob_core::obligation` |
| `ChargingPoint::report`, `Lifecycle`, `PointStatus`, `Report` | Register-consistent status |
| `rate::publish`, `Rate`, `RateNote` | Tariff → DATEX II energy rate |
| `Feed::table`, `Feed::status`, `feed::check` | Table and status publications |
| `datex` | The DATEX II publication types and JSON |

## Feature flags

None. Serialisation is always enabled, since the crate's output is JSON.

## Standards and sources

AFIR (EU 2023/1804) Art. 5, 20, 21 and Annex II; Delegated Regulation (EU)
2025/656; the German DATEX II AFIR Recharging profile and the Mobilithek
reference instances; LSV 2026; PAngV.

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. The `poid` service builds its inventory
from the estate's stations; pushing to the Mobilithek is still to come. See
[locations and the national access point](https://hupe1980.github.io/emob/docs/locations/)
on the documentation site and the [root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
