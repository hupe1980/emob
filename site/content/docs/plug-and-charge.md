+++
title = "Plug & Charge"
weight = 9
description = "ISO 15118-2 and -20 Plug & Charge for CPOs and EMPs: contract chains verified under named PKI roots, the eMAID read off the certificate, revocation status."

[extra]
state = "built"
nav = "Plug & Charge"
+++

**Plug & Charge** (ISO 15118) lets a driver plug in and start charging with no
card or app: the vehicle presents a **contract certificate**, the backend
verifies it, and the session is billed to the contract. From 8 January 2026
every newly installed or comprehensively renovated public AC point in the EU must
implement it `[DA-656 Anh. 2.1.1]`. `emob-pnc` is the platform side: it checks
**which PKI ecosystem** a certificate belongs to, reads the contract identifier
off the certificate, and decides whether anything on the path has been withdrawn.
From 1 January 2027 a point with automatic authentication must support both
ISO 15118-2 and ISO 15118-20 `[DA-656 Anh. 2.1.2]`, so `emob-pnc` validates
both generations' contract chains.

The protocol side — the EXI codec, the ISO 15118-2 and -20 message sets, the
session engines and the XML-signature profile — is
[`iso15118`](https://crates.io/crates/iso15118) 0.9, which runs on stations and
vehicle ECUs. `emob-pnc` covers what the protocol assumes somebody else did.

## Verify a contract certificate against one named ecosystem

`[V2G2-925]` says a leaf *"shall be treated as invalid, if the trust anchor at
the end of the chain does not match the specific root certificate required for a
certain use."* With one Plug & Charge ecosystem this is unobservable. With two,
it is the whole security property: a contract minted in another ecosystem
**validates** — every signature and every Annex F profile holds — yet the driver
has no relationship with this platform, and nothing errors until an invoice goes
to a provider that never heard of them.

So there is no global trust store. Trust anchors live inside a named `Pool`, and
every verification names the pool it runs in:

```rust
use emob_pnc::{Generation, Pool, PoolId, PoolRegistry, TrustAnchor, verify_contract};

let platform = PoolRegistry::new()
    .with("hubject-ccp",    Pool::new().with(TrustAnchor::unbounded(root,  "over OFTP2")))
    .with("another-scheme", Pool::new().with(TrustAnchor::unbounded(other, "…")));

// Valid in the ecosystem that minted it…
verify_contract(&platform, &PoolId::new("another-scheme"), Generation::Iso2, &chain, &revocations, at, None)?;
// …and reaches nothing in this one.
verify_contract(&platform, &PoolId::new("hubject-ccp"), Generation::Iso2, &chain, &revocations, at, None) // Err
```

The test validates the foreign chain in its own ecosystem first, so the refusal
proves something about the pool rather than about the certificate.

## ISO 15118-2 and ISO 15118-20: the generation is an argument

`verify_contract`, `verify_provisioning` and `PoolRegistry::validate_in` take a
`Generation` — `Generation::Iso2` or `Generation::Iso20` — and the verified
result carries it. The **caller** names it per call: it is the protocol the
session negotiated (OCPP 2.x carries it beside the chain as
`iso15118SchemaVersion`). Reading it off the certificate would let whoever
minted the leaf choose the weaker rules.

| | ISO 15118-2 | ISO 15118-20 |
|---|---|---|
| Curve | secp256r1 only `[V2G2-007]` | secp521r1 with `ecdsa-with-SHA512` |
| Ed448 | refused by name | refused by name |
| A chain of the other generation | refused | refused |

A secp521r1 chain judged as -2 would be a -20 chain escaping -20's rules, so it
is refused; a -2 chain under -20 is refused by the path validation. Ed448 is
allowed by ISO 15118-20, but its only Rust verifier sits on a newer generation
of the RustCrypto libraries than this workspace uses. So an Ed448 leaf is
refused before any signature is checked, with `PncError::UnsupportedAlgorithm`
naming the pool, the generation and the curve: an unverifiable signature is
never read as a forged one, or as a good one. A -20 contract on Ed448 cannot be
authorised until that changes.

### Roots are configuration

No production root ships with emob: each `TrustAnchor` is configured with its
provenance, inside the pool it belongs to. As of November 2026 none of the
three European Plug & Charge ecosystems — Hubject, GIREVE with Thales, CharIN
with Irdeto — publishes an ISO 15118-20 root, and no European Certificate Trust
List exists. The -20 tests therefore mint one root per ecosystem with OpenSSL
(`emob_pnc::fixtures::iso20`: `hubject-test`, `gireve-test`, `charin-test`, plus
an Ed448 set to prove the refusal). Each root validates its own contract and
refuses the other two's — all three contracts carry the same eMAID, so the
test shows the **root**, not the identifier, deciding.

## The eMAID comes off the certificate

The eMAID — the e-mobility account identifier naming a driver's contract — sits
in the leaf certificate's Common Name `[V2G2-108]` (and Annex F Table F.4). emob
reads it from there and checks its check digit with the same type used everywhere
else. Nothing in the verification accepts an eMAID as an argument: a platform
that took the identifier and the certificate separately would bill whichever
contract the caller named.

The vehicle's identifier follows the same rule. `[V2G2-933]` makes the
provisioning certificate's Common Name the `CertID`, which `[OPCP §Handling of IDs]`
calls the **PCID**: a World Manufacturer Identifier (WMI) plus fourteen
characters, seventeen or eighteen in total. `Pcid::wmi` matters for authority:
`emob_pnc::may_publish_for` checks the WMI against the OEM account's own list
before the Provisioning Certificate Pool stores anything.

## Certificate revocation: four answers, not a bool

`iso15118`'s path validation fetches nothing — it is `no_std` and opens no
sockets — so revocation is the caller's question. A `bool` gets it wrong:
*revoked / not revoked* leaves nowhere for **nobody could say**, and that case
always ends up as "not revoked" because that answer lets the session start.
`RevocationList::standing_of` returns a `Standing`:

| Standing | Meaning |
|---|---|
| `Good` | a responder said so, recently enough to mean it |
| `Revoked` | a responder said so |
| `Unknown` | this side holds no status for the issuer — which is not `Good` |
| `Stale` | the status held is older than the operator vouches for (`DEFAULT_FRESHNESS`) |

Two details come from the specification:

- `[OPCP §Provisioning Certificate Pool]` checks *"each delivered certificate
  (leaf **and chain**)"*: a revoked leaf is one driver, a revoked sub-CA is every
  driver under it.
- A withdrawn **root** deletes nothing. The specification says an operator
  *"should not delete all its certificates, and their contract data"*. The one
  cascade that does run — a deleted provisioning certificate taking its contracts
  with it (`ContractBook::retire_vehicle`) — is the one the specification
  describes.

## One vehicle, several contracts

`[OPCP §Contract Certificate Pool]` *"enables Multiple contracts storing for each
PCID"* — a car with a lease contract and a private one holds two, from two
providers. A map from PCID to contract breaks that. `ContractBook` is keyed by
**(pool, eMAID)** and carries the PCID as a field, matching how the ecosystem
updates: *"the update process overwrites the existing Contract Certificate with
the same EMAID."*

## Cross-checking the certificate against the signed meter record

A contract certificate establishes **who**. A signed OCMF meter record
establishes **how much** and, in its own signed bytes, **how strongly the driver
was identified** `[OCMF Tab. 11]`. `[OCMF Tab. 15]`'s `ISO15118_PNC` is
Table 11's example of `SECURE`, which is exactly what a Plug & Charge path may
claim. The end-to-end test asserts both halves, with neither told the other's
answer:

```text
contract certificate  ──► EMAID off the Common Name  ──► AuthPath::PlugAndCharge
                                                              │ ceiling: SECURE
virtual station signs ──► IL: SECURE, IF: ISO15118_PNC  ──────┘ record says: SECURE
```

A session recorded as DIN SPEC 70121 yet authorised by a contract certificate is
a contradiction — DIN 70121 supports external identification only — so the check
lives on the session, where both halves meet. See
[Sessions and settlement](@/docs/settlement.md) for the full table of
authorisation paths.

## Tested against certificates emob did not mint

Validation checked only against DER the same repository produced is checked
against its own opinions. The fixtures are minted by **OpenSSL** with absolute
validity windows so they never rot. Where the working copy has the archive, the
tests also run Hubject's published open ISO 15118-2 Plug & Charge test PKI — a real V2G Root,
real OEM sub-CAs and a real provisioning certificate — against ISO 15118's
Annex F profiles.

Those certificates expired in 2022 and still validate, because **the instant is
an argument**: a question about a two-year-old session is the same call as one
about today.

## No I/O, no clock

The four `[OPCP]` services — root pool, provisioning pool, signing service and
contract pool — are behind sockets. A revocation status is a value the caller
supplies together with the instant it was compiled; a validation takes the
instant to judge against. `pncd`, the daemon that will make those HTTP calls, is
📐 designed.

## What emob-pnc does not do

It never **mints** a certificate. Contract certificates are signed by a Mobility
Operator's CA through the Certificate Provisioning Service; a platform that could
mint one would have separation of duties only on paper. This side verifies.

**Related:** [The obligation calendar](@/docs/compliance.md) · [Eichrecht](@/docs/eichrecht.md) ·
[Sessions and settlement](@/docs/settlement.md) ·
[Roaming](@/docs/roaming.md) · [Glossary](@/docs/glossary.md)
