# accountingd

The local eMSP’s receivables ledger tracks issued retail documents, booked bank credits and named allocations. Open **Receivables** in Heute with a finance grant. Choose a payer, import a local statement, then review and confirm an amount against an invoice. Balances and allocations survive a restart.

## Statement format

Import a UTF-8 JSON file using the estate’s v1 format. Each file identifies one payer, currency and bank account source. A source must consistently identify the same bank account across statement periods. Use integer minor units, such as `6000` for EUR 60.00.

```json
{
  "version": 1,
  "account": "PAYER-PRIVATE",
  "source": "OPERATING-BANK",
  "currency": "EUR",
  "entries": [
    {"reference": "ENTRY-2026-00042", "amount_minor": 6000, "status": "booked", "direction": "credit"}
  ]
}
```

A [downloadable example](../../site/static/examples/payment-statement-v1.json) works with the demo. Each file may contain up to 100 entries in 64 KiB. Reference is the bank’s stable entry ID within the source account. Reimporting the same booking leaves the balance unchanged. Missing or conflicting references, unsupported reversals, pending entries, debits and nonpositive amounts appear in the review list with their reason. A corrected import can book a previously held entry. Both the original evidence and its outcome remain in the sealed journal.

## Balances

Money is separate by issuer, payer and currency. **Net due** sums issued charges, credits and confirmed allocations. **Unapplied** is booked cash still available to allocate. A linked replacement carries the cash previously applied to its original. A lower replacement can leave a customer credit. The original allocation continues to identify its operator and document.

A configured payer with no entries has a zero balance. The balance API also serves historical payers with retained records. An unknown payer returns HTTP 404.

## Apply a credit to another invoice

A named finance writer can move available credit between two current invoices for the same issuer, payer and currency. In Heute, open **Receivables**, choose the payer, enter the invoice with credit, the invoice to pay and the amount. Review the signed balances on both sides before confirming. Finance can reopen the posting from the account history. The payer sees the amounts used or received on each authorized invoice.

For API clients, POST to `/v1/accountingd/offsets/review` with `{"source_number":"INV-A","target_number":"INV-B","account":"PAYER","currency":"EUR","amount_minor":2000}`. POST those fields and the returned `review` to `/v1/accountingd/offsets/confirm`. Both endpoints require the named operator’s finance and write grants. After a lost response, retry that exact confirmation. After HTTP 409, read the updated balances and obtain a new review. Each confirmed offset produces a single journal entry with both invoice numbers, both balances and the operator.

## Run and integrate

The daemon binds to `127.0.0.1:9591` by default. Set `ACCOUNTINGD_HTTP_BIND` to override it. Provision `ACCOUNTINGD_DATA_KEY` and `ACCOUNTINGD_SIGNING_KEY` through the estate keyring. Keep one writer for `accountingd.ndjson` and its signed head.

The finance API accepts JSON at `POST /v1/accountingd/balance`, `/v1/accountingd/import`, `/v1/accountingd/review` and `/v1/accountingd/confirm` using a named finance bearer. Balance requests accept `account`, `currency` and an optional zero-based `page`. Each collection returns up to 50 rows. The totals apply to the entire selected account and currency. Reviews take `account`, `currency`, `source`, `reference`, `number` and `amount_minor`. Confirm the same fields with the returned `review`. A stale selection returns 409.

Reading a balance, reviewing or posting a new allocation first catches up with the retail feed at `[services].billd`. If the owner is unreachable or disagrees with the retained checkpoint, the operation returns 503. An exact retry by the original operator returns its recorded posting receipt even during a billing outage. Statement import can also record bank evidence while billing is unavailable. An estate reader can witness the signed head at `GET /v1/accountingd/head`. Run `cargo test -p accountingd` or `just e2e` for the standalone workflow.

A finance operator examining a withdrawal can retrieve allocations for the selected invoice and its correction chain at `POST /v1/accountingd/withdrawal/evidence`. Supply `account`, `currency`, `number` and an optional zero-based `page`. Each result includes the original payment reference, allocated amount and confirming operator. A full billing credit clears the invoice’s open amount and retains any allocated cash as a payer credit. Any outgoing refund requires a separate payment process.

A `line_credit` feed record reduces the original invoice’s debt by its gross amount. Previously applied cash stays on that invoice. If the credit exceeds the open amount, the excess becomes a customer credit. A payer session at `POST /v1/accountingd/customer/balance` supplies one invoice number. Accounting checks live payer access through billing before returning invoice totals. Bank references and allocation history remain on the finance routes.
