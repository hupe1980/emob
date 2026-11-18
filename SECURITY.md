# Security

## Reporting a vulnerability

Report it privately through **GitHub private vulnerability reporting**:
[Report a vulnerability](https://github.com/hupe1980/emob/security/advisories/new)
on the repository's Security tab. Please do not open a public issue, pull request
or discussion for a suspected vulnerability.

Include the affected crate or daemon and its version, the feature flags, a minimal
reproducer (the bytes or the call sequence, and which side sends them) and the
impact you expect. Use test certificates and test keys only — never a real
contract certificate, private key, EMAID, RFID UID, IBAN or meter key.

## What happens next

| Step | Within |
|---|---|
| Acknowledgement of the report | 5 working days |
| First assessment (accepted or not, severity, affected versions) | 14 days |
| Fix released and advisory published, coordinated with the reporter | 90 days |

A fix ships as a patch release; the advisory is published on GitHub (and so in the
RustSec database) when the fix is out, crediting the reporter unless they ask
otherwise. If the 90 days cannot be met, the reporter hears why and when.

## Supported versions

Fixes target the latest published minor. Before 1.0 the minor version is the
breaking one, so a fix ships as a patch of the current minor.

| Version | Supported |
|---|---|
| 0.2.x | yes |
| < 0.2 | no |

## Scope

In scope: everything the fifteen `emob-*` crates parse, verify, price or decide,
and the eight daemons (`csmsd`, `roamd`, `billd`, `empd`, `poid`, `tarifd`,
`sited`, `agentd`) as built from this repository's `Dockerfile`. The known
threats, their status and the open risks are in [THREAT_MODEL.md](THREAT_MODEL.md).

Defects in a protocol kit this workspace stands on (`ocpp-kit`, `ocpi-kit`,
`iso15118`, `eebus`, `ocmf`, …) are welcome here too; they are passed upstream.

## Verifying a release

Every release carries, on its GitHub release page, the `.crate` files as
packaged, one CycloneDX SBOM of the whole workspace (`emob-<version>.cdx.json`)
and `SHA256SUMS`, all under a build-provenance attestation:

```sh
gh attestation verify emob-core-<version>.crate --repo hupe1980/emob
```

Crates are published with crates.io trusted publishing — no long-lived registry
token exists. The images on `ghcr.io/hupe1980/emob-<daemon>` carry BuildKit's
provenance attestation, and every Action and base image in the build is pinned
by commit SHA or digest.
