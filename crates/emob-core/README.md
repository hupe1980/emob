# emob-core

The shared domain model of the [emob](https://github.com/hupe1980/emob)
e-mobility stack: identifiers, exact quantities, time zones, and the regulatory
obligation calendar for charge points, providers and operators.

[![crates.io](https://img.shields.io/crates/v/emob-core.svg)](https://crates.io/crates/emob-core) [![docs.rs](https://img.shields.io/docsrs/emob-core)](https://docs.rs/emob-core) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-core.svg)](#license)

Every other emob crate is written against these types. They keep identifiers
exactly as a partner sent them, never let a binary float near money or energy,
and turn AFIR, Eichrecht (German calibration law), NIS2 and the German price
rules into dated, cited, executable duties — so "are we compliant on 1 January
2027?" is a function call.

```console
cargo add emob-core
```

## Example

```rust
use emob_core::obligation::assess;
use emob_core::station::ChargePointProfile;
use time::macros::date;

let point = ChargePointProfile::bare("DE*AB7*E840*6487".parse()?, date!(2026-06-01));
let report = assess(&point, date!(2027-01-01));

for finding in report.failing() {
    println!("{}  {}", finding.obligation.citation, finding.obligation.remedy);
    // [AFIR Art. 5(1)]  offer a contract-free path at the point, or make the point free of charge
    // [AFIR Art. 5(7)]  connect the point to a CSMS: without it neither the data
    //                   nor the smart-charging duties can be met
    // [LSV26 §4]        file the electronic Inbetriebnahme notice within two weeks
    // …
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What it guarantees

- **Identifiers round-trip verbatim.** Hubject compares the identifier in a URL
  with the TLS client certificate as text and answers a mismatch with
  `017 Unauthorized Access`, so nothing is normalised on ingest.
- **A contract identifier's check digit is checked.** One wrong character is
  refused rather than routed, because a mistyped eMAID still parses and bills
  somebody else's contract.
- **No binary floats.** `cargo xtask no-floats` fails the build on any `f32` or
  `f64` in the workspace; every quantity is a `rust_decimal::Decimal`.
- **Scale is preserved.** `2935.600 kWh` stays three decimals, because OCMF
  forbids discarding the resolution a meter states.
- **Energy is never netted.** `Energy` is a non-negative magnitude and
  `Direction` says which way it flowed, so a V2G discharge cannot cancel a draw.
- **Every duty carries its citation and its date window.** A duty cannot be
  applied before it exists, and every citation resolves to an indexed source
  (`cargo xtask check-citations`).
- **No I/O, no clock.** Every function that needs "now" takes it as an argument,
  so a question about a date two years out is the same call as one about today.

## Identifiers: two grammars, one identity

`DE*AB7*E840*6487` and `DEAB7E8406487` are the same charge point. They compare
equal and hash the same, and each writes itself back exactly as it arrived:

```rust
use emob_core::EvseId;

let separated: EvseId = "DE*AB7*E840*6487".parse()?;
let packed: EvseId = "DEAB7E8406487".parse()?;

assert_eq!(separated, packed);                          // one charge point…
assert_eq!(separated.to_string(), "DE*AB7*E840*6487");  // …each written back verbatim
assert_eq!(packed.to_string(), "DEAB7E8406487");
assert_eq!(separated.operator_id(), "AB7");
# Ok::<(), emob_core::IdError>(())
```

Call `canonical()` when you want the normalised form deliberately. `-` is a
separator in a contract id but not in an EVSE id, because the eMI3 grammar does
not define one there; accepting it would make `DE-AB7-E840` compare equal to a
real charge point.

[`Emaid`] — the e-mobility account identifier, i.e. the contract id — accepts all
three contract grammars (ISO 15118-1, eMI3 and DIN SPEC 91286). It tells them
apart by shape, because their instance sections differ in length and their
check-digit algorithms differ:

```rust
use emob_core::ids::Emaid;

let iso: Emaid = "NL-TNM-000122045-U".parse()?;
let packed: Emaid = "NLTNM000122045".parse()?;   // written without the digit

assert_eq!(iso, packed);                          // one contract
assert_eq!(packed.check_digit(), 'U');            // computed, not invented
assert!("NL-TNM-000122045-X".parse::<Emaid>().is_err());
# Ok::<(), emob_core::IdError>(())
```

A DIN card also has an eMI3 spelling: `NL-TNM-012204-5` and `NL-TNM-C00122045-K`
are one contract, and `to_emi3()` / `to_din()` convert explicitly. They are not
equal on their own, because an ISO instance that happens to start `C0` would
then collide with a DIN contract nobody issued.

`PartyId` reads every spelling the market uses (`DE*ABC`, `DE-ABC`, `DEABC`, …)
and writes one, so a routing table cannot hold the same party twice.

## Quantities: exact energy and money

```rust
use emob_core::Energy;
use rust_decimal::Decimal;
use std::str::FromStr;

// The subtraction OCMF prescribes for a session's energy, done exactly.
let start = Energy::from_kwh(Decimal::from_str("2935.600")?)?;
let end   = Energy::from_kwh(Decimal::from_str("2965.100")?)?;

assert_eq!((end - start)?.to_string(), "29.500 kWh");
# Ok::<(), Box<dyn std::error::Error>>(())
```

- `Energy::from_wh` moves the decimal point rather than dividing, so `29500 Wh`
  becomes `29.500 kWh`.
- `Money::round_to_minor_unit` reads `Currency::minor_unit_digits` (ISO 4217:
  the yen has none, the dinar three) and rounds *to* that scale in both
  directions, so an invoice line prints `10.00 EUR`, not `10 EUR`. Money's scale
  belongs to the currency; energy's scale belongs to the meter — which is why
  they are separate types.
- Magnitudes are bounded: `Decimal` panics on overflow, and both operands of a
  later multiplication can come from a partner's record. `Energy::MAX_KWH` is one
  terawatt-hour — thirty years of the largest Megawatt Charging System unit
  running flat out.
- `Money::checked_add` and `checked_sub` check the currency first, then the sum.

### One apportionment

`apportion(base, delta, offset, span)` gives the cumulative value `offset` units
into a window of `span` across which a register moved by `delta`. `emob-session`
uses it for quarter-hour boundaries and `emob-tariff` for tariff thresholds, so
both settle money with one implementation. It multiplies before it divides and
rounds to `APPORTIONED_SCALE` (twelve places, a nanowatt-hour), so the pieces of
a split sum back to the whole exactly.

## Charging activity: charging, parked, or withheld

A plugged-in vehicle draws nothing for two different reasons, and only one owes
an occupancy fee. OCPI 2.3.0 redefined `PARKING_TIME` as time the *vehicle is
not requesting power*, so a driver is not charged a loitering fee while the
point withholds energy `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`.

| `Activity` | priced as | energy transferred |
|---|---|---|
| `Charging` | `TIME` | ✅ |
| `Parked` — the vehicle stopped asking | `PARKING_TIME` | |
| `Withheld` — the point stopped offering | *nothing* | |

`Withheld` covers a zero charging profile, an `[EnWG §14a]` dimming, a grid
limit or a fault. `emob_tariff::Dimension::pricing` says which dimension prices
which activity.

## Identification strength

`IdentificationStrength` compares how a session was authorised with how the
signed meter record says the user was identified `[OCMF Tab. 11]`; when they
disagree, the signed one wins. OCMF's error levels (`MISMATCH`, `INVALID`) are
not on the scale, because a rejected certificate is a failure, not a weak
identification.

## Time zones, not UTC offsets

A tariff's "0.30 from 22:00" is local civil time at the charge point
`[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, and OCPI carries the zone
on the Location `[OCPI 2.3.0 §mod_locations_location_object]`. An offset is only
how one timestamp was written; judged by offset instead of zone, one session
under a German night tariff costs €6.00 stamped `+01:00` and €9.00 stamped `Z`.

```rust
use emob_core::TimeZone;
use time::macros::{date, datetime, time};

let berlin = TimeZone::new("Europe/Berlin")?;

// One instant, two spellings, one wall clock.
assert_eq!(berlin.local(datetime!(2026-01-02 21:00 +0)).time, time!(22:00));
assert_eq!(berlin.local(datetime!(2026-01-02 22:00 +1)).time, time!(22:00));
assert_eq!(berlin.local(datetime!(2026-07-02 21:00 +0)).time, time!(23:00)); // summer time

// A civil time can happen once, or twice.
assert_eq!(berlin.instants_at(date!(2026-03-29), time!(2:30)).len(), 1); // spring gap
assert_eq!(berlin.instants_at(date!(2026-10-25), time!(2:30)).len(), 2); // autumn fold
# Ok::<(), Box<dyn std::error::Error>>(())
```

The tz database is compiled in (`jiff` with `tzdb-bundle-always`); nothing reads
`/usr/share/zoneinfo` or `TZ`, so two machines give the same answer and
`Cargo.lock` pins the version. An unknown zone name is refused rather than
replaced with UTC.

## The obligation calendar

**51 duties** from AFIR — Article 5 and Annex II — Delegated Regulation (EU)
2025/656, the German Ladesäulenverordnung (LSV 2026), the Preisangabenverordnung,
MessEG/PTB-A, the THG-Quote (greenhouse-gas quota) preconditions, the
Bundesnetzagentur's MiSpeL rules for bidirectional charging and the NIS2/CRA
cybersecurity regime, each a dated, cited, executable rule. Each duty
is judged against exactly one of three subjects:

| Subject | Function | Example duty |
|---|---|---|
| `ChargePointProfile` | `assess` | `[AFIR Art. 5(1)]` ad-hoc payment |
| `ProviderProfile` (eMSP) | `assess_provider` | `[AFIR Art. 5(5)]` no cross-border roaming surcharge |
| `UndertakingProfile` (the company) | `assess_undertaking` | `[NIS2 Anh. I]` risk management, registration |

```rust
use emob_core::obligation::{assess_provider, assess_undertaking, ObligationId, Status, Verdict};
use emob_core::station::{ProviderProfile, RiskManagement, UndertakingProfile};
use emob_core::PartyId;
use time::macros::date;

let mut provider = ProviderProfile::bare(PartyId::new("DE", "MSP")?);
provider.surcharges_cross_border_roaming = true;
assert_eq!(assess_provider(&provider, date!(2026-09-01)).verdict(), Verdict::Failing);

let mut operator = UndertakingProfile::bare(PartyId::new("DE", "CPO")?);
operator.operates_recharging_points = true;
operator.employees = 400;
operator.risk_management = RiskManagement::complete();

let report = assess_undertaking(&operator, date!(2026-09-01));
assert_eq!(report.status_of(ObligationId::Nis2RiskManagement), Some(Status::Satisfied));
assert_eq!(report.status_of(ObligationId::Nis2Registration),   Some(Status::Failing));
# Ok::<(), Box<dyn std::error::Error>>(())
```

How the calendar reads the law:

- **Applicability and satisfaction are separate.** A private depot is not
  *failing* the ad-hoc payment duty; the duty does not bind it. Reports that
  conflate the two cry wolf. `starting_between` answers the planning question:
  what lands between now and 2028.
- **A renovation is not a deployment.** Article 5 uses three wordings: "deployed
  from 13 April 2024" (`commissioned_on`), "built after … or renovated after …"
  (both dates), and "installed or renovated from …"
  (`installed_or_renovated_on()`, for `[DA-656 Anh. 2.1]`). The DA-656 legacy
  exemption is a date, not a technology.
- **Calibration paperwork is a duty too.** The `[MessEG]` rows cover the
  verification period: eight years `[MessEV Anl. 7 Nr. 6.7]` from placing on the
  market `[MessEG §37(1) S. 2]`, ending with the calendar year `[MessEV §34(2)]`.
- **German price indication is wider than AFIR.** `[PAngV §14]` requires the
  energy price (*Arbeitspreis*) at the post since 28.05.2022 at any power, so a
  22 kW post priced per minute alone is lawful under `[AFIR Art. 5(4)]` and an
  administrative offence in Germany.
- **AC-side metering of DC stations** is permitted only in the narrow case
  `[REA 6-A]` allows — before 2018, at most 50 kW, one rectifier per session — and
  only if the customer is told that part of the billed energy is conversion loss.
- **NIS2's risk-management measures are a conjunction** `[NIS2 Art. 21(2)]`:
  `RiskManagement::missing()` names the gaps rather than scoring them.
- **Some entries are entitlements, not duties.** The THG-Quote and the two
  MiSpeL entries are `Consequence::ForgoneEntitlement`: a point that misses them
  breaks nothing and leaves money unclaimed. A bidirectional point
  (`ChargePointProfile::mispel`, a `MispelPosture`) has its fed-back energy
  relieved of levies only under a `Formelsatz` — `A2` or `A11` — with its
  metering concept (`mispel-metering-concept`, `[MiSpeL A1 3.2.5]`, from
  21.10.2026), and until 30.09.2027 only with the grid and metering operators'
  consent (`mispel-operator-consent`, `[MiSpeL Tenor 9]`). The arithmetic is
  `emob_session::mispel`.
- **The SME test's financial half is an `and`.** An undertaking exceeds the SME
  ceilings with 250 or more staff, *or* when turnover *and* balance sheet are
  both above the thresholds. Many summaries drop the second condition.
- **A directive binds from transposition.** NIS2 applies in Germany from
  06.12.2025 (NIS2UmsuCG), not the date in `[NIS2 Art. 41]`; registration has a
  three-month window. The Cyber Resilience Act is a Regulation, so
  `[CRA Art. 71]`'s dates apply directly.

Fifty-one duties, each a pair of closures (applies, satisfied), are tested in
both directions: a fully compliant subject fails nothing, and every entry fails
for at least one subject in a test panel.

## Wire formats and validated deserialisation

`time`'s derived `Serialize` writes an instant as an array of its internal
fields, which round-trips perfectly and no partner can read. `emob_core::wire`
pins the spellings partners use — RFC 3339 instants, `YYYY-MM-DD` dates,
`HH:MM:SS` times, whole-second durations, day names.

Every type whose constructor can refuse deserialises through that constructor,
so a negative `Energy`, an invalid currency code or a quarter hour off the
settlement grid is refused on the way in, not just when built in code.
`cargo xtask check-constructors` enforces this.

## Protocol generation names

`V2gCommunication` states which vehicle-communication generations a point
implements, because `[DA-656 Anh. 2.1.1–2.1.3]` binds a point by exactly that.
`[DATEX-II-Profil Tab. A.130]` spells `iso15118` with no generation and defines
it as -20, so "we do ISO 15118" published there claims a duty the estate may not
meet. `protocol_names()` emits the [`iso15118`] crate's own names (`din70121`,
`iso15118-2`, `iso15118-20`), and a test asserts they match.

[`iso15118`]: https://github.com/hupe1980/iso15118

## Crossing: what a wire translation loses

Every translation onto a wire in emob returns a `Crossing<T>`: the value, plus
an account — by RFC 6901 pointer into the recipient's document — of anything
rounded, collapsed or dropped.

```rust
let crossing = emob_ocpp::to_ocpp(&tariff, at)?;
for reason in crossing.reasons() {
    // /energy/prices/0: this tariff's prices are gross and OCPP 2.1 quotes
    //                   them net …
}
```

OCPI in `emob-roam`, DATEX II in `emob-poi` and OCPP 2.1 tariffs in `emob-ocpp`
all report through it.

## API at a glance

| Item | Purpose |
|---|---|
| `EvseId`, `Emaid`, `PartyId`, `SessionId`, `CdrId`, `TariffId`, … | Text-preserving identifiers |
| `Energy`, `Money`, `Currency`, `Direction` | Exact quantities |
| `apportion`, `APPORTIONED_SCALE` | Shared linear interpolation |
| `QuarterHour`, `ClockResolution` | The settlement grid |
| `Activity`, `IdentificationStrength` | Session facts several crates share |
| `TimeZone`, `Local` | IANA zones, gaps and folds |
| `obligation::assess`, `assess_provider`, `assess_undertaking`, `starting_between` | The obligation calendar |
| `ChargePointProfile`, `ProviderProfile`, `UndertakingProfile` | Subjects the calendar judges |
| `Crossing`, `Note` | The account a wire translation owes |
| `wire` | Serde helpers for partner-readable spellings |

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `serde` | yes | `Serialize`/`Deserialize` for every public type |

## Standards and sources

AFIR (EU 2023/1804) Art. 5, 20 and Annex II; Delegated Regulation (EU) 2025/656;
LSV 2026; PAngV; MessEG, MessEV; REA 6-A; NIS2 and NIS2UmsuCG; the Cyber
Resilience Act; OCPI 2.3.0; OCMF; DATEX II AFIR profile; ISO 4217.

## Part of emob

emob is an open-source e-mobility stack for charge point operators and
e-mobility service providers. See the
[compliance documentation](https://hupe1980.github.io/emob/docs/compliance/) and
the [root README](https://github.com/hupe1980/emob).

## License

Licensed under either of Apache License 2.0 or MIT, at your option.
