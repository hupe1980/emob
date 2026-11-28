# emobtest

Test and conformance toolkit for EV charging platforms: stations and cars that
charge on any CSMS over OCPP, a meter that signs OCMF, the checks a receipt and
an Eichrecht chain make, the rating an invoice uses, an inbox for CloudEvents,
and suites that hold a replacement service to the contract it replaces.

The erasure suite restores a cut of the provider, billing and receivables journals against current customer key custody. It checks sign-in, the original erasure receipt, a second driver and a partially paid invoice. The shared-invoice suite partially pays a two-driver fleet invoice, checks a line credit’s UBL, tests payer and driver access and reopens the credit after a restart. The withdrawal suite charges a private card, allocates a payment, records a declaration and reconciles the original invoice, credit and entitlement across restart. The finance suite imports a statement, checks open cash against the archived UBL and retries a confirmation after replay. The estate suite also signs in as a private payer, two fleet drivers, a billing delegate and an unrelated user. It compares downloaded UBL to the original and exercises live revocation, provider restart and a billing outage.

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

## Run the external suite against emob

From the workspace root, `just e2e` builds the daemons and starts a private example estate on ephemeral loopback ports with fresh credentials and journals. The suite checks successful and refused charges, point prices, reservations, provider quotes and station tariff delivery. It also reviews, confirms and reverses card restrictions from the actual desk forms, restarts the provider on its original journal, then checks card history and charging again. These mutations run only in the runner’s disposable estate. The OCPP 2.x simulator issues a fresh transaction ID across reconnects. The suite checks the recorded decision for that exact ID. Two drivers charge against one fleet account. The suite withdraws an offer, rejects a tampered charge, advances the billing clock, checks the retail UBL against the simulated meter quantities and reopens the same document after restart. The runner stops its own estate on exit. It can run alongside a local demo.

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

The [integration OpenAPI document](../schemas/openapi.json) describes the service operations and outbound webhooks. Run the suite for the port your system provides.

```
emobtest conformance mcp emob mcp
emobtest conformance receiver https://erp.example/emob --secret-env EMOB_WEBHOOK_SECRET
emobtest conformance receiver https://erp.example/invoices --event-type de.emob.invoice.issued --secret-env EMOB_INVOICE_DELIVERY_SECRET
emobtest conformance tariff https://prices.example --tariff-id ad-hoc
emobtest conformance authorize https://emsp.example --token-env EMOB_SERVICE_TOKEN --known 04A1B2 --blocked 04FFEE
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
answer `csmsd` asks for within its two-second budget. The tariff suite checks the current seller document against its schema and reads that exact version by fingerprint. In pytest,
`conformance.receiver(url, secret).assert_passed()`.

The `mcp` suite launches the local command you specify on standard input and output. It discovers the tools, checks the meter verifier with a signed sample and a different meter’s key, exercises input limits, and waits for a clean exit. To check a replacement, substitute its executable and arguments for `emob mcp`.

## Recover a subscriber event

Use the original event ID and the named subscriber’s existing webhook secret. Point the command at `csmsd` for a charge detail record (`de.emob.cdr.issued`) or `billd` for an invoice (`de.emob.invoice.issued`). Supply the secret through the named environment variable.

```console
emobtest recover https://billing.example.org --subscriber erp \
  --event-type de.emob.invoice.issued --id R-2026-0001 \
  --secret-env EMOB_ERP_WEBHOOK_SECRET > invoice-event.json
```

A successful command writes the authenticated envelope bytes to stdout. It checks the fresh signature, original delivery and envelope IDs, issuing source, and published event schema before writing anything. Errors go to stderr and return a nonzero status. Responses are limited to 8 MiB. The request goes directly to the configured issuer over HTTPS, or HTTP on loopback for a local estate. In Python, `emobtest.recover_event(url, subscriber, event_type, event_id, secret=secret)` returns the same bytes.

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
