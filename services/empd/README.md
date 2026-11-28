# empd

The e-mobility service provider (eMSP) daemon: the keyed token store, real-time authorisation of a driver's contract, the AFIR price quote before a session, and the monthly fee owed whether or not the driver charged.

The provider also authenticates `[[portal_users]]` and owns live agreement and payer grants. Named operators manage them through **Customer access** in Heute. After restart, the provider restores active grants using the signed journal and current customer custody. Customers sign in again.

Implement a replacement charging authorization service using the request, decisions, retail binding and bearer credentials in the [integration OpenAPI document](../../schemas/openapi.json).

## Status

| | |
|---|---|
| ✅ library | `empd::Empd` and `empd::Book` implement the token store, `[OCPI 2.3.0 §mod_tokens]` authorisation, credit limits, the `[AFIR Art. 5(5)]` quote, and fees per contract — tested in `tests/a_contract_authorises_a_session.rs` |
| ✅ journal | on start the daemon folds its hash-chained journal (`empd` under `EMOB_JOURNAL_DIR`) before reporting ready; card UIDs in it are sealed under `EMPD_DATA_KEY`, and it refuses to start without that key. Every record names its actor; the head is signed under `EMPD_SIGNING_KEY` after every append, and a journal shorter than its signed head refuses the start |
| ✅ estate | contracts and tokens are seeded once from the estate's `[[contracts]]`; the provider party is the estate's `[parties] provider` |
| ✅ authorisation | `POST /v1/authorize`, behind the service token `csmsd` holds, answers `Accepted` (with the token's keyed reference), `Blocked`, `Expired` or `Invalid` with a reason |
| ⏳ not wired | an OCPI tokens endpoint for partners, and the fee hand-off to `billd` |

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
must not make every digest reversible. Recovery requires the original provider data key and current customer custody. The data key opens withdrawal evidence and names card tokens. Each portal subject has a separate random key for its grant records. The contract identifier's check
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
credit limit, silently. `Book` folds the journal’s contract, issue, label,
block, operator change, native agreement, usage and invoice records. A session offered twice at
the same figure records nothing (partner retries must not grow the journal); at
a different figure it records, because a re-rated record moves the total.

### Restore customer access

Stop the provider before replacing its journal and signed head. Keep the **current** `EMPD_CUSTODY_DIR` mounted outside the journal backup and supply the original `EMPD_DATA_KEY`. The first signed journal record pins the custody identity. The provider checks it before opening its HTTP listener. An unavailable, unrelated or unreadable store stops startup.

Erasure replaces one subject’s grant key with its receipt in a single durable update. A signed journal record binds that receipt to the confirmed operation. Restoring an older custody registry alongside a journal containing that record stops startup. Older copies of the journal then replay without that subject’s grant contents. The remaining subjects keep their own keys. Arrange protection and replication of the current custody separately. Historical copies, filesystem snapshots or exports containing destroyed keys can recover those records and must be covered by the deployment’s erasure procedure.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[parties]`, `[[contracts]]`, `[[card_stock]]`, `[[retail_accounts]]`, `[[public_plans]]`, `[desk] csmsd`, `[services]`, `[[operators]]` |
| `EMPD_DATA_KEY` | **required** | 32 bytes, base64, for token references, sealed UIDs, withdrawal evidence and the current customer key store |
| `EMPD_CUSTODY_DIR` | `custody/empd` | Independent private directory for current subject keys and erasure receipts. Mount a persistent volume at `/var/lib/emob-custody` when running the container. Compose supplies it. |
| `EMPD_SIGNING_KEY` | **required** | 32 random bytes, base64 (an Ed25519 seed); signs the journal head. A `.head` signed under another key refuses the start |
| `EMOB_SERVICE_TOKEN` | **required** | Service credential used by `csmsd` for online authorisation |
| `EMOB_OPERATOR_TOKEN` | **required in demo** | Named human operator with `write = true` for card changes and native agreements |
| `EMOB_DESK_READ_TOKEN` | **required in demo** | Separate read-only identity for background desk queries |
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
| `POST /v1/empd/cards/lookup` | operator token. Find cards by a printed number, eMAID or exact keyed reference. Results are private and bounded to 20 cards. |
| `POST /v1/empd/cards/preview` | Named writer. Review a change to one card and obtain its five-minute operation ID. |
| `POST /v1/empd/cards/confirm` | Same named writer. Confirm the reviewed operation and return its journalled receipt. |
| `POST /v1/empd/customer/erasure/{status,review,confirm}` | Named privacy operator. Status and review accept the customer sign-in ID in `subject`. Confirm also requires the returned `operation`. Reopen the subject for the original receipt. |
| `GET /v1/empd/head` | operator token: the journal's signed head, `text/plain`, verifier key in `Emob-Verifier-Key` |
| `GET /v1/empd` | operator token: contracts and tokens **by reference, never by UID** |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the contracts are folded from the journal; `503` listing `contracts` until then |
| `GET /about` | name and version |

To find a card, send JSON such as `{"kind":"visual","value":"MOB-1001"}` to the lookup endpoint. A token seed can set `visual_number` to the case-sensitive number printed on the card. The provider journals a keyed digest of the number. Changing or removing that number on the same token updates its search entry at the next start. The response shows an emob support label, token type, block status and keyed reference. The support label also works as a visual search. For a contract, use `kind: "emaid"` and supply its eMAID. For the reference stored on a session, use `kind: "reference"`. To read the next page, repeat the original request with the returned `next` value in `after`. Every search uses a POST body and a private, non-cacheable response. [OCPI token object](https://github.com/ocpi/ocpi/blob/release-2.3.0-bugfixes/mod_tokens.asciidoc#mod_tokens_token_object) · [RFC 9110 §17.9](https://www.rfc-editor.org/rfc/rfc9110.html#section-17.9)

### Enrol a native driver card

A named writer can issue one stocked RFID against one currently offered point and provider plan. The sleeve carries an inventory code such as `STOCK-1004`. Only the provider reads its configured UID. The signed agreement preserves the full operator tariff, provider fee schedule, validity, currency, VAT and their public offer digest.

| Request | Access and result |
|---|---|
| `GET /v1/empd/contracts/options` | A reader can list configured provider, point and plan choices. The provider checks live publication during review and confirmation. |
| `POST /v1/empd/contracts/preview` | A writer submits `{"account":"PAYER-FLEET","stock":"STOCK-1004","point":"DE*ABC*E00001","plan":"standard"}`. The five-minute review contains a masked label, complete offer and operation reference. |
| `POST /v1/empd/contracts/confirm` | The same writer submits `{"operation":"<reference>"}`. The provider checks the current offer again and commits the agreement, actor, assigned eMAID and sealed card together. |
| `POST /v1/empd/contracts/lookup` | An operator submits `{"agreement":"<reference>"}` to retrieve the original accepted terms and time. The original writer can also repeat a successful confirmation after restart. |

The next online authorization must identify the enrolled EVSE. The provider checks the card’s standing and the offer’s validity interval, and the CSMS checks the digest against its current advertised terms before approving charging. The card uses `NEVER` whitelisting, which requires real-time authorization. [OCPI Tokens](https://github.com/ocpi/ocpi/blob/release-2.3.0-bugfixes/mod_tokens.asciidoc#mod_tokens_real-time_authorization)

To replace a card or end an agreement, submit `{agreement,action,stock?}` to `POST /v1/empd/contracts/change/preview`. Confirm the returned `{agreement,operation}` at `/v1/empd/contracts/change/confirm`. The old credential is retired immediately for online authorization.

A writer gets one pending enrolment at a time. A later review replaces it, and restarting the provider discards unconfirmed reviews. An unknown stock code or selection returns 404. An already used card, changed terms, wrong actor or expired review returns 409. Unavailable or unverifiable publication returns 503. After a lost confirmation response, look up the agreement reference or retry the original confirmation. The operator intake records administrative issuance. The sale retains the selected bill-to account, its private or fleet classification, and its profile digest with the terms. A named reader can retrieve its agreements through `POST /v1/empd/contracts/billing-agreements`. Billing owns the retail invoice. No mandate is created at enrolment.

### Erase customer portal access

Use a named privacy operator to review a customer’s current agreement and payer grant counts. Confirmation destroys the independent key for all that subject’s historical portal grants, drops their live sessions and disables future sign-in and regrant. The status receipt retains counts, the reviewer and confirmation time. Card authorization, original contracts, charging records, invoices and payment allocations continue under their respective owners. Remove the obsolete sign-in credential from deployment configuration.

The privacy API accepts POST requests at `/v1/empd/customer/erasure/review` and `/v1/empd/customer/erasure/status` with `{"subject":"driver-one"}`. To confirm, submit the subject and the review’s `operation` to `/v1/empd/customer/erasure/confirm`. The review expires after five minutes or when the grants change. A retry of the confirmed operation by its original reviewer returns the recorded receipt.

### Review a withdrawal

A named writer records a declaration already received for a private agreement. The four JSON endpoints accept POST requests under `/v1/empd/contracts/withdrawal/`.

| Action | Request |
|---|---|
| `intake` | `agreement`, one stable `request_ref`, RFC 3339 `received_at` and `channel` (`email`, `letter`, `phone`, `in_person` or `other`). |
| `lookup` | `agreement`. Returns the archived offer reference, receipt time, current stage and latest decision. |
| `review` | `agreement`, `outcome` (`hold`, `approve` or `decline`), `evidence_ref` and `billing` (`unaffected` or `review_required`). |
| `confirm` | `agreement` and the returned `operation`, using the same writer credential within five minutes. |

Approval retires the current card for online authorization and ends the agreement for future retail billing. Holding a case keeps its current entitlement and permits a later decision. A card change or competing decision requires a fresh review. A committed confirmation returns its original receipt on retry, including after restart. Issued invoices and cash adjustments have their own finance review.

### Review and change a card

Find the card and send its keyed reference, desired `blocked` flag and reason to `/v1/empd/cards/preview`. Blocking accepts `lost`, `stolen` or `suspected_misuse`. Unblocking accepts `recovered` or `cleared`. Either direction accepts `correction`. The preview returns the card’s support label, effective change, expiry and operation ID. A new successful preview replaces the same operator’s previous preview. Reviewing makes no change to authorisation.

Send `{"operation":"<64 lowercase hex>","reference":"<reviewed reference>"}` to `/v1/empd/cards/confirm` with the same writer credential. The optional reference checks the selected card against the review. The provider checks the current revision, records one decision and returns its operation, card, reason, actor and both UTC timestamps. Its next online authorisation uses the new standing. Unblocking resumes contract checks. Chargers retain any offline authorisation cache, and running sessions continue.

Read the exact card by reference to recover the ten most recent confirmed changes, newest first. This also resolves a lost confirmation response. Read-only operators may see history. The provider drops pending previews after a restart and restores committed decisions from its signed journal, including changes to cards initially blocked in the estate.

| Status | Meaning |
|---|---|
| 400 | Malformed JSON, invalid reference, unknown field or invalid reason |
| 401 / 403 | Unrecognised credential / named operator lacks write permission |
| 404 | The provider has no card with the requested reference |
| 409 | Already in the requested state, mismatched reference or an expired, used or superseded review. Read the card again before retrying. |
| 413 / 415 | Body exceeds 2 KiB / content type must be JSON |
| 503 | Provider temporarily unavailable. Check the card’s history before retrying. |

## Built on

`emob-roam` (`RoamingToken`), `emob-session`, `emob-tariff` (price
descriptions), `emob-billing` (`Subscription`), `emob-core` (provider
obligations), `emob-service` (shell, journal). See
[Pricing](https://hupe1980.github.io/emob/docs/pricing/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0

### Public plans at a point

For an opted-in public pass-through plan, the point's provider-price page and provider-side callers share `empd::public_retail_tariff`. It combines exact gross EUR rates **before** one session rating. `Empd::quote` remains available for existing library callers. The public offer includes the separate e-roaming fee and excludes recurring subscriptions from session examples. Rendering and durable price references live with the current station assignment in csmsd. Partner settlement and individual consumer invoicing are distinct from this public price lookup. [Public provider-price routes and estate setup](../csmsd/README.md#public-provider-prices)
