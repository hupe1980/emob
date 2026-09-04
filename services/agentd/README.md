# agentd

**The advisory plane for emob**, on [`agentplane`] — specialists that correlate
across many exact answers, and which **cannot move money, by construction**.

[`agentplane`]: https://github.com/hupe1980/agentplane

## The question nothing else in the workspace answers

Every crate below this one answers a question about **one** thing: is this chain
sound, does this record add up, is this tariff lawful at this power. Those
answers are exact, and they are the ones that decide money.

None of them answers the question an operator actually has at eight in the
morning, which is a question about a **population**:

> Four hundred sessions were refused overnight. Is that four hundred support
> tickets, or one meter?

```text
[evidence-triage] BQ27400330016: the meter was in state Substitute at record 2,
                  which may not be billed
                  (3800.000 kWh at risk across 380) — that is a device fault
                  rather than a dispute: raise it with the station vendor…
[evidence-triage] BQ99999999999: pagination jumped from 4 to 6
                  (20.000 kWh at risk across 20) — records are missing from the
                  middle of these sessions…
```

The same shape for tariffs. `check_afir` decides whether one tariff is lawful at
one power; what it cannot say is that the **same** tariff is an ordinary product
on the 22 kW posts and a breach on the two 150 kW cabinets beside them — because
`[AFIR Art. 5(4)]` binds a tariff *at the power the point offers it at*, and no
tariff document says which points those are.

## Advisory only, and it is a property

An agent **proposes**. The invariants decide. Written down, that is a promise
somebody has to keep; two things make it structural instead.

**The output type is a leaf.** `Advice` carries observations and a suggestion for
a human. It has no method returning a `Cdr`, a `Tariff`, an `Invoice` or a
posting, and nothing in this workspace consumes one — so there is no path from an
agent's answer into a document. A reviewer checks that by looking at what
`Advice` can be turned into, which is nothing.

**The principal cannot hold a write capability.** A specialist runs under a
principal derived by `emob_service::Principal::attenuate`, which refuses to
widen, and `advisory()` is the only constructor:

```rust
let agent = advisory(&operator)?;
agent.may(caps::CDR_READ);       // true
agent.may(caps::INVOICE_WRITE);  // false — and a test asserts every write case
```

An agent that wanted to issue an invoice would have to be given a principal that
could, and the constructor is the place that would have to change.

## Ranked by a quantity, never by a score

A triage that returns "high, medium, low" has invented a scale. The quantities
here are the workspace's own — kilowatt-hours that cannot be billed, money a
partner has not accepted, charge points in breach — so an operator comparing two
findings is comparing the units they will be asked about.

Two findings in **different** units are grouped rather than compared: there is no
exchange rate between a kilowatt-hour and a euro that this daemon is entitled to
invent.

### …and a kind is not a group

The refusal goes one level further than the kind. Ordering every `Money` by its
amount puts €100 and CHF 100 in one queue and invents the exchange rate the
paragraph refuses; `400 sessions` above `5 charge points` says four hundred
sessions matter eighty times more than five dead posts.

So the group is the **unit** — the currency for money, the counted noun for a
count, kilowatt-hours for energy — and each unit is its own queue:

```text
[settlement] 500 CHF …     [triage] 9 charge points …
[settlement]  10 CHF …     [triage] 5 charge points …
[settlement] 900 EUR …     [triage] 400 sessions …
[settlement] 100 EUR …     [triage]  12 sessions …
```

A magnitude means nothing without its unit, and two magnitudes are comparable
only when the unit is the same one.

## Written as code, not as a prompt

Every specialist is a deterministic function over data a daemon already holds.
None calls a model, and that is the design rather than a stage on the way to one:
these questions have exact answers, and an answer that varied between two runs of
one input would be useless in the queue it lands in and indefensible in the
dispute it feeds.

What `agentplane` provides for a function like that is not inference — it is the
**journal**. The run, its input, its answer and every effect go into an
append-only hash-chained log, and a replay re-executes the logic while reading
each effect back rather than performing it again:

```rust
let outcome  = runtime.run("evidence-triage", Tainted::trusted(refusals)).await?;
let replayed = runtime.replay(outcome.run_id, Mode::Strict).await?;
assert_eq!(outcome.output, replayed.output);
```

"Why did the queue say that in March" becomes a replay instead of an argument —
and for a pure function the replay is exact. A specialist whose work genuinely
needs a model, such as reading a partner's free-text dispute, is a manifest
rather than a module, and it goes through the same `Advice` leaf.

## The subscription table is data, and it is checked

A subscription written as a literal where the dispatch happens is one nobody can
list, and a typo in it silently matches nothing — which looks exactly like a
specialist with nothing to say.

So it is one `const`, its patterns go through `emob_service::events::matches`,
and two tests hold it down: every pattern must match at least one type in the
catalogue, and every specialist named must be one the daemon registers.

Not the reverse. Most event types are for a webhook subscriber rather than for a
specialist, and `de.emob.billing.invoice-issued` reaching no agent is correct —
so an unsubscribed type is a reading rather than a build failure. It was worth
reading once: the whole `compliance.*` family sat in the catalogue with no
subscriber, which is how the sweep below came to be written.

## What is here

| Specialist | Wakes on | Answers |
|---|---|---|
| `compliance-sweep` | `de.emob.compliance.duty-commenced`, `…breach-detected`, `…notice-window-opened` | which duties this estate fails, and **which it will fail on the day they start** |
| `evidence-triage` | `de.emob.evidence.refused`, `…key-unresolved` | which one fault caused most of today's unbillable energy |
| `tariff-review` | `de.emob.tariff.version-published`, `…conformance-failed` | which points offer a tariff `[AFIR Art. 5(4)]` forbids at their power |

### The one that reads the calendar forwards

`emob_core::obligation::assess` judges one point on one date, and the sweep does
not have a second implementation of any rule — it calls it, once per point. What
it adds is the two things one assessment cannot say.

**Which duty, across how many points.** An operator with four hundred posts does
not have four hundred compliance questions; it has the handful of duties its
estate fails and the list of posts under each. One firmware programme, one
contract clause, one retrofit.

**And the breaches that have not started.** Every entry carries the date it
begins binding, so a duty that is `NotYetInForce` today is judged *at its own
commencement date* against the estate as it stands. A point renovated in March
2027 that speaks only EN ISO 15118-2 is compliant today and in breach on new
year's day — and that is the only compliance advice that arrives before the
breach rather than after it.

```jsonc
{ "on": "2026-09-01", "points": [ /* the inventory */ ], "horizon_days": 540 }
```

```console
[compliance-sweep] 400 point(s) fail `afir-digitally-connected` [AFIR Art. 5(7)]
                   → connect the point to a CSMS: without it neither the data
                     nor the smart-charging duties can be met
[compliance-sweep] 12 point(s) will fail `da656-iso15118-20-public`
                   [DA-656 Anh. 2.1.2] from 2027-01-01
                   → TLS 1.3 and the larger certificates of -20 usually mean a
                     hardware refresh
```

The suggested action is the calendar's own `remedy`, never a second wording of
it. The horizon is the caller's — a budget cycle wants eighteen months, a
hardware refresh wants everything — and the date is an argument rather than a
clock, so a run in March replays to the same answer in September.

And a **forgone entitlement is not worded as a breach**: the greenhouse-gas
quota is money, not law, so an estate that declines it reads "lawful, and worth
money" rather than "fail". A queue that cries wolf about a subsidy is a queue
that teaches its reader to discount the findings that matter.

## Licence

MIT OR Apache-2.0
