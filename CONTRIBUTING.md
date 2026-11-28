# Contributing to emob

Thank you for looking. emob is a back office that decides money from signed
meter values, so the bar is "a test can fail", not "it looks right".

## Get going

```console
just doctor                     # what is missing on this machine
just demo                       # the whole estate in one process, seeded
just dev-keys > .env && just run   # the eleven daemons from source, no demo
emob check estate.toml          # an estate checked before a daemon reads it
```

## Before a pull request

```console
just ci      # fmt, clippy, purity, tests, guards, deny, docs — what CI runs
```

The guards (`cargo xtask check-all`) hold the documentation to the code: a
documented route that no daemon serves, a release that was never tagged, a
citation nothing indexes, a binary float, a clock in a domain crate — each
fails the build with the file and the line.

## The rules that are tests

- **Domain crates do no I/O.** No clock, socket, file or environment in
  `crates/emob-*` (except the CLI and `emob-service`); every instant is an
  argument (`just purity`, `check-graph`).
- **No binary floats.** Money and energy are exact decimals (`no-floats`).
- **A regulatory rule cites its source** in brackets, `[AFIR Art. 5(4)]`, and the
  source is indexed (`check-citations`).
- **A daemon's state is its journal.** A new fact is an `Event` variant with a
  fold; nothing writes state around it.
- **Refuse rather than guess.** An unknown estate key, a malformed identifier,
  an unset secret: a refusal that names the key.
- **Pages, not an app.** Server-rendered, no build step, under 30 kB, usable
  without script, WCAG 2.2 AA (`just a11y`).

## How a change is specified

A feature starts as a [Spec Kit](https://github.com/github/spec-kit)
specification — spec, plan, tasks, threat model — before code. Small fixes do
not need one; say in the pull request what would have failed without it.

## Commits and releases

Hard cuts: nothing is deprecated first. A change that removes or renames
something says so in [CHANGELOG.md](CHANGELOG.md) under **Removed** or
**Changed**. Releases are cut from `main` by a `vX.Y.Z` tag (see the README).

## Security

Do not open a public issue for a vulnerability: see [SECURITY.md](SECURITY.md).
