# emobtest

Test and conformance toolkit for EV charging platforms: stations and cars that
charge on any CSMS over OCPP, a meter that signs OCMF, the checks a receipt and
an Eichrecht chain make, the rating an invoice uses, an inbox for CloudEvents,
and suites that hold a replacement service to the contract it replaces.

Built on [emob](https://github.com/hupe1980/emob)'s own Rust crates, so a test
asserts with the code that bills; it speaks only public contracts — OCPP,
OCMF, HTTP, CloudEvents with Standard Webhooks, DATEX II — so it tests any
platform, not only emob.

```
pip install emobtest
```

No runtime dependencies; one abi3 wheel for Python ≥ 3.11.

## Sixty seconds

```python
import emobtest as et

meter = et.Meter(seed=7)  # signs OCMF, deterministically
records = meter.charge("30", start="2026-12-01T10:00:00+01:00", minutes=12)

et.assert_verifies(records[-1], meter.public_key)  # this key signed these bytes
et.assert_bills(records, meter.public_key, kwh="30")  # …and MessEG §33 lets it bill

tariff = et.Tariff(energy="0.59", session_fee="0.50")  # gross, as a driver is shown
tariff.total(kwh=30, power_kw=150)  # Decimal('18.20') — rated, not multiplied
tariff.preisstand  # '59CA' — the code on page, display, receipt
et.assert_price(invoice_line_total, tariff, kwh=30, power_kw=150)
```

A changed digit fails the way it should:

```python
tampered = records[-1].replace('"RV":', '"RV":1', 1)
et.assert_tampered(tampered, meter.public_key)
et.assert_not_billable([*records[:-1], tampered], meter.public_key).reasons
```

Money and energy are never floats: `Tariff(energy=0.59)` is a `ValueError`.
`AssertionError` means the system under test is wrong; `ValueError` means the
test is.

## Driving a CSMS

```python
with et.Station("wss://csms.example/ocpp", "CP-1", password="…", ocpp="2.0.1") as cp:
    cp.expect_display("Preisstand")
    session = cp.charge(card="04A1B2C3D4E5F6", car=et.Car.compact(soc=20), minutes=30)
    session.assert_accepted().assert_bills()  # billed what the meter signed
    cp.charge(card="04A1B2C3D4E5F6", fault="tampered")  # the CSMS must not bill this
```

OCPP 1.6, 2.0.1 and 2.1 over `ws://` or `wss://`: `ocpp-kit`'s wallbox and
vehicle, an emob-sim meter signing every register reading (register
`cp.meter_key` with the CSMS). Thirty minutes play in a second. Faults:
`tampered`, `dropped`. Its sessions are real to the CSMS — on production, use
your own test card.

A `ReserveNow` is answered `Accepted` and kept: `cp.reservations` lists what
the CSMS holds the station for. `et.Emsp()` answers a CSMS's authorisation as `empd` does, and misbehaves on
cue: `.on(uid).accepts()`, `.blocks()`, `.expired()`, `.stalls(3)`,
`.fails(503)`.

## Receiving events

```python
def test_a_session_reaches_the_erp(inbox, platform):
    platform.subscribe(url=inbox.url, secret=inbox.secret)  # your platform's API
    platform.run_a_session()
    cdr = inbox.expect("de.emob.cdr.issued", within=10)  # signed, schema-checked
    et.assert_bills(cdr.data["signed"], METER_KEY)
```

The inbox checks every delivery's Standard Webhooks signature and the event's
published schema, answers `204`, `200` for a duplicate id and `401` for a
delivery it cannot trust — what a well-behaved receiver does.

## Conformance

A service that takes the place of one of emob's runs its suite:

```
emobtest conformance receiver https://erp.example/emob --secret whsec_…
emobtest conformance authorize https://emsp.example --token … --known 04A1B2 --blocked 04FFEE
```

```
receiver · https://erp.example/emob
  ✓ a signed delivery is taken — 204 in 41 ms
  ✓ the same delivery again is taken, not an error — 200 in 12 ms
  ✗ a delivery signed with another secret is refused — 204 in 9 ms — anyone could post a record that bills
  ✓ a delivery fifteen minutes old is refused — 401 in 8 ms
  ✓ an unsigned delivery is refused — 401 in 7 ms
✗ 4/5 held — does not conform
```

`receiver` is for an ERP, a billing system or any subscriber in place of
`billd`; `authorize` is for an eMSP backend in place of `empd`, held to the
answer `csmsd` asks for within its two-second budget. In pytest,
`conformance.receiver(url, secret).assert_passed()`.

## On the command line

```
emobtest verify 'OCMF|{…}|{…}' 3059…        does this key sign this record?
emobtest price --energy 0.59 --kwh 30 --power 150
emobtest feed table.json                   will the Mobilithek take it?
emobtest inbox --port 9199                 show events live, checked
```

## pytest

The plugin loads itself. Fixtures: `meter`, `inbox`, `emsp`, `tariff`,
`emob_seed`, and `station` — a factory dialling `--emob-ocpp URL` (trusting
`--emob-ca PEM`), skipped without it. Option `--emob-seed N`.

## Also

`meter.sign(kwh, at=…, marker="C")` for a record a session would not produce
on its own; `et.evse_id(…)` and `et.emaid(…)`, check digit computed and
checked; `et.check_feed(bytes)` and `et.assert_feed(…)` for a DATEX II AFIR
file; `et.event_problems(…)`; `et.webhooks.sign/verify`.

Licence: MIT OR Apache-2.0, as emob.
