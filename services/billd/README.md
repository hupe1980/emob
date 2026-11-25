# billd

The billing daemon: decides when a month is closed, which invoice number it gets, which invoice supersedes which, and which ledger account each posting lands in — and puts every issued invoice on a page its recipient can read, and in the operator's ERP.

## Status

| | |
|---|---|
| ✅ library | `billd::Billd` and `billd::Books` implement numbering, closing once, cancellation and re-billing, submission state, and double-entry posting into [`doubleentry`](https://crates.io/crates/doubleentry) 0.9 — tested in `tests/a_month_is_closed_once.rs` |
| ✅ journal | on start the daemon folds its hash-chained journal (`billd` under `EMOB_JOURNAL_DIR`) and resumes the number series from it before reporting ready. Record bodies — received CDRs carry a card UID, invoices an IBAN — are sealed under `BILLD_DATA_KEY`; every record names its actor; the head is signed under `BILLD_SIGNING_KEY` after every append, and a journal shorter than its signed head refuses the start |
| ✅ deliveries | `POST /v1/deliveries` takes the `de.emob.cdr.issued` CloudEvents `csmsd` emits (any other type is refused): 1 MiB cap, Standard-Webhooks signature and 300 s freshness, id = CDR key, issuer must be an estate partner; journalled as `received`, then answered. A duplicate is answered `200 duplicate`; a signed sample marked `emobtest` is `202` and never booked, so a replacement and this one pass the same `emobtest conformance receiver` |
| ✅ month close | an hourly calendar closes each partner's previous month once: one EN 16931 invoice per partner and calendar month from the records received, seller from `[billing]`, buyer from the partner, in the partner's `specification` (EN 16931 or XRechnung). A document that specification rejects is refused at the close, while it is only a spent number. Each line carries the driver's receipt link |
| ✅ invoice page | every issued invoice has a page at `/i/{token}` with its UBL and CII files |
| ✅ delivery | with `[billing.delivery]`, each issued invoice is posted, signed, to the operator's ERP or Peppol provider until it answers; an accepted one is booked |
| ⏳ not wired | SEPA collection runs; the library builds them |

What a document **says** — rounding, VAT, EN 16931, SEPA, role-addressed
postings — is `emob-billing`'s and tested there. `billd` holds the four facts no
single document can carry.

## What it owns

### 1. The invoice number

`[UStG §14(4) Nr. 4]` requires a sequential number, in one or more series,
assigned **once** (*einmalig*) by the issuer. That is a fact about a counter,
not about a month's records, so it lives here.

```rust
Numbering::series("R", 2026).resuming_after(6)   // the store said 0006
```

Several series (per year, branch or document kind) are explicitly allowed, so
`R-2026-0001` beside `S-2026-0001` is lawful. A number is spent before the
document is built and stays spent if the document is later rejected.

### 2. Closing a period once

Re-closing a month against the same records is either the same invoice again or
a correction. `Billd::issue` refuses a period it already closed
(`ClosingError::AlreadyClosed`); `Billd::rebill` is the explicit correction.

### 3. Which invoice supersedes which

A re-rated month is cancelled and re-billed, reversal first — the same order
`[OCPI 2.3.0 §mod_cdrs]` uses for CDR corrections:

```rust
let (credit, replacement) = billd.rebill(
    "R-2026-0001",
    date!(2026 - 07 - 20),
    "re-rated: the June tariff was published at 0.39 and billed at 0.49",
    closing,
)?;
```

Both documents get numbers from the same series and both are returned, because
the recipient needs the cancellation (*Stornorechnung*) before the replacement
makes sense. The reason travels on the document as a BT-22 note.

### 4. Which account a role lands in

`emob-billing` addresses postings by role; the chart of accounts is the
operator's. Defaults are paths; override them with `ChartOfAccounts::mapping`
(for example to SKR 03 or SKR 04 numbers).

| Role | Default path | SKR 03 example |
|---|---|---|
| `Receivable` | `assets:receivable` | `1400` |
| `EnergyRevenue` | `income:energy` | `8400` |
| `ServiceRevenue` | `income:service` | `8400` |
| `VatPayable { rate, place_of_supply }` | `liabilities:vat:DE:19` | `1776` |

The place of supply is part of the VAT account path, because one document can
owe tax in two countries: electricity where the point stands `[UStG §3g]`, a
subscription fee where the supplier sits `[UStG §3a(1)]` (CJEU C‑60/23).

```rust
billd.balance_of("liabilities:vat:FR:20", "EUR")?;   // the French return
billd.balance_of("liabilities:vat:DE:19", "EUR")?;   // the German one
```

The ledger uses monthly periods, opened at January of the first booking's year
and extended forward as later months are booked; a booking dated before that is
refused. Entries carry no memo — the invoice number is the key — so replaying
the journal rebuilds the same ledger.

### Issued, accepted, then booked

`[UStG §14(1)]` requires e-invoices in a structured European-norm format, and a
public buyer's platform answers each submission. So posting is a separate,
later step:

```rust
billd.issue("2026-06", closing)?;                    // numbered, not booked
billd.accepted("R-2026-0001", on, Some(reference))?; // the platform took it
billd.book("R-2026-0001")?;                          // only now the books
```

A cancellation whose original is not booked is refused, because booked alone
it would credit a receivable nobody debited. Booking the same number twice is
reported as a repeat; a replay after a crash finds the entry already written
rather than posting it again.

### State survives a restart

`billd::Books` is `Billd` folded from a hash-chained journal of facts
(`issued`, `submitted`, `booked`). Facts, not commands, so a replay never
re-decides a number.

```rust
let mut books = Books::open(
    "emob",
    Numbering::series("R", 2026),
    ChartOfAccounts::new(),
    emob_service::journal::for_daemon("billd")?,
)?;
let number = books.issue("2026-06", closing)?;   // continues the series
```

A record is appended after the decision and before the caller is told. If the
journal refuses a write the service halts (`Books::is_halted`). A journal edited
in the middle fails to open; one cut at the end is refused against the head
`billd` signed into `billd.ndjson.head`, and a witness (`deskd`, or
`emob witness`) catches a restore from an older backup. After an intended reset,
move the `.head` file aside.

### The invoice page: `/i/{token}`

From 01.01.2027 a German business invoice is the structured EN 16931 file
`[UStG §14(1)]` names, and nobody reads XML. So every issued document has one
page, built like a bill: what is owed, by when and how it is collected; who
bills whom; every session with the receipt link that lets its driver check it;
the VAT; the one rounding amount the document states; and the two files —
`/i/{token}/ubl.xml` (OASIS UBL 2.1) and `/i/{token}/cii.xml` (UN/CEFACT CII),
which are the invoice in law. German first, `?lang=en`, printable, no script.

- **The page computes nothing that decides.** Every figure is the document's
  own, and the files are byte for byte what the operator system receives.
- **The token is a capability.** 128 bits of HMAC-SHA256 over the invoice
  number under `invoice.key` (32 bytes, created beside the journal on first
  start, mode 0600), so a link can be sent again. An unknown token is a bare
  `404`; the routes take 60 requests a second, then `429`; every answer is
  `no-store` and `noindex`. A debtor's IBAN is masked to its country and last
  four.
- **A receipt does not link back.** The invoice is a partner's whole month,
  every driver of that partner on it; a link from one driver's receipt would
  widen a one-session link to all of them.

`[billing] public_url` is the origin the page is linked from (the demo prints
it as `Invoice`).

### Handing the invoice over: `[billing.delivery]`

`billd` writes the e-invoice and does not transmit it — no SMTP, no Peppol access
point. With `[billing.delivery]` set, every document still `Issued` is posted to
the operator's ERP or Peppol provider as a `de.emob.invoice.issued` CloudEvent:
`data` is the file in the configured `syntax` (`ubl` or `cii`,
`application/xml`), `subject` is `invoice/<number>`, the page link is the
`invoiceurl` attribute when `public_url` is set. Signed under the secret
`secret_env` names, `webhook-id` = the invoice number (the receiver's
idempotency key). The URL must be `https://`, or `http://` on loopback.

Every `[[subscribers]]` entry that takes `de.emob.invoice.issued` receives the
same event under its own secret. Whom a document is owed to is fixed at issue;
each answer is an `announced` fact; only the operator system's answer accepts
and books.

| Answer | Recorded | Why |
|---|---|---|
| `2xx` | accepted, with the optional `reference`; then booked | the recipient's platform took it |
| `401`, `408`, `429`, `5xx`, no answer | nothing — retried every `retry_secs` | a secret being rotated, a system starting, a system down |
| any other `4xx` | rejected, with the body (up to 512 bytes) as the reason | the system read it and said no; the number stays spent |

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[billing]` (below), `[[partners]]` (buyer details, `specification`, `buyer_reference`, `mandate`), `[services]`, `[[operators]]` |
| `BILLD_DATA_KEY` | **required** | 32 bytes, base64; seals the journal |
| `BILLD_SIGNING_KEY` | **required** | 32 random bytes, base64 (an Ed25519 seed); signs the journal head |
| the variable `[billing.delivery] secret_env` names | with `[billing.delivery]` | `whsec_<base64>`: signs the handover to the operator system |
| `EMOB_WEBHOOK_SECRET`, `EMOB_OPERATOR_TOKEN`, `EMOB_DESK_READ_TOKEN` | **required** | the variables the estate names: delivery signatures, the example estate’s named operator and background read credentials |
| `BILLD_HTTP_BIND` / `EMOB_HTTP_BIND` | `127.0.0.1:9584` | the shell |
| `EMOB_JOURNAL_DIR` | `var` | journal directory — deliberately not a temp directory, so a reboot does not empty the ledger |
| `EMOB_LOG_FORMAT` | JSON | `text` for a terminal |
| `RUST_LOG` | `info,billd=debug` | log filter |

The invoicing keys of the estate:

| Key | Meaning |
|---|---|
| `[billing] iban` | the operator's account, paid into by transfer (BT-84) |
| `[billing] creditor_id` | the SEPA creditor identifier, for a partner with a mandate (BT-90) |
| `[billing] contact` | `{ name, phone, email }`, the seller's contact (BG-6), which XRechnung requires |
| `[billing] public_url` | the origin invoice pages are linked at: `<public_url>/i/<token>` |
| `[billing.delivery]` | `url`, `secret_env`, `syntax` (`ubl`, default, or `cii`) |
| `[[partners]] specification` | `en16931` (default) or `xrechnung` (BT-24) |
| `[[partners]] buyer_reference` | the partner's reference for our invoices — a Leitweg-ID for a public buyer (BT-10) |
| `[[partners]] mandate` | `{ reference, iban }`: a SEPA direct-debit mandate; without one, the partner pays by transfer |
| `electronic_address` | on `seller` and each partner's `billing`: the e-mail address an e-invoice is addressed to (BT-34 / BT-49), which XRechnung requires of both parties |

## Running

```console
EMOB_ESTATE=deploy/estate.example.toml cargo run -p billd   # plus the *_env variables
```

Or `just demo`, or the whole estate in containers with `just up` (`billd`'s
shell on `localhost:8082`, journal on a named volume).

## Endpoints

| Path | Answers |
|---|---|
| `POST /v1/deliveries` | a `de.emob.cdr.issued` CloudEvent from `csmsd`, signed and fresh; an `emobtest` sample is `202`, not booked |
| `GET /i/{token}`, `/i/{token}/ubl.xml`, `/i/{token}/cii.xml` | unauthenticated, behind the token: the invoice page and its two files |
| `GET /v1/billd/head` | operator token: the journal's signed head, `text/plain`, verifier key in `Emob-Verifier-Key` |
| `GET /v1/billd/today?limit=&invoices=` | operator token: counts of records received and refused deliveries, and the documents waiting on the recipient (`submission` issued or rejected with the reason) — each list `{ total, items, older }` — the newest `limit` (default 50, max 200; anything else is `400`), with `older` the cursor for the next page |
| `GET /health/live` | `200` while the process runs |
| `GET /health/ready` | `200` once the number series is folded from the journal; `503` listing `numbering` until then |
| `GET /about` | name and version |

## Built on

`emob-billing` (documents, VAT, postings by role, the EN 16931 files),
`emob-service` (shell, journal, signed head, pages), [`doubleentry`](https://crates.io/crates/doubleentry) 0.9 (the
ledger). See [Settlement and billing](https://hupe1980.github.io/emob/docs/settlement/)
and the [workspace README](../../README.md).

## License

MIT OR Apache-2.0
