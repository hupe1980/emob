# emob-thg

The German greenhouse-gas quota (*THG-Quote*) for EV charging as executable law: eligibility, quantities and notifications under the 38. BImSchV, built only from energy a calibrated meter signed.

[![crates.io](https://img.shields.io/crates/v/emob-thg.svg)](https://crates.io/crates/emob-thg) [![docs.rs](https://img.shields.io/docsrs/emob-thg)](https://docs.rs/emob-thg) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-thg.svg)](#license)

Every kilowatt-hour a public charge point delivers is worth money twice: once
from the driver, and once from a fuel supplier that must cut the emissions of
what it sells. That second payment is the THG-Quote, settled from a
notification filed per obligation year. Failing its conditions is silent — the
point charges, the sessions bill, and the quota money never arrives — so this
crate turns each condition into a refusal that names the remedy:

```text
DEABCE00042 is not eligible: publish the register entry or consent to its
publication, sign the conformity declaration the authority provides, and obtain
an operator identification code — or forgo the quota
```

```console
cargo add emob-thg
```

## Example

A public-charge-point notification (`[38k §6]`) for 2026:

```rust
use emob_thg::{Attribution, ClaimBuilder, DriveEfficiency, EmissionsBasis};

// The grid emissions value is announced each year in the Bundesanzeiger;
// it is an argument, carried with the notice it came from.
let basis = EmissionsBasis::grid_average(dec("96"), "BAnz AT 31.10.2025 B5")?;
let mut claim = ClaimBuilder::new(2026, Attribution::own("AB7"), basis,
                                  DriveEfficiency::BatteryElectric)?;

// One line per charge point: its profile, its address, and the CDR ledger.
claim.point(&profile, "Musterstraße 1, 10115 Berlin", &ledger)?;

let filed = claim.build()?;
filed.value.megawatt_hours();      // 0.063333 — exact, to the watt-hour
filed.value.emissions_kg_co2e();   // 26.26546176
filed.value.deadline();            // 2027-02-28
for reason in filed.reasons() { println!("{reason}"); }
```

## What it guarantees

- **All four eligibility conditions of `[38k §6(3)]` are checked**, and a point
  failing any is refused with `ThgError::NotPublic` or `ThgError::NotEligible`:
  1. the regulator has **published** the point, or the operator consented to
     publication `[38k §6(3) Nr. 1]` — not merely notified it, which is a
     different duty `[LSV26 §4(1)]` (see below);
  2. the quantity is determined lawfully under measuring and calibration law
     `[38k §6(3) Nr. 2]`;
  3. the operator holds an identification code from an ID registration
     organisation `[AFIR Art. 20(1)]`;
  4. any further identifying features the authority has announced are present —
     a three-state answer, because "none announced" and "announced but missing"
     are opposite outcomes.
- **Only signed, verified energy counts.** A record with no evidence, or with
  evidence that failed verification (`energy_billable: false`), is refused as
  `ThgError::Unmeasured`. A notification including either would be contradicted
  by the filer's own declaration `[38k §6(4)]`, which is kept for three years.
- **Corrections count once.** The ledger is read through `CdrLedger::live`.
- **V2G discharges are excluded**, with a note: `[38k §5(1)]` counts electricity
  *withdrawn* for the vehicle.
- **A point is notified once per year.** `[38k §8(1) S. 3]` allows one
  notification per charge point per obligation year, so adding a point twice is
  a refusal, not a silent replacement.
- **Exact decimals throughout.** The only conversion is × 3.6 MJ/kWh, which is
  exact, so two runs of one year agree to the last digit.
- **No clock.** The year and the emissions announcement are arguments, and
  eligibility is judged on 31 December of the filed year, so a notification
  rebuilt three years later is the same file.

## How it works

### Notified is not published

`[38k §6(3) Nr. 1]` asks whether the point was **published** on the register (or
the operator consented). `[LSV26 §4(1)]` requires the notification (*Anzeige*)
and says nothing about publication, so a duly notified point can still be
ineligible. Reading eligibility off the notification date passes every point in
a compliant estate.

```rust
point.registration = Registration::notified_on(date!(2025 - 03 - 10));
point.quota.publication = RegisterPublication::Withheld;      // not eligible
point.quota.publication = RegisterPublication::ConsentGiven;  // eligible
```

### Energy belongs to the year it was withdrawn in

`[38k §5(1)]` counts electricity withdrawn **in** the obligation year. A session
from 23:45 on 31 December to 00:15 on 1 January is split across both years using
the quarter-hour periods `emob_session::split` produced, which conserve exactly.
The record carries a note when it contributes to two years, so a reconciliation
against billing explains the difference.

```rust
// One 30 kWh session across midnight, filed twice and counted once.
assert_eq!(filed(2026).megawatt_hours(), dec("0.015"));
assert_eq!(filed(2027).megawatt_hours(), dec("0.015"));
```

Settlement windows are half-open, taking each quarter hour's **start**
`[38k §6(1) S. 2 Nr. 5]`. German metrology labels the last quarter hour of the
year `2027-01-01T00:00` `[PTB-A 50.7 §3.1.7.2]`; reading that label as the
withdrawal day would move it into the next year.

### The formula and its factors

`[38k §5(3)]` multiplies the energy by a counting factor (3, stepping down in
2035 and 2036), the average emissions of German electricity, and Anlage 3's
drive-efficiency factor (`0.4` for battery-electric). The emissions value is
announced each year by 31 October for the following year `[38k §5(4)]`, so it is
an `EmissionsBasis` argument with its announcement reference, never a constant.

**Renewable basis.** `[38k §5(5)]` allows a renewable source's value instead of
the grid average only when the electricity comes directly from a plant behind
the same grid connection point **and** the metering point operator's
quarter-hourly measurements prove simultaneous consumption.
`EmissionsBasis::renewable` returns the missing condition by name; the remedy is
`EmissionsBasis::grid_average`. Wind and solar count now; biomass, landfill gas,
sewage gas and biogas only from 2028:

```rust
RenewableSource::Wind.countable_from();     // 2024
RenewableSource::Biomass.countable_from();  // 2028
```

### Two routes, two deadlines

| | `[38k §6]` public charge points | `[38k §7]` estimated per vehicle |
|---|---|---|
| claimant (*Ladepunktbetreiber*) | operator of the point | person the vehicle is registered to |
| quantity | measured value conforming to calibration law | published estimate (*Schätzwert*), once per vehicle |
| evidence | signed meter record | registration certificate (Zulassungsbescheinigung Teil I) |
| deadline `[38k §8(1) S. 1]` | 28 February of the **following** year | 15 November **inside** the obligation year |
| counting factor | `[38k §5(3)]` | `[38k §7(6)]` — M3/N3 reach 4 |
| builder | `ClaimBuilder` | `EstimateClaimBuilder` |

```rust
Route::PublicChargePoints.deadline(2026)   // 2027-02-28
Route::EstimatedPerVehicle.deadline(2026)  // 2026-11-15
```

There is no late filing; the deadline is a `Date` for the filing service to
compare with today.

`[38k §7(6)]` gives buses and heavy goods vehicles (classes M3 and N3) a factor
of 4 from 2027, stepping down to 3.5 (2035), 3 (2036), 2.5 (2037), 2 (2038),
1.5 (2039) and 1 (2040). That matters to depot operators, whose non-public posts
§ 6 refuses but whose registered buses § 7 counts. A bus counted in 2026 gets
§ 5(3)'s factor and the notification says so; a mixed fleet is counted at two
factors in one notification; and each vehicle is counted once per year
`[38k §7(4) S. 2]` by an identifier unique within the filer's records — never a
registration plate.

### Where it stops

At the figure the Verordnung defines. What an operator *sells* depends on the
reference value in § 37a BImSchG over a fuel supplier's whole balance — the
authority's arithmetic, not a fact about a charge point — so this crate produces
no price.

## API at a glance

| Item | Purpose |
|---|---|
| `ClaimBuilder`, `Claim`, `PointRecord` | § 6 notification for public charge points |
| `EstimateClaimBuilder`, `EstimateClaim`, `Estimate`, `RegistrationEvidence` | § 7 notification per vehicle |
| `Attribution` | own filing, or a third party designated by operators/keepers |
| `EmissionsBasis`, `RenewableSource`, `DirectSupply` | grid-average or renewable basis, with its announcement |
| `DriveEfficiency`, `VehicleClass`, `counting_factor` | the factors of `[38k §5(3)]` and `[38k §7(6)]` |
| `Route` | the two routes and their deadlines |
| `ThgError` | every refusal, with its remedy |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | `Serialize`/`Deserialize` on every public type |

## Standards and sources

- 38. BImSchV: `[38k §5(1)]`, `[38k §5(3)]`, `[38k §5(4)]`, `[38k §5(5)]`, `[38k §6(3)]`, `[38k §6(4)]`, `[38k §7(6)]`, `[38k §8(1) S. 1]`, `[38k §8(1) S. 3]`
- Ladesäulenverordnung `[LSV26 §4(1)]`
- `[AFIR Art. 20(1)]` — operator identification codes
- `[PTB-A 50.7 §3.1.7.2]` — which timestamp names a metering period

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices. This crate
reads the CDRs `emob-cdr` produces and the evidence `emob-eichrecht` verified.
Background: [Compliance](https://hupe1980.github.io/emob/docs/compliance/) on
the site; see also the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
