# empd

The e-mobility service provider (eMSP) daemon: the keyed token store, real-time authorisation of a driver's contract, the AFIR price quote before a session, and the monthly fee owed whether or not the driver charged.

## Status

| | |
|---|---|
| ✅ library | `empd::Empd` and `empd::Book` implement the token store, `[OCPI 2.3.0 §mod_tokens]` authorisation, credit limits, the `[AFIR Art. 5(5)]` quote, and fees per contract — tested in `tests/a_contract_authorises_a_session.rs` |
| ✅ journal | on start the daemon folds its hash-chained journal (`empd` under `EMOB_JOURNAL_DIR`) before reporting ready; card UIDs in it are sealed under `EMPD_DATA_KEY`, and it refuses to start without that key. Every record names its actor; the head is signed under `EMPD_SIGNING_KEY` after every append, and a journal shorter than its signed head refuses the start |
| ✅ estate | contracts and tokens are seeded once from the estate's `[[contracts]]`; the provider party is the estate's `[parties] provider` |
| ✅ authorisation | `POST /v1/authorize`, behind the service token `csmsd` holds, answers `Accepted` (with the token's keyed reference), `Blocked`, `Expired` or `Invalid` with a reason |
| ⏳ not yet | an OCPI tokens endpoint for partners, and the fee hand-off to `billd` |

## What it owns

### The token store

`emob_session::Authorization` refuses to store an RFID UID — a UID identifies,
for life, a physical object a person carries. Yet OCPI requires the UID on every
outgoing CDR. The store holds the mapping, keyed:

```rust
let reference = provider.issue_token(&contract, "04A1B2C3D4E5F6", TokenType::Rfid, Whitelist::Allowed)?;
reference.as_str();                 // 64 hex characters, not the UID
provider.present(&reference)?.uid;  // the UID, only at the edge that must send it
```

The reference is an HMAC-SHA256 under a subkey of `EMPD_DATA_KEY`, so another
provider holding the same card computes a different one; the UID itself is
sealed under the same key. The key is never written to the journal — a backup
must not make every digest reversible. Losing the key is crypto-erasure on
purpose: a journal folded under a new key keeps its chain, and its tokens are
`Erased` — they cannot be presented and do not resolve. The contract identifier's check
digit is verified when the token is issued, before any session is billed to it.

### Real-time authorisation

`[OCPI 2.3.0 §mod_tokens]` asks a CPO's question in real time and allows five
answers; four need state no document holds.

| Answer | Needs |
|---|---|
| `Blocked` | the token's standing (a lost or stolen card) |
| `Expired` | a clock against the contract window — inclusive at both ends |
| `NoCredit` | the running total of sessions not yet invoiced |
| `NotAllowed` | the location |
| `Allowed` | all of the above |

`invoiced` subtracts **what the invoice billed** rather than clearing the total,
so a session authorised while a month is closing is not wiped. A closing that
bills more than the total holds is refused by name.

The whitelist rule cuts both ways: an operator asking about an `ALWAYS` token
has a stale list, and a session started from a list for a `NEVER` token was
never authorised (`Empd::started_from_list`). Both are refusals, because each
is a session nobody will be paid for.

### The quote before the session

`[AFIR Art. 5(5)]` obliges the **provider** to show, before the session, all
price components including e-roaming costs and its own fees. The operator's
components come verbatim from `emob_tariff::describe`, in the order
`[AFIR Art. 5(4)]` prescribes; the provider's own charges follow, each named,
with e-roaming as its own component.

```rust
quote.charged_by(ChargedBy::Operator);          // the operator's price
quote.charged_by(ChargedBy::ProviderERoaming);  // the e-roaming cost the article names
quote.passes_the_operators_price_through(&tariff, at);
```

The last check is the article's point: a provider must not fold a surcharge
into the operator's per-kWh price and show one number.

The same paragraph forbids extra charges for cross-border e-roaming. A `Markup`
belongs to a partner and the type has no country, so no border surcharge can be
expressed. That lets `Empd::provider_profile` **derive** the four facts
`emob_core::ProviderProfile` needs, rather than taking ticked boxes:

```rust
let assessment = assess_provider(&provider.provider_profile(), today);
```

A provider with no price list discloses nothing, and the assessment says so.

### The fee owed regardless of charging

CJEU C‑60/23 treats a fee charged *"regardless of whether the user actually
purchased electricity"* as a separate supply of services. A contract with no
sessions produces no records, so the fee is derived from the **contracts in
force**:

```rust
provider.fees_for(date!(2026 - 06 - 01), date!(2026 - 06 - 30));
```

Each is an `emob_billing::Subscription`, taxed under `[UStG §3a(1)]` rather than
`[UStG §3g]`.

### State that survives a restart

Each forgotten fact fails differently: a lost token store refuses every driver;
a lost block admits a stolen card; a lost running total authorises past every
credit limit, silently. `Book` is `Empd` folded from a journal of `contracted`,
`issued`, `ran_up`, `invoiced` and `blocked` facts. A session offered twice at
the same figure records nothing (partner retries must not grow the journal); at
a different figure it records, because a re-rated record moves the total.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[parties]`, `[[contracts]]`, `[services]`, `[[operators]]` |
| `EMPD_DATA_KEY` | **required** | 32 bytes, base64: the token references' key and the seal on UIDs. A default would be a key anyone with this source could use |
| `EMPD_SIGNING_KEY` | **required** | 32 random bytes, base64 (an Ed25519 seed); signs the journal head. A `.head` signed under another key refuses the start |
| `EMOB_SERVICE_TOKEN`, `EMOB_OPERATOR_TOKEN` | **required** | the variables the estate names: the bearer `csmsd` authorises with, the read model's bearer |
| `EMPD_HTTP_BIND` / `EMOB_HTTP_BIND` | `127.0.0.1:9585` | the shell |
| `EMOB_JOURNAL_DIR` | `var` | journal directory — deliberately not a temp directory |
| `EMOB_LOG_FORMAT` | JSON | `text` for a terminal |
| `RUST_LOG` | `info,empd=debug` | log filter |

## Running

```console
eval "$(just dev-keys)"
EMOB_ESTATE=deploy/estate.example.toml cargo run -p empd   # plus the *_env variables
```

Or `just demo`, or `just up` for the whole estate (`empd`'s shell on
`localhost:8083`).

## Endpoints

| Path | Answers |
|---|---|
| `POST /v1/authorize` | service token: a token's authorisation, fail-closed on anything unknown |
| `GET /v1/empd/head` | operator token: the journal's signed head, `text/plain`, verifier key in `Emob-Verifier-Key` |
| `GET /v1/empd` | operator token: contracts and tokens **by reference, never by UID** |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the contracts are folded from the journal; `503` listing `contracts` until then |
| `GET /about` | name and version |

## Built on

`emob-roam` (`RoamingToken`), `emob-session`, `emob-tariff` (price
descriptions), `emob-billing` (`Subscription`), `emob-core` (provider
obligations), `emob-service` (shell, journal). See
[Pricing](https://hupe1980.github.io/emob/docs/pricing/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0
