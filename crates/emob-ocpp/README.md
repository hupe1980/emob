# emob-ocpp

**The OCPP seam** — from a charging station's transaction events to a session
that can be billed, with the rule that no float from the telemetry ledger ever
becomes a kilowatt-hour on an invoice.

Part of [emob](https://github.com/hupe1980/emob), the open-source e-mobility
operating stack. Not on crates.io yet: it depends on
[`ocpp-kit`](https://github.com/hupe1980/ocpp-kit) `0.1`, which is unreleased,
so building this crate needs that repository checked out beside the workspace.
The five crates that decide money have no such dependency — see *Why this is a
crate* below.

## The rule this crate makes structural

OCPP carries two kinds of meter value, and only one of them is money.

The **numeric** ones — `meterStart`, `meterStop`, `SampledValue.value` — are
operational telemetry. They answer whether every event arrived and whether the
sequence is complete.

They are exact, not floating point: `ocpp-kit` carries every OCPP number as a
decimal, so its CSMS ledger holds `meter_wh: Option<Decimal>` and the scale a
meter claimed survives the wire. That closes one failure and does not touch this
one. **Exact is not billable.**
The Open Charge Alliance's own example message carries `meterStop: 108814` — an
exact integer, and the meter's *lifetime* register in watt-hours, while the
signed data set beside it reports `0.636 kWh` for the transaction. A CSMS
billing the protocol's number would bill a figure nothing signed, from a
register that is not the session's, out by a factor of a hundred and seventy
`[OCA SMV §5.2]`, and every digit of it correct.

The **signed** one is a `SignedMeterValueType` carrying an OCMF data set, and it
is the only thing here that becomes a billed kilowatt-hour.

This crate makes that a property of the types rather than a rule somebody
remembers: **its input vocabulary has no numeric meter value in it at all.**
`TransactionEvent` carries signed values, instants, and whether energy was
flowing. There is no field to put a float in, so there is no path from one to a
`Cdr`.

The Open Charge Alliance's own example message is what makes the point concrete.
Its `meterStop` is **108814** — the meter's *lifetime* register, in watt-hours —
while the transaction's signed difference is **0.636 kWh**. A CSMS billing the
protocol's number would bill a figure nothing signed, from a register that is
not the session's, and be out by a factor of a hundred and seventy
`[OCA SMV §5.2]`.

## The protocol half is not here any more

What a station sends is an OCMF data set wrapped three deep, and every layer is
somewhere an implementation goes wrong: OCPP 2.x has a typed field for it, while
**OCPP 1.6 serialises the whole object into the `value` string** of a
`SampledValue` whose `format` is `SignedData` — a string holding JSON holding
Base64 holding the record `[OCA SMV §3.2.1]`. The `publicKey` beside it is Base64
over a colon-separated envelope, `oca:base16:asn1:<hex>`, whose last component is
the key *as printed on the certified meter* — and the same document's own example
then sends Base64 over plain hex with no envelope at all, so a reader that
implemented only the specification would reject the specification's own example
`[OCA SMV §3.2.2]`.

None of that lives here. It is spec knowledge every OCPP CSMS doing German
Eichrecht has to reimplement, which is the definition of something belonging in
the protocol kit, so `ocpp-kit` owns it: `metering::SignedMeterValue::decoded_str`,
`metering::decode_public_key`, and a version-neutral `csms::events::DomainEvent`
that carries the signed values through. One `match` covers all three versions
here. `OCPP-KIT_FEEDBACK.md` records what was asked for and what
landed.

The public key is still a **claim**, wherever it is decoded: OCMF is explicit
that the key travels out of band, and a key arriving on the same socket as the
record it signs proves only that whoever holds that socket owns a private key.
The key that decides anything comes from `emob_eichrecht::KeyRegistry`.

## A retry is not a reading

OCPP transports retry. A `MeterValues.req` that does not get its confirmation in
time is sent again, and the same signed record arrives twice — with the same
pagination counter, because the meter produced one. A CSMS that appends both
hands the chain a duplicate, and the chain answers `PaginationBreak`: a
transport retry reported as a missing record, on a session that is intact.

Records are de-duplicated by the digest of the bytes their signature covers. Two
that hash the same *are* one record; two that differ are both kept, because a
station that reused a counter for different content is exactly the fault the
chain exists to find. `Assembled::duplicates_dropped` reports how often the link
retried.

## What comes from where

Two sources describe one charging process and neither is sufficient alone.

| Fact | Source | Why not the other |
|---|---|---|
| The register, and its scale | the **signed record** | OCPP's numbers are telemetry |
| When the transaction opened and closed | the **OCPP events** | OCMF's clock is often `I` — informative — and the CSMS knows when it authorised |
| Charging, or merely connected | the **OCPP events** | the meter cannot tell a taper from an occupancy, and `[AFIR Art. 5(4)]` prices them differently |
| Whether a reading is clock-aligned | the **OCPP `ReadingContext`** | nothing in the record says why it was taken |

So `Transaction::assemble` takes the *shape* of the session from the protocol
and every *number* from the signature.

## What it does not do

It does not speak OCPP. [`ocpp-kit`](https://github.com/hupe1980/ocpp-kit) does
that — framing, the sans-I/O engine, transports, security profiles, and the
version-neutral `DomainEvent` this crate reads. `emob_ocpp::kit` is a `match`
over that event and nothing more.

It also does not verify. `Transaction::assemble` gets the bytes out of the
transport intact; `emob_eichrecht::Evidence::assemble` decides whether they hold
up, against a registry.

## Why this is a crate

It holds one job, and it is a boundary rather than a quarantine. Folding it into
`emob-cdr` would put **`ocpp-kit` in the dependency graph of every
crate that decides money**. Today `emob-core`, `emob-session`, `emob-eichrecht`,
`emob-tariff` and `emob-cdr` build with no OCPP anywhere in their tree, and this
crate is the reason: it is the only one on both sides.

The billing chain should be buildable, testable and auditable without a protocol
implementation in it, and a boundary the compiler enforces is the only kind that
stays true.

## No I/O, no clock

Nothing here opens a socket, reads a file or asks the time. `just purity` fails
the build if that stops being true.

## License

MIT OR Apache-2.0.
