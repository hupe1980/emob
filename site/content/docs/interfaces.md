+++
title = "Replace an integration"
description = "The HTTP, event and MCP contracts, who serves each one, and how to verify a replacement."
weight = 13
+++

Download the [OpenAPI document](../../schemas/openapi.json) to inspect the endpoints, request bodies, authentication and responses. Keep its sibling schema files beside it when resolving references offline. Each tagged release bundles the complete directory in `emob-interfaces.tar.gz`. Read `interface-checks.txt` for results from the running estate and `image-checks.txt` for results from the packaged CLI and tariff service.

| I need to… | Contract | Reference implementation |
|---|---|---|
| Answer a charging authorization | `POST /v1/authorize` with the CSMS service bearer | `empd` |
| Supply a public driver tariff | `GET /v1/driver-tariffs/{id}`, optionally followed by its version fingerprint | `tarifd` |
| Receive a settled charge detail record | Signed `de.emob.cdr.issued` at the configured callback URL | `csmsd` sends to subscribers, `billd` and `roamd` |
| Receive an issued invoice | Signed `de.emob.invoice.issued`, with the XML document in `data` | `billd` sends to subscribers |
| Recover one missed delivery | Signed `POST /v1/subscriber-events/recover` with its original event ID | `csmsd` for charge records, `billd` for invoices |
| Verify a meter signature offline | MCP tool `verify_meter_record`, over local stdio | `emob mcp` |

The authorization, tariff and delivery contracts allow another implementation to serve the same clients. Browser pages use their own staff or customer credentials. Stations and roaming partners use their respective OCPP and OCPI protocol interfaces.

## Check a replacement

Install `emobtest` and point the appropriate probe at your service. Use dedicated fixture credentials and known test cards. Each check reports the observed behavior and exits nonzero if the contract fails.

```sh
emobtest conformance authorize "$PROVIDER_URL" --token-env SERVICE_TOKEN --known "$ACTIVE_CARD" --blocked "$BLOCKED_CARD"
emobtest conformance tariff "$TARIFF_URL" --tariff-id ad-hoc
emobtest conformance receiver "$CDR_CALLBACK" --secret-env CDR_SECRET
emobtest conformance receiver "$INVOICE_CALLBACK" --secret-env INVOICE_SECRET --event-type de.emob.invoice.issued
emobtest conformance mcp emob mcp
```

Check the signature and timestamp over the exact request bytes before processing an event. Use the CloudEvent source and ID for deduplication. Book a genuine delivery once. A signed sample carries the `emobtest` extension and should be acknowledged without booking. The event schemas describe both formats. [Charge detail record](../../schemas/events/de.emob.cdr.issued.json) · [Issued invoice](../../schemas/events/de.emob.invoice.issued.json)

## Recover an event by ID

Point the recovery client at the issuer and name the environment variable holding your subscriber’s signing secret. On success, stdout contains the verified CloudEvent. A failure exits nonzero and sends the reason to stderr.

```sh
emobtest recover "$BILLING_URL" --subscriber warehouse \
  --event-type de.emob.invoice.issued --id "$INVOICE_NUMBER" \
  --secret-env WAREHOUSE_SIGNING_SECRET > recovered.json
```

The request uses a fresh signature. The response carries its own signature, original event ID and business document. Receipt and invoice-page links reflect the configured public origin. Reading an event leaves delivery acknowledgements and the books untouched.

| Response | What to check |
|---|---|
| 400 | Send the four fields described by OpenAPI, including the `recover` purpose. |
| 401 | Check the subscriber name, key, signature and clock. The freshness window is five minutes. |
| 404 | Check the issuer, event type and original ID. The subscriber must have been a recipient at issue and still subscribe to this type. |
| 413 | The recovery request exceeds 4096 bytes. Send the ID in the request and read the document from the response. |

Give each subscriber a distinct secret and stable name. Rotate its key when credentials change. Assign a new name when the recipient organization changes, because the historical recipient is recorded by name.

## Connect an MCP client

Set the server command to `emob` and its arguments to `["mcp"]`. Supply a signed OCMF record and an independently trusted meter public key. The tool returns a structured verification result. It reads its arguments over stdio and needs no estate credentials. [CLI configuration](https://github.com/hupe1980/emob/tree/main/crates/emob-cli#emob-mcp)

## Verify the source checkout

Run `just interfaces` with Rust, Python, uv, OpenSSL and just installed. It builds the current kit, validates the contracts, starts a throwaway estate, exercises the replacement ports and checks MCP. The release workflow also builds every Bake target and runs the MCP probe against the packaged CLI with networking disabled and checks current and versioned tariff responses from the tariff service image.
