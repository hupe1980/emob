# emob-cli

The `emob` command line: `emob verify` checks a charging receipt's signed meter
values offline, `emob demo` runs the whole estate on your machine, and
`emob witness` keeps an independent record of where each daemon's journal ends.

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

## `emob demo`

```console
$ just demo            # = just dev-ca (once) + cargo run -q -p emob-cli -- demo
emob demo — Stadtwerke Musterstadt eMobility · seed 2026 · 60× · from 2026-06-01 10:00 Europe/Berlin
  ✓ empd billd roamd csmsd deskd up · CP-1 (1.6) CP-2 (2.0.1) CP-3 (2.1) on wss://127.0.0.1:9000 with profile 2 · partner DE*MSP registered

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

The whole estate in one process, on loopback: `empd`, `billd`, `roamd`,
`csmsd` and `deskd` started through the `daemon::run` their own `main` calls,
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
Resources → Network). The image is published from the 0.3 release on; before
it, the pull is denied.

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
