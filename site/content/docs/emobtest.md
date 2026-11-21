+++
title = "emobtest: test and conformance for charging platforms"
description = "A Python toolkit on emob's crates: stations and cars that drive a CSMS, a meter, the invoice's rating, an event inbox, and conformance suites."
weight = 12
+++

`emobtest` is the toolkit for testing a charging platform from Python —
emob, or any other. It is built on emob's Rust crates, so a test asserts with
the code that bills, and it drives only public contracts: OCMF, HTTP,
CloudEvents with Standard Webhooks, DATEX II.

```console
pip install emobtest
```

No runtime dependencies; one abi3 wheel for Python 3.11 and later.
`AssertionError` means the system under test is wrong, `ValueError` that the
test is.

## Sign, check, bill

```python
import emobtest as et

meter = et.Meter(seed=7)                       # emob-sim's meter, deterministic
records = meter.charge("30", start="2026-12-01T10:00:00+01:00", minutes=12)

et.assert_verifies(records[-1], meter.public_key)       # the receipt's verdict
et.assert_bills(records, meter.public_key, kwh="30")    # the Eichrecht chain's
```

`assert_tampered` and `assert_not_billable` are the other side: one changed
digit, and both say why.

## Prices

```python
tariff = et.Tariff(energy="0.59", session_fee="0.50")   # as [[tariffs]] states it
tariff.total(kwh=30, power_kw=150)   # Decimal('18.20'), rated by the function that bills
tariff.lines()                       # ['0.59 EUR / kWh', '0.50 EUR / session'] — AFIR order
tariff.preisstand                    # '59CA' — page, display and receipt
et.assert_price(charged, tariff, kwh=30, power_kw=150)
```

A float is refused: money and energy are exact or they are not money and
energy.

## Stations and cars

A station on any CSMS's OCPP endpoint — 1.6, 2.0.1 or 2.1, `ws://` or
`wss://` — that charges a car with a card the way a field station does:
`ocpp-kit`'s wallbox and vehicle for the physics and the protocol, an
emob-sim meter signing every register reading. Half an hour plays in a second,
as having happened over the last half hour.

```python
with et.Station("wss://csms.example/ocpp", "CP-1", password="…", ocpp="2.0.1") as cp:
    cp.expect_display("Preisstand")                      # what the CSMS shows
    session = cp.charge(card="04A1B2C3D4E5F6", car=et.Car.compact(soc=20), minutes=30)
    session.assert_accepted().assert_bills()             # what the meter signed, billed
    cp.charge(card="04A1B2C3D4E5F6", fault="tampered")   # …and what a CSMS must refuse
```

`fault="tampered"` changes the closing reading after it was signed,
`"dropped"` signs one and never sends it. Register `cp.meter_key` with the
CSMS first, or it is right to refuse to bill. `et.Emsp()` is the other side
of an authorisation: `.on(uid).accepts()`, `.blocks()`, `.expired()`,
`.stalls(3)`, `.fails(503)`, answered as `empd` does.

`just e2e` runs emob's estate and drives it this way on all three versions;
in pytest, the `station` fixture dials `--emob-ocpp` (trusting `--emob-ca`).
The station is a real one to the CSMS: its sessions are invoiced like any —
on a production estate, use the operator's own test card.

## Events

The `inbox` fixture is a receiver on loopback: point a subscriber at
`inbox.url` with `inbox.secret`, and `inbox.expect("de.emob.cdr.issued")`
returns the event — signature checked, schema checked — or fails saying what
did arrive. `et.assert_event(…)` holds any event to its published schema.

## Conformance

A service that takes the place of one of emob's runs its suite:

```console
emobtest conformance receiver https://erp.example/emob --secret whsec_…
emobtest conformance authorize https://emsp.example --token … --known 04A1B2 --blocked 04FFEE
```

- **`receiver`** — for an ERP, a billing system or any `[[subscribers]]` entry
  in place of `billd`: a signed delivery is taken, the same delivery again is
  taken rather than an error, and a delivery signed with another secret,
  fifteen minutes old or unsigned is refused. Its samples are marked
  `emobtest` and are taken without being booked.
- **`authorize`** — for an eMSP backend in place of `empd`: no token or a wrong
  one is `401`, an unknown uid is `Invalid` with a reason, a known one
  `Accepted` naming its contract, a blocked one `Blocked`, and every answer
  within the two seconds `csmsd` waits.

emob's own `billd` and `empd` pass both. In pytest:
`conformance.receiver(url, secret).assert_passed()`.

## On the command line

```console
emobtest verify 'OCMF|…' 3059…          does this key sign this record?
emobtest price --energy 0.59 --kwh 30   what does a session cost?
emobtest feed table.json                will the Mobilithek take it?
emobtest inbox --port 9199              emob's events, live and checked
```

**Related:** [Eichrecht](@/docs/eichrecht.md) ·
[Tariffs and price transparency](@/docs/pricing.md) ·
[Run the estate](@/docs/run-the-estate.md)
