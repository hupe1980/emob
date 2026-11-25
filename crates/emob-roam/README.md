# emob-roam

Carries one canonical charge detail record onto OCPI 2.3.0, OCPI 2.2.1 and OICP 2.3 — with an explicit account of what each translation cost — and routes it to the partner the contract identifier names.

[![crates.io](https://img.shields.io/crates/v/emob-roam.svg)](https://crates.io/crates/emob-roam) [![docs.rs](https://img.shields.io/docsrs/emob-roam)](https://docs.rs/emob-roam) [![MSRV 1.94](https://img.shields.io/badge/MSRV-1.94-blue)](#) [![License](https://img.shields.io/crates/l/emob-roam.svg)](#license)

Roaming means a CDR (charge detail record) is sent to another company, on
another protocol version, which pays against it. Translation is where roaming
money goes missing: a hand-written `From` impl makes every lossy decision
silently, and weeks later two companies hold two different numbers for one
session. Every translation here returns a `Crossing` — the document plus a
reason, by JSON Pointer, for each rounding or each distinction the wire could
not carry. Nothing opens a socket or reads a clock.

```console
cargo add emob-roam
```

## Example

```rust
use emob_roam::{Partner, PartnerRegistry, Reach};

let registry = PartnerRegistry::new("DE*CPO".parse()?)
    .with(Partner::emsp("NL*TNM".parse()?).on_signed_data());

// Routed by what the contract identifier itself says (eMAID prefix NL-TNM).
let Some(Reach::Direct(party)) = registry.route(&"NL-TNM-000122045-U".parse()?) else {
    unreachable!()
};
let partner = registry.get(&party).unwrap();

// `context` holds the token, the location, the signed meter records and
// `last_updated` — see `emob_roam::ocpi::cdr::Context`.
let crossing = emob_roam::ocpi::cdr::for_partner(&cdr, partner, &context)?;
for reason in crossing.reasons() {
    eprintln!("{reason}");
    // /total_time: 1200 s is 0.3333 h rounded to 4 places … the cost beside it
    // was computed from whole seconds, so re-deriving it will not reproduce it
}
```

`for_partner` emits the OCPI version the partner is registered as speaking
(`Outbound::V2_3_0` or `Outbound::V2_2_1`) and folds the downgrade's cost into
the same `Crossing`. Use `to_ocpi` and `downgrade` directly only when you
already know the version.

## What it guarantees

- **Every loss is stated.** Rounding, dropped distinctions and uncovered spans
  appear in `Crossing::reasons`, composed with `ocpi-kit`'s own account, so a
  2.2.1 partner gets one report rather than two halves.
- **No V2G discharge crosses as a draw.** `ENERGY_EXPORT` is *Session Only*
  `[OCPI 2.3.0 §mod_cdrs_cdrdimensiontype_enum]` and `total_energy` has no sign,
  so a discharge would settle backwards — the provider paying for energy the
  driver supplied. `to_ocpi` refuses it (`RoamError::ExportNotExpressible`); an
  inbound CDR with an `ENERGY_EXPORT` volume is blocked in the pre-flight rather
  than netted or dropped.
- **No unrated record crosses.** OCPI says *"a `total_cost` of 0.00 means free
  of charge"*, so a placeholder zero would answer the price question
  permanently in the partner's favour.
- **No tariff restriction is dropped or defaulted.** Dropping a restriction
  widens the element; defaulting a `start_time` to midnight turns a night tariff
  into an all-day one. Both are `RoamError::RestrictionNotExpressible`, and a
  test asserts every expressible restriction arrives with its value.
- **The contract id's check digit is verified before it routes money.**
  `ocpi-kit` decodes but does not check it (a wire library must not let one bad
  id poison a page); `RoamingToken::new` refuses an eMAID — the contract
  identifier — whose check digit fails.
- **Credit CDRs are never read as sessions.** Inbound, a Credit CDR is refused
  by name: it repeats the original's kilowatt-hours with negated money.

## How it works

### Durations in hours are usually not decimals

OCPI states `total_time`, `TIME` and `PARKING_TIME` in hours
`[OCPI 2.3.0 §mod_cdrs_cdr_object]`. 3600 = 2⁴·3²·5², so a duration has an exact
decimal only when nine divides its seconds: 21 minutes is `0.35`, 20 minutes is
not. The money was computed from whole seconds, so the crossing notes where a
partner re-deriving it from the rounded hours gets a different figure. (The same
arithmetic makes an occupancy fee of €2.50 per hour unlawful under
`[AFIR Art. 5(4)]`.)

### Two different `PARKING_TIME`s

After an erratum, the `PARKING_TIME` **dimension** means time the vehicle is
not requesting power; the CDR **field** `total_parking_time` still means time no
energy was transferred. They differ by exactly the time the point withheld
power. The crossing takes the dimension from `Dimension::pricing` and the field
from `Activity::transfers_energy`. A withheld period crosses with energy and no
time dimension, and is read back as withheld rather than as occupancy.

### Charging periods have a start and no end

An OCPI period runs to the next one's start. A canonical period has both ends,
so a gap (authorised at 10:00, first meter value at 10:20) is reported rather
than filled with an invented zero-energy period.

### Rating notes travel to the partner

OCPI states a quantity and a cost per dimension but nothing about the distance
between them — a promotional first tier or a `step_size` makes a document that
does not multiply out. Notes that `emob_tariff::RatingNote::concerns_the_payer`
selects are attached to that dimension's total; faults in this side's tariff
stay with the operator.

### Plug & Charge and AutoCharge share one value

`AUTH_REQUEST` covers both an ISO 15118 contract certificate and a spoofable MAC
address (AutoCharge). The crossing notes the loss and points at the
identification strength read off the signed meter data, where the distinction
survives.

### A tariff's `22:00` needs the Location's time zone

Tariff time restrictions are local time at the charge point
`[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`; the zone lives on the
Location (`time_zone`) `[OCPI 2.3.0 §mod_locations_location_object]`, not on the
Tariff. `to_ocpi` for tariffs names the tariff's own zone so you can check every
Location it applies at carries the same one.

### Routing by the contract identifier

`DE-ABC-C00122045-6` names its issuing eMSP in its first five characters. The
registry routes by that prefix instead of a hand-maintained map; a direct peer
beats a hub, and a partner that issues under another namespace can declare it,
which wins over the prefix (OCPI states a party id has no fixed link to the
issuer).

```rust
let registry = PartnerRegistry::new("DE*CPO".parse()?)
    .with(Partner::hub("DE*HUB".parse()?))
    .with(Partner::emsp("NL*TNM".parse()?).on_signed_data());

registry.route(&"NL-TNM-000122045-U".parse()?);   // Reach::Direct(NL*TNM)
registry.route(&"FR-XYZ-C00000001-4".parse()?);   // Reach::Hub { issuer: FR*XYZ }
```

The token UID is an argument, not a CDR field: `emob_session::Authorization`
stores only a keyed hash because a UID identifies an object a person carries for
life. OCPI needs the real value, so the party holding the mapping supplies it at
the edge, for leaving records only.

### Locations come from the AFIR feed's model

A CPO publishes each location twice: to partners in OCPI and to the national
access point `[AFIR Art. 20(2)(f)]`. This crate has no location model of its
own; it reads `emob_poi::site`, the same source as the DATEX II feed, so the two
cannot drift. OCPI length limits are enforced, not truncated — a truncated
address names a different building.

### Corrections and invoice reconciliation

OCPI cannot amend a CDR. `to_ocpi_credit` writes the Credit CDR (`credit = true`,
`credit_reference_id`, only `total_cost` negated); the replacement is a new CDR
whose account says what it replaces.

`[OCPI 2.3.0 §mod_invoice_reconciliation_module]` ties an invoice id to the CDR
ids it covers. `reconciliation::check` sums this side's canonical records, each
with its exact rating, and answers in four kinds: an id nobody here holds, an id
since corrected (reported beside its replacement), a record held unrated, and a
second currency (which makes the total absent rather than approximate).

```rust
let record = reconciliation::to_ocpi(&cpo, "rec-2026-03", "R-2026-0001", &covered, now)?;
let checked = reconciliation::check(&record, &cpo, &ledger);
assert!(checked.is_conclusive());
```

### OICP carries no money

An OICP 2.3 CDR has no amount; the price crosses separately as a pricing
product both parties agreed on (`oicp::pricing::to_oicp_product`). So the
account on an OICP record opens by stating the amount and that it did not
travel.

## API at a glance

| Item | Purpose |
|---|---|
| `PartnerRegistry`, `Partner`, `Reach`, `Wire` | peers, their protocol and version, routing by contract issuer |
| `RoamingToken` | the token OCPI requires, with check-digit verification |
| `ocpi::cdr::for_partner`, `ocpi::cdr::to_ocpi`, `ocpi::cdr::downgrade`, `ocpi::cdr::to_ocpi_credit` | outbound CDRs, 2.3.0 and 2.2.1, and Credit CDRs |
| `ocpi::tariff::to_ocpi`, `ocpi::location::cdr_location` | outbound tariffs and locations |
| `preflight`, `from_ocpi` | inbound: OCPI's own checks first, then conversion |
| `ocpi::reconciliation::to_ocpi`, `ocpi::reconciliation::check` | invoice reconciliation |
| `oicp::cdr::to_oicp`, `oicp::pricing::to_oicp_product` | OICP 2.3 CDR and pricing product |
| `Crossing`, `Note`, `RoamError` | the account of a translation; every refusal |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | serialises the routing facts (`Wire`, `Reach`) a roaming node records about where a record went |

## Standards and sources

- OCPI 2.3.0 and 2.2.1 — CDRs, tariffs, locations, invoice reconciliation, via `ocpi-kit`
- OICP 2.3 (Hubject) — CDRs and pricing products, via `oicp-kit`
- `[AFIR Art. 5(4)]`, `[AFIR Art. 20(2)(f)]`

Tests: `tests/the_same_session.rs` runs one genuinely signed session over
self-roaming, OCPI 2.3.0 and OCPI 2.2.1 and asserts the same money, with the
signed records re-verified at the far end against the receiver's own key
registry. `tests/the_same_session_over_oicp.rs` does the same over OICP against
`oicp-kit`'s in-process broker, which refuses a CDR for a session it never
opened and a duplicate submission.

## Status

✅ OCPI 2.3.0 and 2.2.1 in both directions (CDRs, tariffs, locations) and OICP 2.3
outbound, at the library level. ✅ Transport, the OCPI receiver and the push queue
are [`roamd`](../../services/roamd)'s (OCPI); ⏳ its OICP client. eMIP (GIREVE) is not supported.

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices. This crate
sits between `emob-cdr` (records) and `roamd` (the roaming daemon). Background
and regulation: [Roaming](https://hupe1980.github.io/emob/docs/roaming/) on the
site; see also the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
