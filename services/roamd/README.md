# roamd

The roaming node: **who** a record goes to, **who may send one here**, **when**
it is late, and **what to do** with one that arrives — and never what a record
says.

```console
cargo run -p roamd
```

## What it decides, and what it must not

Every document is `emob-roam`'s: `for_partner` carries a canonical record onto
the OCPI version a peer speaks, `to_oicp` onto Hubject's wire where it carries
no money at all, and `from_ocpi` reads a partner's document back **unpriced**.
One signed session settles at the same money over three paths.

None of that is here, and none of it could be: a document is what a domain crate
says it is. What is here is the half a domain crate cannot have — a **ledger of
what was sent to whom and what came back**, the policies an operator wrote about
the partners it peers with, and the sockets either side of both.

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

**An acceptance is terminal.** The seal is enforced where a record is consigned,
by reading the delivery — which made it exactly as durable as that field: one
`refused` recorded after the fact overwrote the acceptance, and the same record
could then go to the same partner again. Recording the same outcome twice is
idempotent, because a location that arrives late is a fact about one delivery
rather than a second one.

**And a reversal is owed only where one happened.** A Credit CDR reverses a
record the eMSP **has**. For one they refused there is nothing to reverse and no
credit will ever be accepted, so requiring one before every replacement
deadlocked the correction of every refused record — against the rule this section
states two paragraphs up, that nothing refused is sealed.

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

## 4. Who may send one here

A partner is onboarded while the node runs, and what an operator wants to say
about one is not a set of dotted capability patterns:

```cedar
@id("nl-tnm-trial")
permit (
  principal == emob::Party::"NL*TNM",
  action == emob::Action::"emob.cdr.write",
  resource
)
when { resource.owner == principal }
unless { context.at > datetime("2027-01-01T00:00:00Z") };
```

```rust
let node = Roamd::new(registry).with_policies(Gate::new(Policies::load(text)?));
node.reload_policies(&edited)?;   // …while it runs, and never with nothing
```

The policies are `emob_service::policy`'s and the schema is generated from the
capability list, so this service decides *when* a grant is consulted and never
what one may say. Three things about the composition are this node's:

**It is asked before the document is read.** The other five answers are
information: telling a caller this node does not accept which of its charging
periods do not sum is telling it about a session it has no business knowing
exists. So `Verdict::Unauthorised` is a sixth answer and it comes first.

**A policy may grant, and it may never widen a delegation.** The typed ceiling in
`emob_service::authority` is asked first and a `no` there is final, so a peer
pushing another party's record is refused whatever any policy says — and the
refusal names the ceiling rather than the policy, because that is the half no
text can turn off.

**And a reload that does not validate leaves the running set deciding.** Default
deny makes an empty policy set a total outage rather than a degraded state, so a
failed reload must not be able to produce one. A node that was never given a gate
refuses to load policies at all, because a set nothing consults is a grant nobody
is enforcing.

## 5. What to do with one that arrives

**Four** gates, in the order that makes them mean something — the **credential**
(above), the **document** (`preflight`), the **conversion** (`from_ocpi`), the
**record** (`validate`) — and six answers rather than one error:

| Verdict | Means | What the sender does |
|---|---|---|
| `Unauthorised` | this credential may not push this record here | asks the operator |
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

## 6. …and the socket

`dispatch::Dispatcher` pushes the record to the platform that registered for the
party the contract names:

```rust,ignore
let answer = dispatcher.deliver(&registration, &document).await;
node.answered(&consignment.key, &answer, now)?;   // …and only then
```

**What goes out is what the outbox holds.** `for_partner` produces the partner's
copy *and* the account of what carrying it there cost, by JSON Pointer into that
copy. The document is rendered once, when the record is routed, and sent
verbatim — a second rendering at send time, from a session that may since have
been re-rated, puts a document on the wire the account does not describe (D324).

**A push has three outcomes, not two.** A receiver that answered has *seen* the
record: it leaves the queue and a person reads the reason. A connection that
died is not an answer, so `Answer::Unreached` records **nothing** and the next
drain sends it again — safe because `[OCPI 2.3.0 §mod_cdrs]` keys a CDR and makes
the receiver treat the second copy as the first (D325).

**There is no peer list.** Who this node can reach *is* the set of platforms
registered with it, read from the journalled state each tick. A second map
beside it is a second answer to *where does a record go*, stale the moment a
token is rotated (D330).

## 7. …and the module OCPI makes mandatory

> *The Credentials module is symmetric and has to be implemented by all OCPI
> implementations.* `[OCPI 2.3.0 §credentials]`

A node that can push and cannot be **registered with** is not an OCPI
implementation. `inbound::router` serves `versions`, `credentials` and the CDRs
receiver. Four things about it are decisions:

- **The reach-back is a precondition.** A `POST credentials` is not answered
  until this node has read the client's versions *with the token it was just
  handed* — `3001` when it cannot. Skipping it leaves a credential for a
  platform this node cannot reach, which looks like a registration and fails the
  day a record is owed.
- **A registration is journalled before the peer is told.** The tokens *are* the
  authentication; forgotten, every partner ever onboarded gets a `401`.
- **The principal comes from the credential, never from the document.** Built
  from the document, the question *may this caller write for the party this
  document names* compares a claim with itself and passes for everything (D332).
- **The path, the advertised URL and the `Location` are one computation.** Work
  any of them out separately and this node answers on paths nobody was told
  about, or returns a `Location` that 404s — every byte well-formed, nothing
  working (D333).

The tests run the handshake from the other side through the kit's own client,
and read every URL out of this node's own version-details document, because a
test against paths this workspace chose would pass while a real peer failed.

## Where the answers are kept

Every rule above is about the past — who is owed this, whether it was sent, what
came back — and `[OCPI 2.3.0 §mod_cdrs]` seals a record the moment the eMSP has
it. The seal is exactly as durable as the place the answer is kept: a node that
forgot its deliveries answers *"nobody has this"* to every question and re-opens
every record it had already sent.

`Node` is `Roamd` folded from a hash-chained journal — `consigned`,
`delivered`, `received`, `offered`, `registered`, `unregistered`. The **ledger**
and the **outbox** are journalled; the partner registry and the operator's
policies are not, because those are configuration an operator edits and a node
that replayed an old one would route today's records by yesterday's contracts.

`Node::accepted` is called with the `Location` the receiver returned and only
then, so a delivery that failed stays pending across a restart rather than
looking sent.

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
| `EMOB_JOURNAL_DIR` | `var` | where the journal is folded from and appended to. **Not** a temporary directory by default: a service that silently journalled to `/tmp` would come back from a reboot with an empty ledger and no error |
| `EMOB_HTTP_BIND` | — | the estate-wide bind address, which `ROAMD_HTTP_BIND` wins over. A container image sets it, because every default here is **loopback** and loopback inside a container is reachable by nothing |

## License

MIT OR Apache-2.0.
