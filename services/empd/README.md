# empd

The provider side of a contract: the token store two domain crates deliberately
refuse to be, whether a contract authorises a session **now**, what the driver is
quoted **before** it starts, and the fee that is owed **whether or not** they
charged.

```console
EMPD_TOKEN_KEY=… cargo run -p empd
```

## The service `emob-roam` asks for by name

> A `RoamingToken` is **presented to the crossing** by the party that holds the
> mapping — the token store, which is a service with a key and a database and no
> place in a domain crate.

`emob_session::Authorization` refuses to store an RFID UID: a UID is a lifelong
identifier of a physical object a person carries, and a session row holding one
builds a movement profile nothing in this platform needs. OCPI requires that
same UID on every outgoing CDR. Those two facts do not compose inside a crate,
and the resolution was never to weaken either.

```rust
let reference = provider.issue_token(&contract, "04A1B2C3D4E5F6", TokenType::Rfid, Whitelist::Allowed)?;
reference.as_str();                 // 64 hex characters, and not the uid
provider.present(&reference)?.uid;  // the uid, at the edge that has to send it
```

The digest is **keyed**, so a second provider holding the same card computes a
different reference. And the contract identifier's check digit is verified here
rather than at the crossing: `emob-roam` checks it *"because here is the last
place anyone looks"*, and this is one place earlier — the difference between
refusing a card at the counter and discovering three weeks later that a month of
sessions was billed to somebody else's contract.

## 1. Whether a contract authorises this session, now

`[OCPI 2.3.0 §mod_tokens]` asks a real-time question and takes one of five
answers. Four of them need something no document has.

| Answer | What it needs | Why no crate can give it |
|---|---|---|
| `Blocked` | the token's standing | a lost card is a fact about a store |
| `Expired` | a **clock** against the contract's window | a contract is a relationship over time |
| `NoCredit` | a **ledger** of sessions nobody has invoiced | the sessions are precisely the ones with no document |
| `NotAllowed` | the location | this point, not this driver |
| `Allowed` | all of the above | — |

A contract that runs *until* the last of the month covers a session on that day.
Inclusive at both ends, because that is what a driver reading their own contract
expects.

### The whitelist is a two-sided rule

`ALWAYS` tells an operator to start from its own list and **never ask**; `NEVER`
says always ask. Both directions are refusals here, and each of them is a session
somebody is not going to be paid for:

- an operator that asks about an `ALWAYS` token has a **stale list**, and the
  sessions it is *not* asking about are being started from it — including for
  tokens this provider has since blocked;
- a session started from a list for a `NEVER` token was never authorised, and its
  CDR arrives with nobody to bill.

Neither shows up as an error anywhere else in the system.

## 2. What the driver is quoted before they plug in

`[AFIR Art. 5(5)]` binds the *provider* rather than the operator:

> Mobility service providers shall make available to end users, prior to the
> start of an intended recharging session, all price information specific to that
> recharging session … clearly distinguishing all price components, including
> applicable e-roaming costs and other fees or charges applied by the mobility
> service provider.

The operator's components are `emob_tariff::describe`'s, in the order
`[AFIR Art. 5(4)]` prescribes, because a provider has no business restating
somebody else's price in its own words. This service's own charges follow, each
named — and the e-roaming cost is its own field, because the article names it
separately from "other fees or charges".

```rust
quote.charged_by(ChargedBy::Operator);          // their price
quote.charged_by(ChargedBy::ProviderERoaming);  // the cost the article names
quote.passes_the_operators_price_through(&tariff, at);
```

**That last one is the article's substance.** The fold it forbids is not an
unnamed component — it is a provider that adds five cents to the operator's price
per kilowatt-hour and shows the driver one number. Every component is still
"clearly distinguished", and two providers quoting one point give two different
accounts of the same operator's price. So the test is not that the components are
named; it is that the ones attributed to the **operator** are the ones the
operator published.

### A price list with no country in it cannot surcharge the border

The same paragraph ends: *"Mobility service providers shall not apply any extra
charges for cross-border e-roaming."* Not "reasonable and transparent" — forbidden
outright.

So a `Markup` belongs to a **partner**, and there is no country anywhere in the
type. Charging two partners differently is an ordinary commercial difference —
their roaming costs differ — and the point's country never reaches the
arithmetic.

That is what lets this service **derive** what `emob_core::ProviderProfile` takes
as four booleans somebody ticked:

```rust
let assessment = assess_provider(&provider.provider_profile(), today);
```

Three of the four are facts about this service's own data; the fourth is a fact
about the type. A provider that has stated no price list at all discloses
nothing, and the calendar says so rather than assuming the best.

## 3. The fee that is owed whether or not anybody charged

C-60/23 (*Digital Charging Solutions*) turns on a fixed fee charged *"regardless
of whether the user actually purchased electricity during the relevant period"*,
which is why the Court held network access to be a **separate and independent**
supply of services.

A month's invoice is assembled from records. A contract with no sessions produces
none — so the one line that is owed anyway is invisible to everything downstream
of the ledger.

```rust
provider.fees_for(date!(2026 - 06 - 01), date!(2026 - 06 - 30));
```

Derived from the **contracts in force**, not from the records. Each is an
`emob_billing::Subscription`, which is the line `billd` puts on the document and
taxes under `[UStG §3a(1)]` rather than `[UStG §3g]`.

## No I/O

Nothing in the library opens a socket or reads a clock. Every date and instant is
an argument, so two runs of one authorisation give one answer.

## Configuration

| Variable | Default | What |
|---|---|---|
| `EMPD_HTTP_BIND` | `127.0.0.1:9585` | the readiness and health endpoint |
| `EMPD_TOKEN_KEY` | *required* | the key the token store is hashed under |

`EMPD_TOKEN_KEY` has no default on purpose. A default is a store whose digests
anybody holding this source can recompute, which is the property `TokenRef`
exists for.

## License

MIT OR Apache-2.0.
