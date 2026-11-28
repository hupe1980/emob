# roamd

The roaming node: decides who a charge detail record (CDR) goes to, whether it may be sent, when it is late, who may push records here, and what to do with one that arrives — over OCPI 2.3.0/2.2.1 and OICP 2.3.

A replacement CDR receiver can use the event schema, signed headers and acknowledgement rules in the [integration OpenAPI document](../../schemas/openapi.json).

## Status

| | |
|---|---|
| ✅ library | `roamd::Roamd` and `roamd::Node` implement routing, the OCPI CDR seal and correction order, per-partner settlement windows, Cedar-gated inbound verdicts, settlement against a retail tariff, the OCPI push client (`roamd::Dispatcher`) and the OCPI `versions`/`credentials`/CDRs receiver (`roamd::inbound::router`) — tested in `tests/a_record_reaches_its_partner.rs` and `tests/the_ledger_holds_under_any_order.rs` |
| ✅ journal | on start the daemon folds its hash-chained journal (`roamd.ndjson` under `EMOB_JOURNAL_DIR`): consignments, deliveries, received records and registrations. Record bodies are sealed under `ROAMD_DATA_KEY`, because consignments carry the CDR's token UID; a journal sealed under another key refuses to start. Every record names its actor; the head is signed under `ROAMD_SIGNING_KEY` after every append, and a journal shorter than its signed head refuses the start |
| ✅ OCPI | the inbound OCPI router is mounted at `/ocpi` (versions, credentials, CDRs) behind the reach-back guard. Partners and their Token A come from the estate's `[[partners]]`; Token A and Token C are held as keyed digests, Token B sealed; registration tokens are 32 random bytes |
| ✅ deliveries | `POST /v1/deliveries` takes the `de.emob.cdr.issued` CloudEvents `csmsd` emits (signed, fresh, from an estate partner) and consigns each to the contract's issuer with the estate station's location and the verbatim OCMF records; a drain loop pushes them |
| ⏳ not wired | the OICP client (Hubject) — the library builds OICP documents, the daemon does not send them |

Every document is `emob-roam`'s (`for_partner`, `to_oicp`, `from_ocpi`); `roamd`
keeps the ledger of what was sent to whom and what came back, the operator's
policies about partners, and the sockets.

## Grant a sender access to its charge records

The estate advertises its operator as CPO and its provider as eMSP. An authenticated credentials GET returns the caller’s own token and the estate’s versions URL. A registered peer rotates with PUT. Repeating POST after registration, or sending PUT or DELETE before registration, returns HTTP 405.

In the platform’s `[[partners]]` entry, set `cdr_owners = ["NL*TNM", "BE*ALT"]` to approve its two sending CPOs. An empty list grants no CDR sender roles. The platform must also declare each CPO in its Credentials exchange. Registration and rotation check each claim against the estate.

POST a CDR to the advertised receiver endpoint and retain the returned `Location`. That URL identifies the owner and local CDR ID. An exact retry returns the original URL. A conflicting document leaves the first original intact. Current CPO grants also govern reads after restart.

## What it owns

### 1. Routing by the contract's issuer

A record goes to the issuer named in its contract identifier (eMAID): a direct
peer claiming that namespace first, then a hub, otherwise refused.

```rust
let consignment = node.consign(&cdr, &token)?;
consignment.reach;      // Direct(NL*TNM), or Hub { hub, issuer }
consignment.wire;       // OCPI or OICP — a field on the partner, never inferred
```

The wire cannot be inferred from the role: Hubject is a hub speaking OICP,
GIREVE a hub speaking OCPI. `Roamd::prepare` refuses a consignment owed on the
other wire (`Roamd::prepare_for_broker` builds the OICP document). An unknown
provider and an undeclared provider namespace are reported as two different
problems, because they have two different fixes.

### 2. The CDR seal and the correction order

`[OCPI 2.3.0 §mod_cdrs]`: once an eMSP has a CDR it *"cannot be changed or
replaced"*; a correction is a Credit CDR, and only *"after having sent the
Credit CDR"* a new CDR.

- Consigning a record a partner already **accepted** is refused by name. An
  acceptance is terminal; a later outcome cannot overwrite it.
- A replacement is refused until its reversal (`Roamd::credit`) was accepted —
  otherwise the partner holds and settles two records for one session.
- A record the partner **refused** needs no reversal: there is nothing to
  reverse.

### 3. Late records, per partner

OCPI leaves the CDR cadence to agreement between parties, so a monthly settler
and a same-day settler need different windows. `Partner::settles_within` sets
it, and `Roamd::unsettled` lists sessions delivered and settled here that the
partner has not billed within its own window.

### 4. Who may push here (Cedar policies)

Partner-specific grants such as *"CDRs until the trial ends"* are Cedar policies
from `emob_service::policy`:

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
node.reload_policies(&edited)?;   // while running; an invalid set leaves the old one in force
```

Authorisation is checked **before** the document is read, so an unauthorised
caller learns nothing about it. The typed ceiling in `emob_service::authority`
is checked first and its "no" is final: no policy lets a peer push another
party's record. A node without a gate refuses to load policies at all.

### 5. Inbound records: six verdicts

Four gates in order — credential, document (`preflight`), conversion
(`from_ocpi`), record (`validate`):

| Verdict | Meaning | Sender's next step |
|---|---|---|
| `Unauthorised` | this credential may not push this record | ask the operator |
| `Accepted` | valid and new | none |
| `Disputed` | the document is valid but the claim does not hold here (e.g. signatures fail against **our** key registry) | talk to us — not a retry |
| `Duplicate` | already held, unchanged | none; the retry was harmless |
| `Conflicted` | already held, different | a human decides |
| `Rejected` | fails OCPI's own checks | fix and re-send |

A disputed record is not in the ledger. An accepted one is settled to two
figures through `Cdr::rerated_with`, the same gated door the issuer prices with
(tariff in force, superseded versions, signed durations, `[REA 6-A §3.1]` clock
resolution):

```rust
let settlement = node.settle(&inbound, &retail)?;
settlement.owed_to_partner;   // what the CPO's document states
settlement.owed_by_driver;    // what our retail tariff makes of it
settlement.margin()?;
```

### 6. The OCPI push client

```rust
let answer = dispatcher.deliver(&registration, &key, &document).await;
node.answered(&consignment.key, &answer, now)?;   // record only after the answer
```

- **What is sent is what the outbox holds**: the document is rendered once, at
  consign time, together with its account of what the crossing cost, and sent
  verbatim.
- **Three outcomes**: `Answer::Accepted` (with the returned location),
  `Answer::Refused` (a person reads the reason), `Answer::Unreached` (records
  nothing; resent next drain — safe because OCPI keys CDRs so a second copy is
  treated as the first).
- **No separate peer list**: reachable platforms are the ones registered in the
  journalled state, read fresh each tick, so a rotated token is used at once.

### 7. The OCPI credentials module

`[OCPI 2.3.0 §credentials]` is mandatory and symmetric. `inbound::router`
serves `versions`, `credentials` and the CDRs receiver:

- a `POST credentials` is answered only after this node has read the client's
  versions with the token it was handed (status `3001` otherwise);
- a registration is journalled before the peer is told;
- the principal comes from the credential, never from the pushed document;
- the served path, advertised URL and returned `Location` come from one
  computation.

The tests run the handshake from the other side with `ocpi-kit`'s own client,
reading every URL from this node's version-details document.

### State that survives a restart

`Node` is `Roamd` folded from a journal of `consigned`, `delivered`,
`received`, `offered`, `registered` and `unregistered` facts. The partner
registry and the policies are configuration, not journalled, so a replay never
routes today's records by yesterday's contracts. A delivery is recorded only
after the receiver answered, so a failed push stays pending across a restart.

## The reach-back guard

During the credentials handshake a partner names a URL this node then calls.
`roamd::reach` admits only `https`, and `http` or a private, loopback or
link-local address only for hosts listed in `[roaming] reach_back_allow` — so a
registering partner cannot point the node at an internal service. The answer
it gives is early and readable; the enforcement is `ocpi-kit`'s, whose resolver
refuses an internal address on every connection and redirect hop, so a name
that rebinds after the check is refused too. Keep `reach_back_allow` empty in
production: the kit's policy cannot open private addresses for one host only.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[parties]`, `[[partners]]`, `[[stations]]`, `[roaming]` (`public_url`, `reach_back_allow`), `[services]`, `[[operators]]` |
| `ROAMD_DATA_KEY` | **required** | 32 bytes, base64; seals the journal and Token B |
| `ROAMD_SIGNING_KEY` | **required** | 32 random bytes, base64 (an Ed25519 seed); signs the journal head. A `.head` signed under another key refuses the start |
| `EMOB_PARTNER_<PARTY>_TOKEN_A`, `EMOB_WEBHOOK_SECRET`, `EMOB_OPERATOR_TOKEN`, `EMOB_DESK_READ_TOKEN` | **required** | the variables the estate names: each partner's Token A, delivery signatures, the example estate’s named operator and background read credentials |
| `ROAMD_HTTP_BIND` / `EMOB_HTTP_BIND` | `127.0.0.1:9583` | the shell: probes, OCPI, deliveries, read model |
| `EMOB_JOURNAL_DIR` | `var` | journal directory — deliberately not a temp directory |
| `EMOB_LOG_FORMAT` | JSON | `text` for a terminal |
| `RUST_LOG` | `info,roamd=debug` | log filter |

## Running

```console
just demo                     # the estate with an OCPI partner registered
EMOB_ESTATE=deploy/estate.example.toml cargo run -p roamd   # plus the *_env variables
```

Or `just up` for the whole estate (`roamd`'s shell on `localhost:8081`).

## Endpoints

| Path | Answers |
|---|---|
| `/ocpi/…` | OCPI `versions`, `credentials` and `cdrs` for registered partners |
| `POST /v1/deliveries` | a `de.emob.cdr.issued` CloudEvent from `csmsd`: Standard-Webhooks signature, 300 s freshness, size cap; a signed sample marked `emobtest` is `202` and never booked |
| `GET /v1/roamd/head` | operator token: the journal's signed head, `text/plain`, verifier key in `Emob-Verifier-Key` |
| `GET /v1/roamd/today?limit=&consignments=` | operator token: partners (registered or not), counts, and the consignments no partner has accepted yet — each list `{ total, items, older }` — the newest `limit` (default 50, max 200; anything else is `400`), with `older` the cursor for the next page |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the registry and journal are loaded; `503` listing what is missing |
| `GET /about` | name and version |

## Built on

`emob-roam` (documents and routing), `emob-cdr`, `emob-service` (shell,
journal, Cedar policies), [`ocpi-kit`](https://crates.io/crates/ocpi-kit) 0.5
(client and server). See [Roaming](https://hupe1980.github.io/emob/docs/roaming/)
and the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
