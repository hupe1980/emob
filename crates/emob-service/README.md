# emob-service

**The shell every emob daemon shares** — configuration, structured logging, a
readiness probe that does not lie, graceful shutdown — and the three pieces that
are about charging: an **OCPI-party authority model**, the CloudEvents
catalogue, and one webhook signature.

```console
cargo add emob-service
```

📖 The reasoning behind the daemons this carries is in
**[Architecture](https://hupe1980.github.io/emob/docs/architecture/)**.

## The worst thing a roaming node can do

It is not losing a session. It is serving party A's CDRs to party B — a
competitor's charging volumes, its tariffs, and its drivers' movements, out of an
endpoint that answered a perfectly valid token.

That is not a deployment detail to be added later behind a reverse proxy, because
the proxy does not know which party owns a record. Ownership is a field on the
record, and the check belongs where that field is in scope.

```rust
let peer = Principal::peer(theirs, Role::Emsp, Capabilities::of([caps::CDR_READ]));

peer.may_act_for(caps::CDR_READ, &theirs);   // true
peer.may_act_for(caps::CDR_READ, &ours);     // false — a valid token, somebody else's record
peer.may_act_for(caps::CDR_WRITE, &theirs);  // false — reaching it is not writing it
```

Three questions, kept apart: a credential answers **who** (a constant-time token
comparison), a `Capabilities` set answers **what**, and a `PartyScope` answers
**whose records**. Collapsing any two produces the same bug — an endpoint that
checks the token and not the ownership, which is every roaming data leak there
has ever been.

### Capabilities, not roles — because an agent has to hold less

`mako`'s authorisation is built on Marktrollen because `mako` *is* a market
participant; `hems`'s is built on capabilities because a household energy manager
is not. emob is a third thing: its principals are **OCPI parties**, which already
have a role — and a role is not enough.

An `agentd` specialist acting for an operator must be able to hold **less** than
that operator. `Role::Cpo` delegated to an agent is still `Role::Cpo`; a
capability set delegated is a subset:

```rust
let agent = operator.attenuate(
    Capabilities::of([caps::CDR_READ, caps::EVIDENCE_READ]),
    PartyScope::just(&party),
)?;                                   // None if it would widen either axis
```

Two pattern forms and no more — `emob.cdr.read`, or `emob.cdr.*` — because
attenuation has to be decidable by **containment**, and a richer grammar makes
containment undecidable in general. A pattern widens at a segment boundary, so
`emob.cdr.*` does not admit `emob.cdrs.read`: a prefix match on characters is how
a capability grammar grows holes.

An empty set permits nothing rather than everything. The deployment where
somebody forgot the grants is exactly the one nobody would notice.

## The readiness probe that does not lie

Almost every readiness endpoint in this industry returns `200` unconditionally,
because it was written before there was anything to check. An orchestrator then
routes stations to a `csmsd` whose key registry has not loaded, and every session
for the next thirty seconds is refused for want of a key — which looks like a
fleet fault and is a deployment one.

```rust
let readiness = Readiness::new().expecting("key-registry").expecting("tariff");
readiness.is_ready();      // false, and `blockers()` says which
readiness.up("key-registry");
readiness.is_ready();      // still false — one is not all
```

A daemon that declares nothing is **not** ready: an empty set is a daemon that
has not said what it needs, which is the state to fail in rather than the state
to pass in. And liveness stays separate, because a liveness probe that failed on
a dependency makes a restart the cure for something that was never in the
process — and restarting a CSMS drops every station's socket.

## Stopping without dropping what is in flight

Killing a CSMS mid-transaction does not lose a request. It loses the
`StopTransaction` that carries the signed meter record, and the session becomes a
kilowatt-hour nobody can bill.

So a daemon stops in two steps: it stops being **ready**, so the orchestrator
takes it out of rotation, and only then stops **serving** after a drain window.
The window is the caller's, because a CSMS holding a two-hour session cannot
drain it and a tariff publisher can drain in a second.

## The event catalogue is checked at compile time

One `pub const` per CloudEvents type, so a rename is a one-line change and a
subscription that matches nothing is a build failure rather than a specialist
that silently never runs.

```rust
events::cdr::ISSUED;                                  // "de.emob.cdr.issued"
events::matches("de.emob.cdr.*", events::cdr::ISSUED) // true
```

Every type's last word is a **past participle**, and a test enforces it: a type
in the imperative is a command wearing an event's clothes, and a subscriber
cannot tell the difference at runtime. That test renamed three entries in this
catalogue before it shipped.

## One webhook signature

[Standard Webhooks]: HMAC-SHA256 over `{id}.{timestamp}.{payload}`, base64, in a
header that can carry several signatures at once so a secret rotates without a
flag day.

[Standard Webhooks]: https://www.standardwebhooks.com

```rust
let header = webhook::sign_with(&delivery, [&outgoing, &incoming]);
webhook::verify(&delivery, &[outgoing], &header);   // the old end still accepts
```

Signing reads **no clock** — the instant is an argument — so a delivery replayed
from an outbox is byte-identical to the first attempt, which is what lets a
receiver de-duplicate on the signature. The freshness tolerance is the caller's:
five minutes is right for an interactive callback and wrong for an overnight
batch, and a library that picked one would be picking it for both.

### …and the secret's encoding is stated, never inferred

Standard Webhooks writes a secret as `whsec_<base64>`, and some deployments
configure a passphrase instead. A constructor that stripped the prefix, tried
base64 and fell back to the raw bytes looks accommodating and is **ambiguous for
exactly the secrets that look like base64**: `"mysecret"` is eight ASCII bytes
*and* a valid base64 string, so it silently becomes six arbitrary ones, while
`"hunter2"` is not and stays as it is.

Nothing about a configured value tells an operator which they will get, and a
sender using the literal bytes and a receiver decoding them disagree on every
delivery — with `SignatureMismatch` as the only diagnostic, which points at the
payload rather than at the key.

```rust
Secret::standard("whsec_bXlzZWNyZXQ=")?;   // the specification's spelling
Secret::raw("bXlzZWNyZXQ=");                // the bytes as written
// …and the same string is a different key under each, which is why
// neither is a default.
```

## Why this is not `mako-service`

Extracting it was considered and rejected, for the reason `hems-service` gives
and one more. `mako`'s authorisation is built on **Marktrollen** and its OIDC
layer carries a `Sparte` grant; its Cedar schema is those roles. emob's
principals are OCPI parties scoped by the party that owns each record, and the
check that matters is one `mako` has no reason to have: *may this credential
reach a record this other company owns*.

What was left after removing the market model was five domain-free modules, and
copying five domain-free modules is cheaper than maintaining a diff guard against
a fork that is *supposed* to diverge.

## Sans-I/O ends here

Every domain crate in this workspace takes its instants as parameters and opens
no socket, so a dispute two years old is answered by replaying the check exactly
as it ran. This crate is where that stops being true, and it is the only shared
place it does.

Two things stay pure even here: `webhook::sign` takes the instant it signs, and
`authority` reads no clock at all — so a credential's reach is a property of the
credential rather than of when it was asked.

## Licence

MIT OR Apache-2.0
