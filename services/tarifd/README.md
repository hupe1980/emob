# tarifd

**The service that publishes a tariff version.** It decides *when* a price takes
effect, and never *what* it says.

## The three audiences

A charge point's ad-hoc price is owed to three parties, and each is a duty with
its own citation:

| Audience | Wire | Duty |
|---|---|---|
| the driver at the point, **before** they start | OCPP 2.1 `SetDefaultTariff` | `[AFIR Art. 5(4)]` |
| the roaming partner that will settle against it | OCPI 2.3.0 | the contract |
| the national access point | DATEX II | `[AFIR Art. 20(2)(c)]` |

Almost every stack computes that price three times, in three systems, and
reconciles none of them against the invoice. This one computes it **none** times:
each payload is built by the crate that already owns the crossing — `to_ocpp`,
`ocpi::tariff::to_ocpi`, `rate::publish` — from the one `Tariff` that rates the
CDR. There is no second computation for the three to drift from, and a test
asserts the consequence rather than the intention.

## A tariff id is a name, and this service publishes content

`prepare` refuses a tariff whose **content** no version of that id has.
`TariffHistory::in_force_at` is what rates a CDR, so an edited object that never
entered the history is a price no session will ever be billed at — and
publishing it would put that price in front of the driver, the national access
point and every roaming partner:

```rust
tarifd.prepare(&edited, &party, at)?;
// Err: tariff ad-hoc is published by this service and no version of it has the
//      content 9f2c…: publishing it would quote a price no session is rated at
```

The rule is `emob_cdr::Cost`'s, which carries a fingerprint beside the id because
*"a tariff id is a name, and names get reused"*.

## Publishing is not what makes a version effective

The version in force is decided by its own window; a CDR is priced with whichever
version covered the instant the session started. Publication is a **duty about**
that fact, and the duty runs *ahead* of it — `[AFIR Art. 5(4)]` requires the
price to be "known to end users **before they initiate** a recharging session".

So a publication that goes out when a version takes effect is already late for
everybody standing at a point at that instant: they were shown the old price and
will be billed the new one. Two questions, therefore, and they are different:

```rust
tarifd.due(now, lead);   // work: takes effect soon, not everyone has been told
tarifd.late(now);        // breach: in force *now*, and somebody was never told
```

The second names the article it breaches, because it is not a backlog item — it
is a price the estate is charging that a driver was never shown.

## All three audiences, or none

`prepare` builds every payload before any is sent, and fails as a whole. OCPP 2.1
**refuses** a tariff it cannot state without widening the price against the
driver — an hourly rate with no exact per-minute spelling, a dimension at two VAT
rates. Publishing the other two anyway would leave the national access point and
every roaming partner quoting a price the estate's own stations do not charge,
and a driver comparing on a map would be misled by a document this operator
signed off. That is worse than publishing nothing.

## Recording a delivery is a separate act from attempting one

The library opens no socket. `prepare` returns the payloads, the daemon sends
them, and `confirm` records what came back **accepted**. A push that failed
leaves that audience unconfirmed, so the version turns up in `late` the moment it
takes effect rather than being forgotten.

Versions are keyed by **content**, not by id: a redeployment of the same numbers
is the same publication, and an edit under the same id is a new one nobody has
been told about — the same argument `Cdr::was_priced_with` makes one layer down.

## No I/O in the library

Everything above is a pure function of a `TariffHistory`, a clock reading and a
record of deliveries. The daemon holds the sockets and the schedule.

## License

MIT OR Apache-2.0.
