# emob-ocpp

The OCPP seam for CSMS developers: turns a charging station's transaction events
into a billable session, and carries tariffs and § 14a grid limits back to the
station.

[![crates.io](https://img.shields.io/crates/v/emob-ocpp.svg)](https://crates.io/crates/emob-ocpp) [![docs.rs](https://img.shields.io/docsrs/emob-ocpp)](https://docs.rs/emob-ocpp) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-ocpp.svg)](#license)

OCPP carries two kinds of meter value, and only one is money. The numeric ones
(`meterStart`, `meterStop`, `SampledValue.value`) are operational telemetry; the
signed one — an OCMF record inside a `SignedMeterValueType` — is the only thing
that becomes a billed kilowatt-hour. This crate makes that structural: its input
vocabulary has **no numeric meter value at all**, so there is no path from
telemetry to a CDR. In the other direction it puts the tariff that rates the
session onto the station's display via OCPP 2.1, and a grid operator's limit
onto the station as a charging profile.

```console
cargo add emob-ocpp
```

## Example

```rust
use emob_ocpp::{SignedMeterValue, SignedReading, Transaction, TransactionEvent};
use emob_session::{Authorization, EndReason};
use emob_core::Direction;

let transaction = Transaction::new(
    "t-96".parse()?,
    "DE*SIM*E00001".parse()?,
    Authorization::ad_hoc(),
)
.with(TransactionEvent::started(started, vec![]))
.with(TransactionEvent::ended(
    ended,
    vec![SignedReading::new(
        SignedMeterValue::new(raw_ocmf),
        Some("Transaction.End".to_owned()),
    )],
    EndReason::Local,
));

let assembled = transaction.assemble(Direction::Import)?;
assert_eq!(
    assembled.session.total(Direction::Import).unwrap().to_string(),
    "0.636 kWh",
);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What it guarantees

- **No telemetry number reaches an invoice.** The Open Charge Alliance's own
  example sends `meterStop` = **108814** (the meter's lifetime register, in Wh)
  for a transaction whose signed difference is **0.636 kWh** `[OCA SMV §5.2]`.
  Only the signed value is lifted.
- **A transport retry is not a reading.** Records are de-duplicated by the digest
  of their signed bytes, so a retried `MeterValues` does not look like a missing
  record. Records that differ under one pagination counter are both kept — that
  is a fault the chain must see. `Assembled::duplicates_dropped` counts retries.
- **The price on the screen is the price on the invoice.** The OCPP 2.1 tariff is
  built from the same `emob_tariff::Tariff` that rates the CDR.
- **What the wire cannot carry exactly is refused or accounted for**, never
  silently rounded.
- **The money crates stay protocol-free.** `emob-core`, `emob-session`,
  `emob-eichrecht`, `emob-tariff` and `emob-cdr` build with no OCPP in their
  dependency tree; this crate is the only one on both sides.
- **No I/O, no clock.** `just purity` fails the build if that changes.

## Inbound: from transaction events to a session

`ocpp-kit` decodes the protocol — OCPP 1.6 nests the signed record as a string
holding JSON holding Base64 `[OCA SMV §3.2.1]`, and the `publicKey` envelope has
two spellings `[OCA SMV §3.2.2]` — and delivers a version-neutral
`csms::events::DomainEvent`. `emob_ocpp::kit` maps that event onto a
`TransactionEvent`, and `Transaction::assemble` builds the session:

| Fact | Source | Why not the other |
|---|---|---|
| The register, and its scale | the **signed record** | OCPP's numbers are telemetry |
| When the transaction opened and closed | the **OCPP events** | OCMF's clock is often only informative, and the CSMS knows when it authorised |
| Charging, or merely connected | the **OCPP events** | the meter cannot tell a taper from occupancy, and `[AFIR Art. 5(4)]` prices them differently |
| Whether a reading is clock-aligned | the **OCPP `ReadingContext`** | nothing in the record says why it was taken |

Where the signed record has its own opinion — OCMF's `S` (suspended) marker
`[OCMF Tab. 7, TX]` — disagreements with the OCPP events are reported in
`Assembled::charging_disagreements` for an operator, not silently resolved.

The public key a station sends is only a claim; the key that decides anything
comes from `emob_eichrecht::KeyRegistry`. This crate does not verify:
`emob_eichrecht::Evidence::assemble` does.

## Outbound: the tariff on the station's display

AFIR requires the ad-hoc price to be known *before* the driver starts
`[AFIR Art. 5(4)]`, and the station display is where that happens. OCPP 2.1's
*Tariff and Cost* block makes it structured:

```rust
let crossing = emob_ocpp::to_ocpp(&tariff, at)?;          // the same Tariff that rates
let request = v2_1::SetDefaultTariffRequest::new(0, crossing.value);
for note in crossing.reasons() { /* what the wire could not carry exactly */ }
```

OCPI orders elements and picks the first match per dimension; OCPP 2.1 orders
prices within each dimension and picks the first match. Projecting one onto the
other is exactly `emob_tariff::matching_component`, so the station selects the
component the invoice is built from. The required `description` is
`describe().full_disclosure()`, and `cost_updated` turns a `Rated` into the
running total `CostUpdated` carries.

A loss in the driver's disfavour is a **refusal**:

| Refused | Why |
|---|---|
| a time price with no exact per-minute spelling | OCPP's field is `priceMinute`; €2.50/h is €0.041666…/min. Quote an hourly rate divisible by three |
| a dimension charged at two VAT rates | OCPP carries one `taxRates` list per dimension |
| a session fee conditioned on a quantity | `TariffConditionsFixed` carries only the wall clock, so the fee would be charged on every session |
| an unevaluable restriction | the station could not check the condition |

What only affects *display* is carried with a note in the `Crossing`: a
`step_size` or `valid_until` OCPP has no field for, the residual when a gross
price is quoted net, and tiers beyond the ten `description` lines OCPP allows.
The finished document is then checked by `ocpp-kit`'s own schema validator.

## Outbound: § 14a grid limits as charging profiles

`to_charging_profiles` carries an `emob_smart::Allocation` — what a site may draw
under a distribution grid operator's setpoint (§ 14a EnWG, German controllable
loads) — to its stations at the purpose `ChargingStationExternalConstraints`.
The purpose is fixed: OCPP resolves overlapping profiles by purpose first, and a
`TxDefaultProfile` would sit in the same tier as the operator's own optimiser,
which the grid operator outranks `[BK6-22-300 Ziff. 4.6]`.

Three facts have no OCPP field and are reported through the `Crossing`: the site
ceiling (OCPP addresses EVSEs), the headroom (curtailed vs. nobody plugged in),
and whether full power is granted or the grid operator has gone silent.

## API at a glance

| Item | Purpose |
|---|---|
| `Transaction`, `TransactionEvent`, `SignedReading` | Inbound events, signed values only |
| `Transaction::assemble`, `Assembled` | Session + records, duplicates dropped, disagreements reported |
| `kit::event_from` and helpers | Map `ocpp-kit`'s `DomainEvent` onto this vocabulary |
| `record_of` | Extract the OCMF record from a signed value |
| `to_ocpp`, `cost_updated` | Tariff and running cost onto OCPP 2.1 |
| `to_charging_profiles`, `Setpoint` | § 14a allocation onto charging profiles |
| `SignedMeterValue` | Re-exported from `ocpp-kit` |

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `serde` | yes | `Serialize`/`Deserialize` for every public type |

OCPP 1.6J, 2.0.1 and 2.1 support comes from
[`ocpp-kit`](https://github.com/hupe1980/ocpp-kit) 0.5 and is always enabled.

## Standards and sources

OCPP 1.6J, 2.0.1, 2.1 (Tariff and Cost, smart charging); OCA Signed Meter Values
whitepaper; OCMF; AFIR Art. 5(4); BNetzA BK6-22-300 (§ 14a EnWG).

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. This crate is the library under the `csmsd`
daemon. See [the OCPP seam](https://hupe1980.github.io/emob/docs/ocpp/) on the
documentation site and the [root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
