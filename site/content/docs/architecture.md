+++
title = "Architecture"
weight = 4
description = "What is built, what is designed, how emob sits beside the protocol kits it consumes, and the roaming stance that decides how the rest gets written."
+++

# Architecture

## What exists today ✅

| Crate | Holds |
|---|---|
| `emob-core` | Identifiers in both grammars, text-preserving; exact energy and money; the charge-point profile; the obligation calendar |
| `emob-eichrecht` | OCMF parse and verify, the key registry, the session chain, the evidence record |

105 tests, no I/O, no clock, no binary floats. `just ci` green.

## What is designed 📐

| Crate | Holds |
|---|---|
| `emob-session` | Session lifecycle across every entry path — local RFID, roaming, ad-hoc, Plug & Charge, AutoCharge — with clock-aligned energy series |
| `emob-cdr` | CDR construction from session and evidence, inbound validation, dedupe, re-rating, disputes |
| `emob-tariff` | CPO and EMP tariffs, ad-hoc pricing, and display strings derived from the tariff that rates |
| `emob-roam` | The canonical ↔ wire translation layer and its cost notes; the partner registry |
| `emob-pnc` | Plug & Charge contracts, certificate pools, multi-PKI |
| `emob-poi` | Locations, the LSV/AFIR registry state machine, DATEX II export |
| `emob-smart` | Site load management, OCPP charging profiles, DER control, the § 14a guard, V2G |
| `emob-billing` | Rating → EN 16931 e-invoice → SEPA → double-entry postings |
| `emob-sim` | Virtual stations and vehicles, a roaming peer in-process, reference days with seeded faults |

Services — `csmsd`, `roamd`, `empd`, `pncd`, `poid`, `tarifd`, `billd`, `opsd`,
`agentd` and an optional edge `sited` — are all 📐.

## Standing on the kits

emob does not implement charging protocols. Five sibling workspaces already do,
and they are the hard sixty per cent of a platform:

| Sibling | What emob takes |
|---|---|
| `ocpp-kit` | OCPP 1.6J / 2.0.1 / 2.1 payloads, a sans-I/O engine, transport, the CSMS ledger |
| `ocpi-kit` | OCPI 2.1.1 / 2.2.1 / 2.3.0 models, client and server, hub pieces, the tariff engine |
| `oicp-kit` | Hubject, both halves, delta-sync, CDR pre-flight, and a mock hub for CI |
| `iso15118` | The EXI codec, the -2/-20 message sets, the Plug & Charge signature profile |
| `eebus` | The § 14a wire at a site's grid connection point |

The billing half comes from `billing`, `en16931` (+ formats), `sepa` and
`doubleentry`; the market-communication half — German pass-through charging
under NZR-EMob — from `mako`.

### Why `emob-core` has its own identifiers

`ocpi-kit` and `oicp-kit` each carry an `EvseId` that is correct for its own
wire. A platform that speaks both needs one type its handlers are written
against, or the translation layer becomes a web of conversions between two
vocabularies that already agree.

They do have to agree, though, and that is a test rather than a hope:
`oicp-kit` documents `DE*ABC*E123 == deabce123`, so `emob-core` asserts the same
equality. The two meet in the roaming layer, and an id that compares equal on
one side and not the other routes a session to nobody.

## The roaming stance 📐

One canonical model; every wire native; translation cost recorded.

| Wire | Partner | Via |
|---|---|---|
| OCPI 2.3.0 (2.2.1 / 2.1.1 translated at the edge) | peers, most hubs | `ocpi-kit` |
| OICP 2.3 | Hubject | `oicp-kit` |
| eMIP | GIREVE legacy | only if a partner forces it — GIREVE speaks OCPI |
| OCHP | e-clearing.net | not planned; the spec froze in 2016 |

Three rules carry over from the kits and become platform invariants:

**Nothing a peer sent is thrown away.** Unknown fields and enum values survive a
round trip, because a hub that damages a vendor extension is how real data gets
lost.

**Identity is text-preserving.** Identifiers compare canonically and write back
verbatim.

**Translation cost is reported.** A 2.2.1 partner has no `tax_included`; a 2.3.0
one requires it. Every crossing yields the value *and* a note of what was
assumed. Disputes are won with that note.

## Purity, and why it is a build failure

The domain crates never read a clock, open a socket or touch the filesystem.
Every instant is an argument and the key registry is handed in already
populated.

That is not architectural taste. A dispute about a session from two years ago is
answered by replaying the verification exactly as it ran — same records, same
keys, same instant — and the replay stops being a replay the moment any part of
it consults the ambient world. `just purity` fails the build if a domain crate
reaches for one.

## The guards

| Guard | Prevents |
|---|---|
| `no-floats` | a workspace exact everywhere it was reviewed and approximate in the helper nobody read |
| `check-citations` | a rule citing a document nobody can produce |
| `check-manifests` | a `cargo publish` that fails after the version is spent |
| `purity` | a domain crate that cannot be replayed |
