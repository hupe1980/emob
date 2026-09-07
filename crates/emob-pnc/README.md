# emob-pnc

**ISO 15118 Plug & Charge, from the platform's side**: which ecosystem a
contract belongs to, whether anything on its path has been withdrawn, and which
vehicle it was installed against.

```console
cargo add emob-pnc
```

## It speaks to no vehicle, and that is the point

There is no EXI codec here, no message set, no session engine, no TLS. Those are
[`iso15118`](https://crates.io/crates/iso15118)'s, they run on a charging station
and on an ECU, and re-stating any of them would be a second implementation of
something that already round-trips against the reference encoder.

What is left is everything the protocol assumes somebody else did — which is
where a Plug & Charge deployment actually goes wrong.

## A chain that validates is not a chain that belongs to you

`[V2G2-925]` reads like a formality:

> \[a leaf\] shall be treated as invalid, if the trust anchor at the end of the
> chain does not match the specific root certificate required for a certain use.

With one Plug & Charge pool it is unobservable. With two it is the whole security
property, because a contract minted in another ecosystem **validates**: the
mathematics is sound, every signature checks, every Annex F profile holds — and
the driver has no relationship with this platform. Nothing fails. A session
starts, a CDR is written, and an invoice goes to a provider that never heard of
them.

So there is no global trust store in this crate. Anchors live inside a pool, the
pool is named at the call, and the validation cannot reach another one's:

```rust
let platform = PoolRegistry::new()
    .with("hubject-ccp",    Pool::new().with(TrustAnchor::unbounded(root, "over OFTP2")))
    .with("another-scheme", Pool::new().with(TrustAnchor::unbounded(other, "…")));

// Impeccable in the ecosystem that minted it…
verify_contract(&platform, &PoolId::new("another-scheme"), &chain, &status, at, None)?;
// …and reaches nothing in this one.
verify_contract(&platform, &PoolId::new("hubject-ccp"),    &chain, &status, at, None)  // Err
```

## The identifier comes off the certificate

`[V2G2-108]` puts the EMAID in the leaf's Common Name, so that is where it is
read — parsed through `emob_core::Emaid`, check digit and all. Nothing in
`verify_contract` takes an eMAID as an argument, because a platform that accepted
the identifier and the certificate separately would attribute the session to
whichever contract the caller named and the certificate would be decoration.

The same for the vehicle: `[V2G2-933]` makes the provisioning certificate's
Common Name the `CertID`, which `[OPCP §Handling of IDs]` spells **PCID** — a
World Manufacturer Identifier and fourteen characters — and `Pcid::wmi()` is a
method rather than a slice at the call site because it decides authority.

## Revocation: three answers, not two

`iso15118::pnc::pki::validate` says what it does not establish, and this is it.
`[OPCP §Provisioning Certificate Pool]` makes it a control step all the same, and
asks it of *"each delivered certificate (leaf and chain)"* — a check that looks
only at the contract certificate passes a contract issued under a sub-CA the
ecosystem withdrew this morning.

The shape this crate refuses is a `bool`. *Revoked* and *not revoked* leaves
nowhere for the case that actually happens — **nobody could say** — and it ends
up in *not revoked* every time, because that is the answer that lets the session
start. So there are four standings and one of them authorises:

| | |
|---|---|
| `Good` | a responder said so, recently enough to mean it |
| `Revoked` | a responder said so |
| `Unknown` | this side holds no status at all — which is not `Good` |
| `Stale` | this side holds one and it is older than the operator vouches for |

A status is a claim about an instant, so a `RevocationList` carries the instant
it was compiled and the freshness window is the operator's. A list from March
consulted in September is not evidence that a contract cancelled in June is
still good.

## One vehicle, several contracts

`[OPCP §Contract Certificate Pool]` *"enables Multiple contracts storing for each
PCID"*. A car with a lease contract and a private one has two, from two
providers — and the shape that gets this wrong is a map from PCID to contract,
which is what a platform writes on the day it has one of each. `ContractBook` is
keyed by **(pool, eMAID)** and carries the PCID as a field.

Deletion runs one way. Retiring a vehicle takes its contracts, because the
specification says it does; a withdrawn **root** takes nothing, because the
specification spends a paragraph saying it must not.

## No I/O, no clock

The four `[OPCP]` services are all on the far side of a socket. A revocation
status is a value a caller supplies with the instant it was compiled at; a
validation takes the instant to judge against. Both are arguments for the reason
every instant in this workspace is one — a session disputed two years later is
answered by asking the question exactly as it was asked then.

## The crypto is the workspace's

`iso15118`'s `pnc` feature takes the curve as a *trait*, so this crate implements
it on the same `p256`/`p521` generation `ocmf` links rather than importing a
second one. Two implementations of one primitive would compile perfectly,
because the boundary between them is SEC1 bytes, which is exactly the problem.

## Licence

MIT OR Apache-2.0
