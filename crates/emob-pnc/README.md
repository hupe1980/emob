# emob-pnc

ISO 15118 Plug & Charge from the back office's side: which ecosystem a contract certificate belongs to, under which generation's rules (ISO 15118-2 or -20), whether anything on its path has been revoked, and which vehicle it was installed against.

[![crates.io](https://img.shields.io/crates/v/emob-pnc.svg)](https://crates.io/crates/emob-pnc) [![docs.rs](https://img.shields.io/docsrs/emob-pnc)](https://docs.rs/emob-pnc) [![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue)](#) [![License](https://img.shields.io/crates/l/emob-pnc.svg)](#license)

With Plug & Charge the vehicle authenticates with a contract certificate instead
of an RFID card. The protocol side — EXI codec, message sets, session engines,
TLS — belongs to [`iso15118`](https://crates.io/crates/iso15118) (0.9) and runs
on stations and vehicles. This crate covers what the protocol assumes somebody
else did: keeping several PKI ecosystems (pools) apart, answering revocation
honestly, judging each chain under the generation the caller names, reading the
contract identifier off the certificate, and keeping the book of contracts per
vehicle. It has no I/O and no clock.

```console
cargo add emob-pnc
```

## Example

```rust
use emob_pnc::{Generation, Pool, PoolId, PoolRegistry, RevocationList, TrustAnchor, verify_contract};

// A platform is connected to ecosystems, plural; each has its own anchors.
let registry = PoolRegistry::new()
    .with("hubject-ccp",    Pool::new().with(TrustAnchor::unbounded(root_der, "over OFTP2, 2026-01")))
    .with("another-scheme", Pool::new().with(TrustAnchor::unbounded(other_der, "…")));

// Revocation status is supplied, with the instant it was compiled.
let revocations: RevocationList = load_status();

let contract = verify_contract(
    &registry,
    &PoolId::new("hubject-ccp"),     // the pool is named at the call
    Generation::Iso20,               // …and so are the rules: Iso2 or Iso20
    &chain,                          // leaf first, DER
    &revocations,
    at,                              // the instant to judge against
    None,                            // or Some("ABC") to require that provider
)?;

// The contract identifier (eMAID) came off the certificate, not the caller.
println!("{}", contract.emaid.canonical());
```

## What it guarantees

- **A chain from another ecosystem does not validate here.** `[V2G2-925]`
  requires the trust anchor to be the root required for that use. With two
  pools, a contract minted elsewhere is mathematically valid but belongs to a
  driver this platform has no relationship with. There is no global trust store:
  anchors live in a `Pool`, the pool is named at each call, and
  `PoolRegistry::validate_in` never falls back to another pool's anchors
  (`PncError::UnknownPool`, `PncError::NoTrustedAnchor`).
- **Revocation is checked over the whole path, leaf and intermediates.**
  `[OPCP §Provisioning Certificate Pool]` asks for the status of *"each
  delivered certificate (leaf and chain)"*; a leaf-only check would pass a
  contract under a sub-CA withdrawn this morning. `iso15118`'s path validation
  requires no revocation evidence for these two profiles, so it is called with
  `Evidence::none()` and this crate applies its own check afterwards.
- **A chain is judged under the generation the caller names.** -2 and -20 have
  different curves, extensions and size limits, so a -2 chain under -20 rules
  and a -20 chain under -2 rules are both refused, and `VerifiedContract` and
  `VerifiedProvisioning` carry the `Generation` they passed under.
- **Ed448 is refused by name.** -20 admits Ed448 alongside secp521r1, but the
  only Rust verifier for it sits on a cryptography generation this workspace
  does not link yet; `PncError::UnsupportedAlgorithm` says so instead of a
  misleading bad signature.
- **Only `Good` authorises.** See the four standings below.
- **Identifiers come off the certificate.** `[V2G2-108]` puts the eMAID (the
  contract identifier) in the leaf's Common Name; it is parsed through
  `emob_core::Emaid`, check digit included. `verify_contract` takes no eMAID
  argument, so a caller cannot attribute a session to another contract.
  `[V2G2-933]` makes the provisioning certificate's Common Name the vehicle's
  PCID, which `verify_provisioning` reads into a `Pcid`.
- **Manufacturer accounts are scoped.** `may_publish_for` checks a PCID's World
  Manufacturer Identifier against the WMIs an OEM account is enrolled for — the
  first control step of `[OPCP §Provisioning Certificate Pool]`. An empty list
  authorises nothing.

## How it works

### Revocation: four standings, not a `bool`

A `bool` has no place for "nobody could say", and that case always ends up as
"not revoked" because it lets the session start. `RevocationList::standing_of`
returns:

| Standing | Meaning | Authorises |
|---|---|---|
| `Good` | a status this side holds, fresh enough, does not list it | yes |
| `Revoked` | a status lists it as withdrawn | no |
| `Unknown` | this side holds no status for the issuer | no |
| `Stale` | the status is older than the operator's freshness window | no |

A `RevocationList` records when each issuer's list was compiled; the freshness
window is the operator's, set with `RevocationList::fresh_for` (default
`DEFAULT_FRESHNESS`, 24 hours). Self-issued roots are asked the same question. Anything but `Good` is
`PncError::Revoked`, carrying the standing.

### ISO 15118-2 and ISO 15118-20

| `Generation` | Rules | Curves |
|---|---|---|
| `Generation::Iso2` | ISO 15118-2, Annex F profiles | secp256r1 only `[V2G2-007]` |
| `Generation::Iso20` | ISO 15118-20 with Amendment 1: Annex B's extensions, every certificate inside its issuer's validity | secp521r1 with `ecdsa-with-SHA512`; Ed448 refused |

`PoolRegistry::validate_in`, `verify_contract` and `verify_provisioning` take
the generation as an argument: the caller knows which protocol the vehicle
spoke, and a certificate cannot be trusted to say which rules it wants. Either
way a chain is judged only under its own pool's anchors.

Roots are configuration — a `TrustAnchor` with its provenance — and no
production root ships in this crate. As of November 2026 none of the three
European V2G ecosystems (Hubject, GIREVE with Thales, CharIN with Irdeto)
publishes its -20 root, and no European certificate trust list exists yet.

### One vehicle, several contracts

`[OPCP §Contract Certificate Pool]` allows multiple contracts per PCID — a lease
contract and a private one from two providers. `ContractBook` is keyed by
**(pool, eMAID)** with the PCID as a field, so `for_vehicle` returns them all.
`retire_vehicle` deletes a vehicle's contracts, the one cascade OPCP describes; a
withdrawn root deletes nothing, because `[OPCP §Root Certificate Pool]` says the
contract data must not be deleted. `renewals_due` lists contracts expiring
within an operator-chosen window.

### Crypto from the workspace

`iso15118`'s `pnc` feature takes the curve as a trait. `Curves` implements it
(secp256r1 and secp521r1, the two Annex F curves `[V2G2-884]` admits) on the same
RustCrypto generation `ocmf` uses, so the workspace never links two
implementations of one primitive. Verification only — this crate signs nothing.

### Testing against real PKI

`fixtures` holds a -2 V2G test PKI minted by OpenSSL, so path validation is not
checked only against DER this repository produced. `fixtures::iso20` holds three
-20 test roots named for the European ecosystems — `hubject-test`,
`gireve-test`, `charin-test`, each P-521 with a sub-CA and a contract — and an
Ed448 set that proves the refusal. Each root validates its own contract and
refuses the other two's. `tests/a_contract_is_installed.rs`
also runs the provisioning chain against Hubject's published open test PKI when
that archive is present locally, and says so by name when it is not
(`just pnc-fixtures` re-mints both files; `scripts/make-test-pki.sh iso2` or
`iso20` re-mints one).

## API at a glance

| Item | Purpose |
|---|---|
| `PoolRegistry`, `Pool`, `PoolId`, `TrustAnchor` | ecosystems and their roots, with validity windows |
| `Generation` | `Iso2` or `Iso20`: the rules a chain is judged under, chosen per call |
| `verify_contract`, `VerifiedContract` | validate a contract chain in one pool; read the eMAID |
| `verify_provisioning`, `VerifiedProvisioning` | validate an OEM provisioning chain; read the PCID |
| `RevocationList`, `Withdrawals`, `Standing` | supplied revocation status and the four answers |
| `Contract`, `ContractBook` | contracts per (pool, eMAID), per vehicle, renewals |
| `Pcid`, `may_publish_for` | vehicle identifier, WMI authorisation |
| `Curves` | the ECDSA backend for `iso15118` |
| `PncError` | every refusal |

## Feature flags

| Flag | Default | Effect |
|---|---|---|
| `serde` | on | `Serialize`/`Deserialize` on the contract and pool types |

## Standards and sources

- ISO 15118-2: `[V2G2-007]`, `[V2G2-108]`, `[V2G2-884]`, `[V2G2-925]`, `[V2G2-933]`
- ISO 15118-20 with Amendment 1 (certificate profiles), via `iso15118`'s -20 policy
- Open Plug & Charge Protocol (OPCP): `[OPCP §Provisioning Certificate Pool]`, `[OPCP §Contract Certificate Pool]`, `[OPCP §Root Certificate Pool]`
- A Plug & Charge authorisation becomes `emob-session`'s `AuthPath::PlugAndCharge`, whose ceiling `Secure` follows `[OCMF Tab. 15]`, so a signed meter record is cross-checked against it.

## Part of emob

emob is an open-source Rust toolkit for EV charging back offices. Background:
[Plug & Charge](https://hupe1980.github.io/emob/docs/plug-and-charge/) on the
site; see also the [workspace README](../../README.md) and
[THREAT_MODEL.md](../../THREAT_MODEL.md).

## License

MIT OR Apache-2.0
