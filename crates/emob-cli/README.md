# emob-cli

The `emob` command line: `emob check` refuses an estate before a daemon reads
it, `emob verify` checks a charging receipt's signed meter values offline,
`emob demo` runs a seven-service charging demo on your machine, `emob witness` keeps an
independent record of where each daemon's journal ends, and `emob subscribers
test` proves an event receiver before the first real record reaches it.

```console
$ emob verify transparenz-1.xml
transparenz-1.xml: 2 signed value(s)
  ✓  1  B 10.000 kWh 2026-01-02T10:00:00,000+0100 S
        verified: this key signed exactly these bytes
  ✓  2  E 25.000 kWh 2026-01-02T10:30:00,000+0100 S
        verified: this key signed exactly these bytes
2 verified, 0 not verified, 0 not checkable here
```

It reads the S.A.F.E. transparency file a receipt page offers for download and
checks every value against the key the file carries, offline, with
`emob_verify::check` — the same function the receipt page runs in the browser.

Exit status: `0` all verified · `1` one did not verify · `2` one could not be
checked here (brainpool/secp192k1, a non-OCMF value, no key) · `64` usage ·
`66` unreadable file.

The file says which key signed; whether that is the key printed on the charge
point is a comparison only the driver can make. `publish = false`.

The demo prints a two-driver fleet invoice and an agreement for reviewing a line credit. The private payer has a separate issued agreement and invoice. Use the agreement reference in Heute to review a withdrawal. Use **Customer erasure** to retire the demo customer’s portal access and reopen its receipt. [Operator walkthrough](../../site/content/docs/run-the-estate.md#review-a-withdrawal)

## `emob mcp`

Configure a local MCP host to launch the `emob` executable with the argument `mcp`. For a host using `mcpServers` configuration:

```json
{
  "mcpServers": {
    "emob": { "command": "emob", "args": ["mcp"] }
  }
}
```

Ask the host to verify one signed meter record. The `verify_meter_record` tool takes the verbatim OCMF string as `record` and the independently obtained meter key as `public_key`. Copy the public key from the charge point or another trusted source. The tool checks the signature with the same offline verifier as the receipt page. Its structured answer gives `verified`, `mismatch` or `inconclusive` and a short explanation. A mismatched key or altered signature produces `mismatch`. Unsupported curves and unreadable inputs produce `inconclusive`.

For an isolated container, configure the host command as `docker` with arguments `["run", "--rm", "-i", "--network", "none", "ghcr.io/hupe1980/emob:<version>", "mcp"]`, replacing `<version>` with the selected image tag. Keep stdin attached with `-i` and leave the terminal unallocated so the host receives the protocol bytes.

The host communicates over standard input and output. The verifier reads only the supplied strings, performs the check locally and returns the verdict without echoing the record or key. It needs no estate configuration. The record accepts up to 65,536 characters, the public key up to 1,024, and a protocol message up to 512 KiB. The MCP host's own policy determines whether it sends the supplied record to a model.

Check an installed server from another process:

```console
emobtest conformance mcp emob mcp
```

## `emob demo`

```console
$ just demo            # = just dev-ca (once) + cargo run -q -p emob-cli -- demo
emob demo — Stadtwerke Musterstadt eMobility · seed 2026 · 60× · from 2026-06-01 10:00 Europe/Berlin
  ✓ empd billd roamd csmsd deskd portald accountingd up · CP-1 (1.6) CP-2 (2.0.1) CP-3 (2.1) on wss://127.0.0.1:9000 with profile 2 · partner DE*MSP registered

  Heute     http://demo:<token>@127.0.0.1:9587/        (operator page, signed in)
  10:22  CP-1 · 1.6    ✓ billed   26.984 kWh · 13.22 EUR · DE*MUS/CP-1-1

  Receipt   http://127.0.0.1:9001/r/<token>   (what the driver's phone opens)
  …
  10:44  CP-2 · 2.0.1  ✕ refused  record 5: signature does not verify against this key [MessEG §33]
  …
  ⏩ month end (2026-07-01): billd issues invoice R-2026-0001 to MSP Mobility GmbH — 7 lines, 85.78 EUR incl. VAT (72.08 net)

  Invoice   http://127.0.0.1:9584/i/<token>   (the bill its receipts are on, with its XRechnung)
  🔏 witness: csmsd ✓ N · billd ✓ N · roamd ✓ N · empd ✓ N
```

The customer portal line gives the sign-in ID `mika` and the path to its private temporary key file. The customer can open an invoice for a simulated charge.

The seven demo services run in one process on loopback: `empd`, `billd`, `roamd`,
`csmsd`, `deskd`, `portald` and `accountingd` started through the `daemon::run` their own `main` calls,
with a generated estate file; three `ocpp-kit` stations — OCPP 1.6, 2.0.1 and
2.1 — over `wss://` with Basic auth (security profile 2, `just dev-ca`'s
throwaway CA); `ocpi-kit`'s `MockPeer` as the roaming partner, whose inbox is
the third link. One simulated clock (`--clock`, `--speed`, default 60×) drives
the stations and the page; one seed (`--seed`, default 2026) decides the meters
and the energies, so the same seed gives the same records and invoice number.

CP-3 (OCPP 2.1) opts in to `receipt_display_minutes = 10`, so the demo prints
`📟 its display shows "Beleg: <receipt link>" for 10 minutes`.

`--check` plays the story at full speed and exits `1` naming every outcome that
differs:

- seven sessions billed and at the partner, the tampered one refused naming the
  signature, the blocked card;
- one invoice holding exactly the billed records, whose page, UBL and CII files
  are fetched and the files validated with `en16931`;
- CP-3 received each of its receipt links on its display, CP-1 and CP-2 none;
- every journal record names an actor, and the witness saw every head;
- every log line is JSON with a request id on each request, and none holds a
  token, secret, key, password, card UID or receipt token.

`--listen` takes a loopback address only; `--links FILE` writes the printed
links for `just screenshots` and `just a11y`; every token, key and password is
minted for the run and gone with it.

### Without Rust: the `emob` image

`just demo` on a machine without `cargo` runs the released
`ghcr.io/hupe1980/emob` image instead:

```console
docker run --rm -it --network host --user "$(id -u):$(id -g)" \
  -v "$PWD/target/dev-tls:/tls:ro" ghcr.io/hupe1980/emob demo --tls /tls
```

Host networking, because the demo binds loopback only and the links it prints
are the host's (on Docker Desktop, enable host networking under Settings →
Resources → Network).

## `emob check`

```console
$ emob check deploy/estate.example.toml
✓ deploy/estate.example.toml: 1 station, 2 tariffs, 1 partner, 2 operators — every daemon would start
```

Every section against the whole estate (one no daemon reads is refused), then
each daemon's own checks — `csmsd`'s fleet and keys, `roamd`'s locations,
`poid`'s sites, `tarifd`'s tariffs, subscribers and operators. A mistake is
reported like `rustc` reports one, with the line and `= help:`. `--secrets`
also checks the resolved operator and service credentials for value reuse. The
desk needs a named read-only credential. Every `*_env` variable must be set;
`--schema` prints the estate's JSON Schema. Exit `0` passes, `1` refused, `2`
unreadable.

## `emob subscribers test`

```console
$ emob subscribers test warehouse
✓ → https://dwh.example.com/emob/events de.emob.cdr.issued … 204 No Content (83 ms)
```

One signed sample of every type the `[[subscribers]]` entry takes, sent as the
daemons send the real one, marked `emobtest: true` with an id starting `test-`.
Nothing is journalled. Exit `1` when one answer is not `2xx`.

## `emob check-feed`

```console
$ emob check-feed table.json
table.json: 1 publication(s), JSON in its envelope
  ✓ publication 1: the Mobilithek profile takes it
✓ table.json: the Mobilithek profile takes every publication
```

Any DATEX II AFIR file, XML or JSON, emob's or another operator's: every
deviation the reader tolerated is listed, and each publication is held to the
Mobilithek profile by the writer `poid` publishes with, which names the member
and the rule it breaks. Exit `1` when the profile refuses one.

## `emob qr`

```console
$ emob qr deploy/estate.example.toml 'DE*ABC*E00001' > sticker.svg
```

The QR code of a charge point's price page, `<receipts.public_url>/p/<evse>`,
as SVG with the address in plain text beneath it, for a sign at the point. It
carries no Preisstand, because that changes with the price; the station's
display and the page show it.

## `emob witness`

```console
$ EMOB_OPERATOR_TOKEN=… emob witness http://127.0.0.1:9001 http://127.0.0.1:9584
✓ csmsd      57  first sight — key emob/csmsd+1a2b3c4d pinned
✓ billd      12  (was 9)
```

Each daemon signs where its journal ends after every append and serves it at
`GET /v1/<name>/head`. A daemon refuses to start from a journal shorter than
that head — but whoever can cut the journal can delete the head too, and a
backup restore brings an old one back. `emob witness` is a copy no daemon's
volume holds: for each URL it reads `/about` for the name and the head with
`EMOB_OPERATOR_TOKEN`, pins the verifier key at first sight, and records every
observation in a chained file (`--file PATH`, default `./witness.ndjson`). Run
it from cron on another machine, or against `poid`, which deskd does not watch.

A head that went backwards (records removed, or an older copy restored), forked
(the same position with another record) or came under another key is an alarm,
and it stays in the file until you move the file aside.

Exit status: `0` every head witnessed · `1` an alarm, now or recorded earlier ·
`2` a daemon could not be read · `64` usage.

## License

MIT OR Apache-2.0. Part of [emob](../../README.md); see
[Run the estate](https://hupe1980.github.io/emob/docs/run-the-estate/).
