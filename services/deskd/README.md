# deskd

<picture><source media="(prefers-color-scheme: dark)" srcset="../../site/static/img/heute-dark.webp"><img src="../../site/static/img/heute-light.webp" alt="Heute: the charging success rate, a refused session with the paragraph that refused it, and an invoice awaiting acceptance" width="1280"></picture>

The operator's page. **Heute** is what needs a person today, in order, headed by
the charging success rate — rendered on the server from the read models of the
daemons that made each decision. `deskd` decides nothing; the one thing it
keeps is the **witness** — its own record of where each daemon's journal ended.

## Status

| | |
|---|---|
| ✅ page | `GET /` — the success rate (estate, weakest points, failures by cause), then refused sessions with **"Warum?"**, stations silent beyond their heartbeat, records a partner has not taken, invoices awaiting acceptance, the obligation calendar's duties of the next 90 days, and **Journale** (below). German first, `?lang=en`. Works without JavaScript; `/ui/heute.js` (< 2 kB) refreshes it every 15 s while visible |
| ✅ station page | `GET /station/<identity>` — one station's asset card from `csmsd`'s station view: whether it is held for a card (reserved and cancelled from the card), its notes (written and removed from the card), online or silent since when, what it said at boot, what its meter signs and whether its key is registered, every firmware change (the `[MessEG §31(2) Nr. 4]` intervention record), its reports, success and last sessions. Every station name on the page links here |
| ✅ access | HTTP Basic whose password is one of the estate's `[[operators]]` tokens; binds `127.0.0.1:9587` unless `DESKD_HTTP_BIND`/`EMOB_HTTP_BIND` says otherwise (compose publishes it on `localhost:8088`) |
| ✅ witness | every 30 s reads the signed journal head of `csmsd`, `billd`, `roamd` and `empd` and records it in `witness.ndjson` under `EMOB_JOURNAL_DIR` — on deskd's own volume (`deskd:/var/lib/emob` in compose), never one of the daemons' |
| ✅ tested | render tests (order, escaping, bounds, a down daemon, de/en, a11y basics) and `tests/heute_reads_the_estate.rs`, which starts the real `empd`, `billd`, `roamd` and `csmsd` and holds the rate's numerator to the records `billd` received |

## What it reads

| From | What | Rendered as |
|---|---|---|
| `csmsd` `GET /v1/csmsd/today` | the success rate per day (`emob_session::success::Daily`): today and the last 7 days, refused sessions with their typed `findings` (`emob_eichrecht::Finding`), silent stations | "Heute" and "7 Tage", refused sessions and "Warum?", silent stations |
| `csmsd` `GET /v1/csmsd/stations/<identity>` | one station: presence, identity, meters, firmware history, reports, success, last sessions | the station page |
| `csmsd` `GET /v1/csmsd/stations`, `GET /v1/csmsd/map` | a page of the search and its map: points, or grid cells | "Stationen": the map (inline SVG over the bundled Natural Earth outline, `src/europe.rs`) and the table |
| `roamd` `GET /v1/roamd/today` | consignments not `accepted` | records a partner has not taken |
| `billd` `GET /v1/billd/today` | documents whose `submission` is not accepted | invoices awaiting acceptance |
| `emob_core::obligation` | `starting_between(today, today + 90 days)` | duties, with "in 44 Tagen" and the exact date on focus |
| `csmsd`, `billd`, `roamd`, `empd` `GET /v1/<name>/head` | each journal's signed head | "Journale" |

**At scale.** deskd reads only these bounded views (body cap 1 MiB), so the page costs the same on day 400 as on day 1: at one million settled sessions and 50 000 refusals, csmsd builds its view in under a millisecond at about 6.5 kB, and the page renders in under a millisecond at about 30 kB (`tests/heute_at_scale.rs`). Each long list ends in a plain "Ältere anzeigen" link — no script needed.

Each read has a 2 s timeout and an 8 MiB cap. A daemon that does not answer is
named in its section with since when; the rest of the page renders, and the
"nothing to do" sentence appears only when every daemon answered.

## Journale: the witness on Heute

A daemon refuses to start from a journal shorter than the head it signed — but
whoever can cut the journal can delete that head too, and a backup restore
brings an old one back. So deskd keeps a copy no daemon's volume holds. The
**Journale** section lists each daemon with its last witnessed position, the
time, and where the key came from: *Schlüssel aus dem Estate* (named in
`[witness] keys`) or *Schlüssel beim ersten Mal gemerkt* (pinned at first
sight).

A head that went backwards, forked (same position, another hash) or came under
another key is a red row naming both positions. It **stays** — the witness file
is a chained history, not a flag the next good head clears — until somebody
moves `witness.ndjson` aside, and while it stands the "nothing to do" sentence
is not shown. `poid` is not among the addresses deskd reads; witness it with
`emob witness`.

## Run it

```sh
export EMOB_ESTATE=deploy/estate.example.toml EMOB_OPERATOR_TOKEN=…   # and the daemons' secrets
just desk            # or: cargo run -p deskd
open http://127.0.0.1:9587/    # any user name, $EMOB_OPERATOR_TOKEN as the password
```

The estate's `[desk]` section names `csmsd`'s shell, the token variable and the
zone the page tells time in; `billd`, `roamd` and `empd` are read at their
`[services]` addresses. The optional `[witness] keys` names a daemon's verifier
key (what its head route sends in `Emob-Verifier-Key`); a daemon not named is
pinned the first time it is seen.

| Variable | Default | Purpose |
|---|---|---|
| `EMOB_ESTATE` | `./estate.toml` | `[desk]`, `[services]`, `[witness]`, `[[operators]]` |
| `EMOB_OPERATOR_TOKEN` | **required** | the variable `[desk] token_env` names |
| `DESKD_HTTP_BIND` / `EMOB_HTTP_BIND` | `127.0.0.1:9587` | the page |
| `EMOB_JOURNAL_DIR` | `var` | where `witness.ndjson` lives |
| `EMOB_LOG_FORMAT`, `RUST_LOG` | JSON, `info,deskd=debug` | log format and filter |

The fastest way to see it is `just demo`, which prints a signed-in Heute link.

## Built on

`emob-service` (the shell, `ui::operator_page`, the shared stylesheet, the
witness),
`emob-session` (the success tally), `emob-eichrecht` (typed findings) and
`emob-core` (the obligation calendar). See
[Run the estate](https://hupe1980.github.io/emob/docs/run-the-estate/) and the
[workspace README](../../README.md).

## License

MIT OR Apache-2.0
