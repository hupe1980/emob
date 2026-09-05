# roamd

The roaming node: **who** a record goes to, **when** it is late, and **what to
do** with one that arrives — and never what a record says.

```console
cargo run -p roamd
```

## What it decides, and what it must not

Both crossings were built and proven before this service existed.
`emob_roam::ocpi::cdr::for_partner` carries a canonical record onto the OCPI
version the registry records a peer as speaking; `emob_roam::oicp::cdr::to_oicp`
carries it onto Hubject's wire, where it carries no money at all;
`emob_roam::ocpi::from_ocpi` reads a partner's document back, **unpriced**. One
signed session already settles at the same money over three paths.

None of that is here, and none of it could be: a document is what a domain crate
says it is. What is here is the half a domain crate cannot have — a **ledger of
what was sent to whom and what came back** — and the four questions that need
one.

## 1. Who is owed this record

Routed out of the **contract identifier's own issuer**, not a map somebody
maintains. A direct peer claiming that namespace wins; failing that, a hub;
failing that, it is refused, because a record sent to a party that never had the
driver is settlement money leaving for the wrong company.

```rust
let consignment = node.consign(&cdr, &token)?;
consignment.reach;      // Direct(NL*TNM), or Hub { hub, issuer }
consignment.wire;       // OCPI or OICP — a field on the partner, not an inference
```

**The wire is a field because it cannot be inferred.** Hubject is a hub *and*
speaks OICP; GIREVE is a hub and speaks OCPI. A service that read the protocol
off `Role::Hub` would hand a broker a document it parses none of, so
`Roamd::prepare` refuses a consignment owed on the other wire rather than
building it.

**And the two ways a record cannot be routed are two messages.** A contract
naming a provider nobody peers with wants a *partner* added; one in an eMSP's own
scheme — which OCPI permits — wants that partner's *namespace* declared. The
registry answers `None` to both, and an operator told the first when it is the
second goes looking for something that is not missing.

## 2. Whether it may be sent at all

`[OCPI 2.3.0 §mod_cdrs]` seals a record the moment the eMSP has it:

> Because a CDR is for billing purposes, it cannot be changed or replaced once
> sent to the eMSP. Changes are simply not allowed. Instead, a Credit CDR can be
> sent.

So consigning a record a partner has already accepted is refused **by name**, and
the correction has an order the same paragraph gives: *"the CPO has to send a
Credit CDR for the first CDR … **after having sent the Credit CDR**, the CPO can
send a new CDR"*. A replacement consigned before its reversal was accepted is
refused, because sent the other way round the partner holds two records for one
session and settles both until somebody notices.

That rule cannot live in a crate. `to_ocpi_credit` builds the reversal,
`emob_cdr` refuses to bill both halves, and `Cdr::supersedes` names what a record
replaces — three layers modelling the correction, and none of them knows whether
the original ever left the building.

## 3. Which records are late

Against the window **that partner** agreed to, because the same paragraph makes
the cadence a contract rather than a protocol rule:

> there is no requirement to send CDRs in (semi-) realtime, it is seen as good
> practice to send them as soon as possible. But if there is an agreement between
> parties to send them, for example, once a month, that is also allowed by OCPI.

A node peering with a monthly settler and a same-day settler therefore has two
answers to one question, and a single constant would report the first in breach
every day. `Partner::settles_within` is the figure, and `Roamd::unsettled` is the
sharp question: not work outstanding, but a session that has been delivered,
settled on this side, and never billed to anybody.

## 4. What to do with one that arrives

Three gates, in the order that makes them mean something — the **document**
(`preflight`), the **conversion** (`from_ocpi`), the **record** (`validate`) —
and four answers rather than one error:

| Verdict | Means | What the sender does |
|---|---|---|
| `Accepted` | the document holds up and the record is new | nothing; settle it |
| `Disputed` | the document is fine and the claim does not hold here | talks to you |
| `Duplicate` | already held, unchanged | nothing; the retry was harmless |
| `Conflicted` | already held, and this one differs | a human answers |
| `Rejected` | the document does not answer OCPI's own questions | fixes it and re-sends |

`Rejected` and `Disputed` are deliberately not one error. *"Your document is
wrong"* is a retry; *"your document is fine and your claim does not hold on our
side"* — signed records that do not verify against **our** registry, minutes
charged that the record's own periods do not account for — is a conversation
between two companies, and a partner told the first when it is the second retries
something no retry fixes.

A record that is disputed is **not** in the ledger: it has not been settled.

### …and then it is worth two numbers, not one

```rust
let settlement = node.settle(&inbound, &retail)?;
settlement.owed_to_partner;   // what the CPO's document states
settlement.owed_by_driver;    // what this side's retail tariff makes of it
settlement.margin()?;         // and the difference
```

Re-rated through `Cdr::rerated_with` — the same door the issuing side prices
with — because reaching for the rating engine directly silently skips every gate
the issuer applied: a retail tariff that was not in force when the session ran, a
version the meter says was superseded mid-session, a duration the signed records
do not vouch for, and the clock resolution `[REA 6-A §3.1]` puts under a
per-minute fee.

## No I/O

Nothing in the library opens a socket or reads a clock. `prepare` returns the
document and the daemon sends it; `accepted` records a delivery that **succeeded**
— with the URL the receiver returned, which `[OCPI 2.3.0 §mod_cdrs]` makes
mandatory on the response precisely so the sender can fetch back what it sent. A
push that failed leaves the consignment pending, so it turns up in `unsettled`
rather than being forgotten, which is the whole reason recording a delivery is a
separate act from attempting one.

The transport is the one leg CI cannot run: a peer is somebody else's server
behind a credentials exchange and Hubject is mutual TLS under a contract, exactly
as `csmsd`'s WebSocket and the Mobilithek subscription are.

## Configuration

| Variable | Default | What |
|---|---|---|
| `ROAMD_HTTP_BIND` | `127.0.0.1:9583` | the readiness and health endpoint |

## License

MIT OR Apache-2.0.
