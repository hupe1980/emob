+++
title = "Plug & Charge"
weight = 8
description = "ISO 15118 contracts from the platform's side: which ecosystem a certificate belongs to, and whether anything on its path has been withdrawn."

[extra]
state = "built"
nav = "Plug & Charge"
+++

A driver plugs in and the car identifies itself. No card, no app, no QR code: the
vehicle presents a **contract certificate**, the station checks it, and a session
starts. That is ISO 15118 Plug & Charge, and by 8 January 2026 every newly
installed or comprehensively renovated public AC point in the Union has to
implement the generation it belongs to `[DA-656 Anh. 2.1.1]`.

The protocol half of that is somebody else's problem, and solved. The EXI codec,
the -2 and -20 message sets, the session engines and the XML-signature profile
are [`iso15118`](https://crates.io/crates/iso15118)'s; they run on a station and
on an ECU, and re-stating any of them here would be a second implementation of
something that already round-trips against the reference encoder.

What is here is everything the protocol **assumes somebody else did** — which is
where a deployment actually goes wrong.

## A chain that validates is not a chain that belongs to you

`[V2G2-925]` reads like a formality:

> \[a leaf\] shall be treated as invalid, if the trust anchor at the end of the
> chain does not match the specific root certificate required for a certain use.

With one Plug & Charge ecosystem it is unobservable. There is one set of trust
anchors, every chain that validates validates against the right one, and a global
trust store is indistinguishable from a correct implementation.

With two it is the entire security property — and the failure is silent in a way
no test written against one pool can reach. A contract minted in another
ecosystem **validates**. The mathematics is sound. Every signature checks. Every
Annex F profile holds. And the driver has no relationship with this platform.
Nothing errors: a session starts, a CDR is written, and an invoice goes to a
provider that never heard of them.

So there is no global trust store here. Anchors live inside a pool, the pool is
named at the call, and the validation cannot reach another one's:

```rust
let platform = PoolRegistry::new()
    .with("hubject-ccp",    Pool::new().with(TrustAnchor::unbounded(root,  "over OFTP2")))
    .with("another-scheme", Pool::new().with(TrustAnchor::unbounded(other, "…")));

// Impeccable in the ecosystem that minted it…
verify_contract(&platform, &PoolId::new("another-scheme"), &chain, &status, at, None)?;
// …and reaches nothing in this one.
verify_contract(&platform, &PoolId::new("hubject-ccp"),    &chain, &status, at, None)  // Err
```

The test that proves it validates the foreign chain **in its own ecosystem
first**, because otherwise the refusal in this one would be a statement about the
certificate rather than about the pool.

## The identifier comes off the certificate

`[V2G2-108]` puts the EMAID in the leaf's Common Name and Annex F Table F.4 says
the same, so that is where it is read — parsed through the same type that
verifies a contract identifier's check digit anywhere else in this workspace.

Nothing in the verification takes an eMAID as an argument. A platform that
accepted the identifier and the certificate separately would attribute the
session to whichever contract the caller named, and the certificate would be
decoration.

The vehicle's identifier is the same rule one object along. `[V2G2-933]` makes
the provisioning certificate's Common Name the `CertID`, which
`[OPCP §Handling of IDs]` spells **PCID**: a World Manufacturer Identifier and
fourteen characters, seventeen or eighteen in total. The WMI is a method rather
than a slice at a call site because it decides authority — the Provisioning
Certificate Pool checks it against the OEM account's own list before it stores
anything, and the Contract Certificate Pool pushes a new contract to the OEM
enrolled for it.

## Revocation: three answers, not two

The path validation says what it establishes and, in the same paragraph, what it
does not: *that no certificate on the path has been revoked*. That is correct for
a `no_std` crate that opens no sockets, and it makes revocation the caller's —
which here means it is a rule and needs a type.

The type that gets it wrong is a `bool`. *Revoked* and *not revoked* leaves
nowhere to put **nobody could say**, and there are two of those: this side holds
no status for the issuer at all, and this side holds one older than the operator
will vouch for. Both end up in *not revoked* every time, because that is the
answer that lets the session start.

| | |
|---|---|
| `Good` | a responder said so, recently enough to mean it |
| `Revoked` | a responder said so |
| `Unknown` | this side holds no status at all — which is not `Good` |
| `Stale` | this side holds one and it is older than the operator vouches for |

Two details come from the specification rather than from the shape.
`[OPCP §Provisioning Certificate Pool]` asks the question of *"each delivered
certificate (leaf **and chain**)"* — a leaf is one driver and a withdrawn sub-CA
is every driver under it. And a withdrawn **root** deletes nothing: the
specification spends a paragraph saying an operator *"should not delete all its
certificates, and their contract data"*, which is precisely the cascade an
implementation invents. The one cascade that does run — a deleted provisioning
certificate taking its contracts with it — is the one the specification
describes.

## One vehicle, several contracts

`[OPCP §Contract Certificate Pool]` *"enables Multiple contracts storing for each
PCID"*. A car with a lease contract and a private one has two, from two
providers — and the shape that gets this wrong is a map from PCID to contract,
which is what a platform writes on the day it has one of each. The book is keyed
by **(pool, eMAID)** and carries the PCID as a field, which is also the key the
ecosystem updates on: *"the update process overwrites the existing Contract
Certificate with the same EMAID."*

## The claim that needs two independent parties

A contract certificate establishes **who**. A signed meter record establishes
**how much** — and, in its own signed bytes, *how strongly the driver was
identified* `[OCMF Tab. 11]`.

Those are two claims about one session, made by two parties, and the whole reason
an identification strength is shared vocabulary is that they can then be
compared. `[OCMF Tab. 15]`'s `ISO15118_PNC` is Table 11's own example of
`SECURE`, and `SECURE` is exactly what a Plug & Charge authorisation path is
allowed to claim. The end-to-end test asserts both, with neither side told the
other's answer:

```text
contract certificate  ──► EMAID off the Common Name  ──► AuthPath::PlugAndCharge
                                                              │ ceiling: SECURE
virtual station signs ──► IL: SECURE, IF: ISO15118_PNC  ──────┘ record says: SECURE
```

A session recorded as DIN SPEC 70121 and authorised by a contract certificate is
the same comparison failing: DIN is external identification only, so the pair is
a claim no charger can have made. Each half is an ordinary value; only the pair
is a contradiction, which is why the check lives on the session.

## Certificates this workspace did not mint

Every path validation checked only against DER the same repository produced is
checked against its own opinions. So the fixtures are **OpenSSL's**, minted by a
script with absolute validity windows so nothing rots — and beside them, where
the working copy has the archive, the tests run Hubject's own published open
Plug & Charge test environment: a real V2G Root, real OEM sub-CAs and a real
provisioning certificate, validated against ISO 15118's Annex F profiles by this
workspace's own elliptic-curve code.

Those certificates expired in 2022. That they still validate is the property the
whole platform rests on: **the instant is an argument**, so a question about a
session two years old is the same call as one about today.

## No I/O, no clock

The four `[OPCP]` services — the root pool, the provisioning pool, the signing
service and the contract pool — are all on the far side of a socket. A revocation
status is a value a caller supplies with the instant it was compiled at; a
validation takes the instant to judge against. `pncd`, the daemon that makes
those four HTTP calls, is 📐.

## What is not here 📐

Minting anything. A contract certificate is signed by a Mobility Operator's CA
through the Certificate Provisioning Service, and a platform that could mint one
is a platform whose separation of duties is a comment. This side verifies.
