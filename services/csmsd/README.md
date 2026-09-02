# csmsd

**The CSMS a charging station connects to** — an OCPP 1.6J / 2.0.1 / 2.1
endpoint whose transaction events reach the Eichrecht chain, with the seam rule
enforced by the types it is allowed to see.

Part of [emob](https://github.com/hupe1980/emob). Not published: a service is
deployed, not depended on.

```console
CSMSD_BIND=0.0.0.0:9000 cargo run -p csmsd
```

## Two ledgers, side by side, doing different jobs

A CSMS has to answer two questions about the same traffic, and conflating them
is how a platform bills a number nothing signed.

| Question | Answered by | Meter values |
|---|---|---|
| **Did the traffic arrive?** Every event accounted for, the sequence complete, a retry recognised as a retry | `ocpp-kit`'s `csms::ledger`, fed from its version-neutral `observe` view | the numeric register — exactly right for the question |
| **What may be billed?** | `emob-ocpp` → `emob-eichrecht` → `emob-cdr` | the signed OCMF register, in exact decimal |

They run **beside** each other here, never one instead of the other, and the
type system keeps them apart: nothing in the billing path can see an `f64`,
because `emob_ocpp::TransactionEvent` has no field for one.

## One funnel, three versions

`observe_v16`, `observe_v201` and `observe_v21` all produce the same `Observed`,
and `emob-ocpp` reads the billable event out of it. So the billing path is **one
function** for all three generations — `Csmsd::bill` — and the only thing the
per-version handlers still do differently is the transaction id, because 1.6
assigns it in the *response* and therefore makes it the CSMS's problem.

An observation also carries `warnings`, and one of them decides money: a station
that sets `format: SignedData` and puts an unparseable document in the value
looks, to anything reading only the billable events, exactly like a station
sending no signed data at all. That transaction is refused **by name**, so the
operator hears about it on the first session rather than at the end of a month
of unbillable ones. The message is still answered — refusing the RPC would only
make the station retry the same broken payload.

The other warning kinds are about the numeric energy register, which is
telemetry on this side of the seam and never billed here. They belong to the
operational ledger's question, not this one.

## What is thin, and why that is the point

Everything in this crate is sockets, routing and bookkeeping. The parts that
could be *wrong* — which field holds the signed data in 1.6, whether a reading
is clock-aligned, what a `chargingState` means, whether a retry is a second
reading — live in `emob-ocpp` and are tested there, against the Open Charge
Alliance's own example messages.

A daemon is the worst place to keep a rule, because CI does not run it.

## The two bindings a station may not supply for itself

```rust
let fleet = Provisioning::new().with(
    Identity::new("CP-1")?,
    ChargePoint { evse_id: "DE*ABC*E00001".parse()?, rated_power_kw: dec("150") },
);

// …and the authenticator hands the point to the session that will use it.
.authenticate(move |auth: Auth| {
    let point = fleet.get(&auth.identity).cloned();
    async move {
        match point {
            Some(point) => AuthOutcome::Accept(SessionContext::new(point)),
            None => AuthOutcome::Unknown,
        }
    }
})
```

**Identity → charge point.** The identity in a WebSocket URL is whatever the
station was configured with. An unknown one is answered **404** rather than 401,
so an operator can tell a typo from a bad password `[OCPP 2.0.1 Part 4 §3.1.1]`
— and a station nobody provisioned never produces a session attributed to a
point that does not exist.

The binding is made **once, at authentication**, and travels with the connection
in `AuthOutcome::Accept(SessionContext::new(point))`. The handler reads it back
with `Ctx::session::<ChargePoint>()` and keeps no provisioning map of its own:
OCPP has no way to change which point a station is mid-session, so a per-event
lookup would be a second source for a fact that cannot change.

**Component → public key.** OCMF requires the key to travel out of band. A
station sends its own `publicKey` beside every signed value and offers a
`MeterPublicKey` configuration key `[OCA SMV §3.3.1]`; neither is a binding, and
a CSMS that trusted either would verify every record against whichever key made
it verify. The `KeyRegistry` comes from a type approval.

## The one piece of protocol the daemon owns

OCPP 1.6 assigns a transaction id in the **response** to `StartTransaction`, not
in the request — so the CSMS allocates it, and every later message carries it
back. 2.x has the station allocate it instead. That asymmetry needs state, which
is why it is here and not in `emob-ocpp`.

## Nothing is dropped silently

Every transaction reaches an `Outcome`: `Settled` with the record it produced,
`AlreadySettled` when the ledger already held it, or `Refused` with the reasons
the chain gave, in the order it gave them. The same rule the fleet simulator
asserts over a hundred stations — every kilowatt-hour either reaches a settled
record or is refused with a reason somebody can act on — applies to a running
daemon.

`AlreadySettled` is a **success**. OCPP guarantees delivery by retrying, so a
record the ledger already holds is the ordinary shape of a flaky link; folding
it into the refusals sends somebody to investigate a success and counts the
energy twice. Only a `Conflict` — a *different* record under a settled key —
needs a human.

A station signing with a key it was not provisioned with gets its own outcome,
separate from the refusal the chain will produce anyway. It has a **different
fix**: a meter was swapped and nobody told the registry, and every session from
that station is unbillable until somebody does. Catching it on the first signed
value rather than after a week of them is the difference between an alert and an
audit.

The tariff is checked against the point's own power before it prices anything:
`[AFIR Art. 5(4)]` is a rule about the *pairing* of a tariff with a charge
point, and a backend that rates first and checks later has already produced the
number it may not charge.

## The test that proves it

`tests/a_station_connects.rs` runs a real `ocpp-kit` `Station` against a real
`ocpp-kit` `Csms` over a real TCP WebSocket — real handshake, real subprotocol
negotiation, real RPC engine — with `csmsd` as the handler.

The station sends the OCA's own published `StopTransaction.req` from a DZG
GSH01.1K2L. Its `meterStop` says **108814** watt-hours, the meter's *lifetime*
register. The CDR that comes out bills **0.636 kWh**, the signed transaction
difference. Nothing between the socket and the ledger can see the first number.

## License

MIT OR Apache-2.0.
