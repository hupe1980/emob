# emob-service

The shared shell for emob's daemons — HTTP health and readiness endpoints, graceful shutdown, the one estate file, JSON logs with request ids, a hash-chained journal sealed under a data key whose every record names its actor and whose end is signed for a witness, the server-rendered pages — plus an OCPI-party authority model, the CloudEvents catalogue and Standard Webhooks signing.

[![crates.io](https://img.shields.io/crates/v/emob-service.svg)](https://crates.io/crates/emob-service) [![docs.rs](https://img.shields.io/docsrs/emob-service)](https://docs.rs/emob-service) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-service.svg)](#license)

Every emob domain crate is sans-I/O: it takes instants and keys as arguments and
opens no socket, so a two-year-old dispute can be replayed exactly. This crate
is the one shared place where sockets, clocks and disks come in. It also holds
the pieces every daemon needs to get right the same way: who may read whose
records, a readiness probe that reports what is missing, a shutdown that drains
in-flight work, and a journal from which a daemon's state is rebuilt after a
restart.

```console
cargo add emob-service
```

## Example

The skeleton every emob daemon's `main` follows:

```rust
use emob_service::{Identity, Readiness, Server, Shutdown, identity};

let me: Identity = identity!();
emob_service::telemetry::init(me, "info,mydaemon=debug");   // JSON unless EMOB_LOG_FORMAT=text

// MYDAEMON_HTTP_BIND, else EMOB_HTTP_BIND, else the loopback default.
let http = emob_service::serve::bind_address("mydaemon", "127.0.0.1:9599")?;

let readiness = Readiness::new().expecting("key-registry");
let shutdown = Shutdown::new();

// …load what the daemon needs, then:
readiness.up("key-registry");

let signal = shutdown.clone();
tokio::spawn(async move { signal.on_signal().await });

// Serves /health/live, /health/ready and /about; add routes with `.with(router)`.
Server::new(me, readiness, shutdown)
    .draining_for(std::time::Duration::from_secs(3))
    .listen(http)
    .await?;
```

## What it guarantees

- **Readiness never lies.** A daemon declares what it waits for; it is ready
  only when every declared dependency is up, and `blockers()` names the rest. A
  daemon that declares nothing is **not** ready. Liveness is separate, because
  restarting a CSMS over a missing dependency drops every station's socket.
- **Shutdown drains in two steps.** On `SIGTERM` the daemon first stops
  reporting ready, so the orchestrator stops routing to it, then stops serving
  after a caller-chosen drain window. Killing a CSMS mid-transaction would lose
  the `StopTransaction` carrying the signed meter record.
- **Loopback by default.** `serve::bind_address` reads `<DAEMON>_HTTP_BIND`,
  then `EMOB_HTTP_BIND`, then the daemon's local default, and names the variable
  in any parse error. A second socket (`serve::bind_socket`) has its own
  variable so the estate-wide one never binds two listeners to one port.
- **One party cannot read another's records.** See the authority model below.
- **State comes back from the journal, and the journal's end is signed.** A
  daemon refuses to start from a journal shorter than the head it last signed.
  See below.
- **Every fact and every request names who caused it** — the authenticated
  caller, never what the request says about itself.

## How it works

### The authority model: who, what, whose

The worst thing a roaming node can do is serve party A's CDRs to party B from an
endpoint that accepted a valid token. A reverse proxy cannot prevent that,
because ownership is a field on the record. So three questions stay separate: a
`Token` answers **who** (constant-time comparison), `Capabilities` answers
**what**, and a `PartyScope` answers **whose records**.

```rust
let peer = Principal::peer(theirs, Role::Emsp, Capabilities::of([caps::CDR_READ]));

peer.may_act_for(caps::CDR_READ, &theirs);   // true
peer.may_act_for(caps::CDR_READ, &ours);     // false — valid token, somebody else's record
peer.may_act_for(caps::CDR_WRITE, &theirs);  // false — reaching it is not writing it
```

Principals are OCPI parties, which already have a role, but a role is not
enough: an AI or rules-based agent acting for an operator must hold **less**
than that operator. `Principal::attenuate` derives a subset and returns `None`
if it would widen either the capabilities or the party scope:

```rust
let agent = operator.attenuate(
    Capabilities::of([caps::CDR_READ, caps::EVIDENCE_READ]),
    PartyScope::just(&party),
)?;
```

Capability patterns have two forms only — `emob.cdr.read` or `emob.cdr.*` — so
attenuation stays decidable by containment. A wildcard widens at a segment
boundary (`emob.cdr.*` does not admit `emob.cdrs.read`). An empty set permits
nothing.

### Operator policies (Cedar), behind the `policy` feature

Some grants need a vocabulary capabilities lack — *"CDRs, until the trial
ends"*. With the `policy` feature, an operator writes Cedar policies:

```cedar
@id("nl-tnm-trial")
permit (
  principal == emob::Party::"NL*TNM",
  action in [emob::Action::"emob.cdr.read", emob::Action::"emob.tariff.read"],
  resource
)
when { resource.owner == principal }
unless { context.at > datetime("2027-01-01T00:00:00Z") };
```

```rust
let gate = Gate::new(Policies::load(text)?);
gate.verdict(&peer, caps::CDR_READ, &record, at);   // the instant is an argument
```

- The Cedar schema is generated from `caps::ALL`, so a policy cannot name an
  action nobody can be granted.
- The typed ceiling is checked first and its "no" is final: a policy may grant,
  never widen a delegation.
- Cedar warnings are errors — they include identifiers that merely look like
  another party's.
- An evaluation error is a denial, never a skipped `forbid`.
- `Gate::reload` refuses an invalid set and keeps the old one deciding.

### CloudEvents: the catalogue and the envelope

One `pub const` per event type, and only types a daemon emits — a test holds
every constant to being named in a daemon's source. `de.emob.cdr.issued` and
`de.emob.invoice.issued`. Every type ends in a past participle, enforced
by a test — an imperative type is a command posing as an event.

```rust
events::cdr::ISSUED;                                   // "de.emob.cdr.issued"
events::matches("de.emob.cdr.*", events::cdr::ISSUED)  // true

let event = cloudevent::CloudEvent::new(key, "csmsd", events::cdr::ISSUED, data)
    .about(format!("cdr/{key}"))
    .at(now);                                          // structured mode, signed as a whole
let read = cloudevent::open::<Data>(&body, events::cdr::ISSUED)?; // any other type is refused
```

`estate::subscriptions` checks `[[subscribers]]`: a name that is an actor, an
`https` (or loopback `http`) URL, its own `whsec_` secret, and type patterns
that each admit a type in the catalogue.

### Webhook signatures

The official Rust SDK signs each UTF-8 CloudEvent. Each attempt carries the
original event id and a fresh Unix timestamp. Receivers use the event id for
deduplication and the timestamp for freshness. [Standard Webhooks specification](https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md)

```rust
let header = webhook::sign_with(&delivery, [&outgoing, &incoming])?;
webhook::verify(&delivery, &[outgoing], &header);
```

For incoming HTTP use `webhook::receive` with the receiver's clock and
freshness tolerance. `webhook::Secret::standard` takes a prefixed Base64 key
and `webhook::Secret::raw` takes raw bytes. Both reject an empty key.

Enable `webhook-client` to send signed CloudEvents with
`webhook::transport::Client`. CDR and invoice delivery use the same status
rules. A 2xx acknowledges delivery. A 401, 408, 429 or 5xx leaves it pending.
Other 4xx responses record a refusal. The client contacts the configured
destination directly, leaves redirects pending, and reads at most 8 KiB
of response content for the receipt or diagnostic.

### The estate: one file for every daemon

Every daemon reads its own sections of one `estate.toml` (`EMOB_ESTATE`,
default `./estate.toml`) with `estate::load` / `estate::from_str` into a type it
declares. The typed sections — `Parties`, `Station` (with its
`SecurityProfile` and `receipt_display_minutes`), `Csms`, `MeterKey`,
`TariffSpec`, `Partner`, `BillingSection`, `Billing`, `ContractSeed`,
`Operator`, `Services`, `Desk`, `Witness`, `Roaming` — are shared, so two
daemons cannot read one key two ways. An unknown or malformed key refuses the
start, naming the key. The invoicing types — `InvoiceSpecification` (EN 16931
or XRechnung), `InvoiceSyntax` (UBL or CII), `InvoiceDelivery`,
`InvoiceContact` and `MandateSeed` — describe what `billd` writes and where it
hands it.

`estate::Estate` is the whole file — every section, a section no daemon reads
refused — and with the `schema` feature `estate::json_schema()` derives the
JSON Schema published as `schemas/estate.schema.json` (a test holds the two
equal). A refusal is rendered like `rustc`'s (`estate::report`): the file, the
line, the key underlined, and `= help: did you mean …`.

Secrets are never in the file: a field ending `_env` is an `EnvRef`, the name of
an environment variable, resolved through an `Env` (the process environment, or
a map in tests) — an unset variable refuses the start, naming it.
`estate::webhook_secret` and `estate::operator_tokens` resolve the two every
daemon needs. `TariffSpec::to_tariff` builds the `emob_tariff::Tariff` that
`csmsd` rates with and `tarifd` publishes, in one place, so the published price
and the charged one cannot diverge `[AFIR Art. 5(4)]`.

### Data at rest: data keys, digests and sealed records

A journal is append-only and the file most likely to leave the building in a
backup, so a daemon writes no credential and no card UID into it in clear
(GDPR Article 17, the right to erasure). The `vault` module:

| Type | What it does |
|---|---|
| `vault::DataKey` | one 32-byte key per daemon from `<DAEMON>_DATA_KEY` (base64); its key id is the first 8 hex of its SHA-256. Two subkeys are derived, one for HMAC, one for encryption |
| `vault::Digest` | HMAC-SHA256 for what a daemon only needs to *recognise* (a token a partner presents); purpose-separated, compared in constant time with `DataKey::matches` |
| `vault::Sealed` | AES-256-GCM (aws-lc-rs) for what it must *use* again (a UID a CDR carries); random nonce, the purpose as associated data so a box cannot be replayed into another field |
| `vault::Sealing` | a `Journal` whose record bodies are sealed while the hash chain runs over the sealed bytes — the chain verifies without the key, and record kinds stay readable |

A record under another key is told apart from a forgery by its key id: for
`empd` that is crypto-erasure (the key was destroyed on purpose), for the
others a refusal to start.

### Operator access and inter-daemon deliveries

- `serve::operator_only` accepts a named estate credential for private reads.
  `serve::operator_write` also requires `write = true` on that operator. The
  handler receives the authenticated actor for attribution. An unknown bearer
  receives HTTP 401 and a read-only bearer attempting a write receives HTTP 403.
- The desk's background requests use a separate read-only credential. The
  station authorization service bearer must differ from every operator bearer.
  Each operator needs a unique name and secret.
- `webhook::receive` checks a delivery's headers, its 300-second freshness and
  its Standard-Webhooks signature, and answers the delivery id or a
  `webhook::Refusal`. `serve::read_bounded` caps a request body before it is
  read.
- `Server::serve_on` serves the shell on a listener the caller bound — how a
  test, or `emob demo`, runs a daemon on port 0.

### Server-rendered pages: `ui`

The operator page (Heute, in `deskd`), the driver receipt (in `csmsd`) and the
invoice page (in `billd`, with a print layout) share one stylesheet and one set of helpers: `ui::page` and `ui::escape`,
`Lang::negotiate` (German first, `?lang=en`), `Lang::date` and `Lang::decimal`
for the reader's conventions, `ui::hardened` for the security headers,
`ui::script` for a page's same-origin script, and `ui::operator_page` — HTTP
Basic over `Operators` with a hardened `401` challenge, so a browser can sign
in.

### The journal: state rebuilt from facts

Domain crates hold no state; daemons hold the rules about a **pair** of states —
an invoice number is spent once, a record is sealed once sent, a card is
blocked. Those are facts about the past, so a daemon keeps them in a `journal`
and folds its state from it on start (requires the default `serde` feature).

```rust
use emob_service::Actor;
use emob_service::journal::{Folded, Journal as _, Kinded, Memory};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum Fact { Counted(u32) }

impl Kinded for Fact {
    fn kind(&self) -> &'static str { "counted" }
}

fn apply(total: &mut u64, fact: Fact) {
    let Fact::Counted(n) = fact;
    *total += u64::from(n);
}

// `journal::for_daemon("billd")` is the same call with a file behind it,
// under EMOB_JOURNAL_DIR (default `var`).
let mut folded = Folded::open(0_u64, Memory::new(), apply)?;
folded.record(&Actor::Timer, Fact::Counted(7))?;   // decide, record, then tell the caller
assert_eq!(*folded.state(), 7);
```

- **Facts, not commands.** Replaying a command would re-decide against today's
  tariff and clock.
- **Records are hash-chained.** An edited or removed record breaks every digest
  after it, and `Folded::open` refuses to start.
- **A failed write stops the service**, so the state is never ahead of the
  record.
- **Every record names its actor**, inside the digest (below).

A journal written before 0.3 has no actor field and does not replay: start from
an empty one.

### Who caused it: `Actor`

`[NIS2 Art. 21(2)]` asks for incident handling, and an incident nobody can
attribute is one nobody can write up. So every journal record carries `actor`
beside `kind`, covered by the hash chain and readable without the data key
(`jq -r .actor billd.ndjson`). It is a closed set, set where the caller was
**authenticated**:

| Actor | Who |
|---|---|
| `station:<identity>` | the identity a station's socket authenticated as |
| `partner:<party>` | the OCPI party a credential belongs to |
| `operator:<name>` | the estate's `[[operators]] name` — never the token |
| `service:<daemon>` | another daemon, by a signed delivery or the service token |
| `timer` | the daemon's own loop: a calendar, a drain, a retry |
| `estate` | the estate file, read at start |

The id is 1–64 characters of `[A-Za-z0-9*._:=+|@ -]` — enough for an OCPP
identity, a party id and a name, and nothing that can carry a token, a newline
into a log or a quote into a page. An operator name outside it refuses the start.

### Where the journal ends: the signed head

A hash chain proves nothing was rewritten or removed from the middle, but not
that nothing was cut from the **end** — a truncated journal looks like a crash
mid-append. So `head::Signing` wraps a journal and, after **every** append,
signs its head (position and hash) with the daemon's Ed25519 key (`aws-lc-rs`)
into `<journal>.head`, as a [C2SP signed note](https://c2sp.org/signed-note):

```text
emob/csmsd
57
q1Iot…=

— emob/csmsd GrH0…==
```

On start it refuses a journal shorter than the signed head, one whose record at
that position differs, one more than a single record past it (a crash between
append and signature leaves exactly one), and one of more than one record with
no head beside it — naming both positions. After an intended reset, moving `.head`
aside is the deliberate operator step.

The key is 32 random bytes, base64, from `<DAEMON>_SIGNING_KEY`
(`head::SigningKey::for_daemon`); it is never defaulted, and a `.head` signed
under another key refuses the start. `head::router` serves the latest note at
`GET /v1/<daemon>/head` (`text/plain`, mount it behind `serve::operator_only`)
with the verifier key — `<name>+<key hash>+<base64>` — in the
`Emob-Verifier-Key` header.

### The witness

Whoever can cut the journal can delete its `.head`, and a backup restore brings
an old head with it. `witness::Witness` is a copy no daemon volume holds: it
reads each head, checks the signature against the key the estate's
`[witness] keys` names or the one pinned at first sight, and records every
observation in its own chained file. A head that comes back **lower** is
`regressed`, the same position with another hash is a **fork**, another key is a
**rekey** — each an alarm that stays in the file's history until the file is
moved aside. `deskd` runs one every 30 seconds; `emob witness` runs one from
the command line.

What it cannot see: a history rewritten **and** extended past the witnessed
position. The journal is a chain, not a Merkle tree, so a head that moved
forward is accepted without a consistency proof.

### Logs

`telemetry::init` writes JSON lines by default (`EMOB_LOG_FORMAT=text` for a
terminal; `RUST_LOG` filters), with the daemon's name and version. The shell
gives every HTTP request a span with:

- `id` — the inbound `x-request-id` when it is 1–64 of `[A-Za-z0-9._-]`,
  otherwise minted; echoed in the response;
- `method` and the matched `route`, never the raw path (`/r/{token}` is a
  capability, so the path is a secret);
- `actor`, set by the handler that authenticated the caller (`serve::acting`).

No token, secret, key, password or card UID is logged; `telemetry::Captured`
holds a process's lines so a test (and `emob demo --check`) can search them.

## API at a glance

| Item | Purpose |
|---|---|
| `Server`, `serve::bind_address`, `serve::bind_socket` | HTTP shell (`/health/live`, `/health/ready`, `/about`) and bind addresses |
| `Readiness`, `Probe`, `Identity`, `identity!` | declared dependencies; the daemon's name and version |
| `Shutdown` | signal handling and drain |
| `telemetry::init`, `telemetry::Captured` | JSON logs (`EMOB_LOG_FORMAT=text`, `RUST_LOG`); captured lines for tests |
| `Actor` | who caused a record or a request |
| `head::Signing`, `head::SigningKey`, `head::VerifierKey`, `head::router` | the signed journal head and its route |
| `witness::Witness`, `witness::Verdict` | the witness: regression, fork and rekey alarms |
| `serve::acting`, `serve::REQUEST_ID` | the request span's actor and id |
| `config::load`, `config::load_from`, `Secret` | file + environment configuration |
| `Principal`, `Capabilities`, `PartyScope`, `Token`, `caps` | the authority model |
| `events` | CloudEvents type catalogue and pattern matching |
| `webhook::sign`, `webhook::sign_with`, `webhook::verify`, `Delivery` | Standard Webhooks |
| `journal::Folded`, `journal::File`, `journal::Memory`, `journal::for_daemon` | hash-chained fact journal |
| `estate::load`, `estate::from_str`, `estate::path`, `EnvRef`, `Env`, `TariffSpec::to_tariff`, `estate::webhook_secret`, `estate::operator_tokens` | the one estate file and the variables it names |
| `InvoiceSpecification`, `InvoiceSyntax`, `InvoiceDelivery`, `InvoiceContact`, `MandateSeed` | the estate's invoicing keys |
| `vault::DataKey`, `vault::Digest`, `vault::Sealed`, `vault::Sealing` | data keys, keyed digests, sealed records and journals |
| `Operators`, `serve::operator_only`, `serve::operator_write`, `serve::read_bounded`, `Server::serve_on` | operator read models, bounded bodies, a caller-bound listener |
| `webhook::receive`, `webhook::Refusal` | a signed, fresh inter-daemon delivery, or why not |
| `ui::page`, `ui::operator_page`, `ui::script`, `ui::hardened`, `Lang` | server-rendered pages |
| `policy::Gate`, `policy::Policies` | Cedar policies (feature `policy`) |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | serialisation of persisted/forwarded types; enables `journal`, `estate` and `vault` |
| `policy` | off | Cedar policies over the authority model's entity types |
| `schema` | off | `estate::json_schema()`, the estate's JSON Schema derived from its types |

## Standards and sources

- [Standard Webhooks](https://www.standardwebhooks.com), [CloudEvents](https://cloudevents.io)
- [C2SP signed note](https://c2sp.org/signed-note) and the [checkpoint](https://c2sp.org/tlog-checkpoint) shape, Ed25519
- `[NIS2 Art. 21(2)]` — incident handling, the reason every fact names its actor
- OCPI party and role model (`emob_core::Role`)
- [Cedar](https://www.cedarpolicy.com) policy language

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices; every daemon
in `services/` is built on this crate. Background:
[Architecture](https://hupe1980.github.io/emob/docs/architecture/) on the site;
see also the [workspace README](../../README.md) and
[THREAT_MODEL.md](../../THREAT_MODEL.md).

## License

MIT OR Apache-2.0
