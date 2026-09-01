# emob-poi

**The charge point register and its national access point feed** — a DATEX II
AFIR Recharging publication generated from the same tariff that prices the
session, so the price a route planner shows and the price on the invoice cannot
disagree.

Part of [emob](https://github.com/hupe1980/emob), the open-source e-mobility
operating stack.

```console
cargo add emob-poi
```

## One number, two duties

`[AFIR Art. 5(2)]` makes the price a driver is shown before a session the price
they may be charged for it. `[AFIR Art. 20(2)(c)]` makes that same ad-hoc price
data an operator must publish, free of charge, through the national access
point — the Mobilithek in Germany, in the DATEX II Recharging profile, from
**14 April 2026** `[DATEX-II-Profil]`.

Two duties about one number, and almost every stack in this market computes it
twice: once in the billing system that rates the CDR, and once in the export job
that fills the feed. Two computations is two chances to be wrong, and the
failure is asymmetric — a feed is read by route planners and comparison sites,
and nobody ever reconciles it against an invoice.

So this crate has no price model. It publishes `emob_tariff::Tariff`, the same
value `emob_tariff::rate` charges with, in exact decimal from the tariff to the
JSON number:

```rust
let (rate, notes) = emob_poi::rate::publish(&tariff, "rate-1");
// …reaches the feed as `"value": 0.49`, not 0.49000000000000000222.
```

## What the profile cannot say, and is told to say anyway

`[DATEX-II-Profil Tab. A.116]` offers six price types: `basePrice`, `flatRate`,
`free`, `other`, `pricePerKWh`, `pricePerMinute`. Two things follow, and both
are reported rather than papered over.

**There is no per-hour price type.** A tariff is written and displayed per hour;
the profile only counts minutes. The conversion is a division by sixty, and
publishing the hourly number under `pricePerMinute` overstates the price
sixtyfold. Where the division does not terminate, the exact hourly price goes
into `additionalInformation` beside the rounded one and a `RateNote` says so.

**There is no occupancy price type.** `[AFIR Art. 5(4)]` explicitly permits a
fee per minute for time connected and *not* charging, at points of 50 kW and
above. The profile's only hook for it is `EnergyRate.combinationWithParkingFee`
— a **boolean** saying whether charging and parking are one fee, not what the
parking costs. The one surcharge the Regulation names cannot be published as a
number under the profile the same Regulation requires. It goes out as `other`
with a sentence, and the note is what keeps the gap from being forgotten.

## The register is upstream of the feed

`[LSV26 §4(1)]` makes three events notifiable — commissioning, decommissioning,
a change of operator — so an operator meeting its notification duty knows, for
every point, which lifecycle state it is in. `[DATEX-II-Profil Tab. B.45]` has a
literal for two of them: `planned` and `removed`.

Those facts usually live in different systems, and the feed is usually generated
from the one that does not know. That is why decommissioned points stay
published as `available` for months, and it is the commonest defect in European
charging data. A schema validator has nothing to say about it.

```rust
Report::new(point, Lifecycle::Decommissioned, PointStatus::Available)
// Err(StatusContradictsRegister)
```

There is no other constructor. The document cannot be built.

## The silence between the two publications

A status message carries no infrastructure. Every object in it is a reference —
`targetClass`, `idG`, `versionG` — into a table publication sent separately, on
a different schedule, usually by a different job.

A reference that does not resolve is **not an error**. The consumer has nothing
to attach the status to, so it drops it, and the point's availability disappears
from every map that reads the feed. No HTTP status changes, no schema validation
fails, nobody is told. Bump a facility's `versionG` in one job and not the other
and that is exactly what happens.

`Feed` builds both publications from one inventory, so the references are right
by construction. `feed::check` is for a deployment whose table is exported by a
different system, and wants the disagreement to be an error rather than a
silence.

## Two more things a validator would accept

**A station's total power is bounded by its own points.** `totalMaximumPower` is
`1..1` and a route planner decides on it. Below the largest point it holds, the
station is turning traffic away from capacity it has; above their sum, it is
advertising capacity the sockets cannot deliver whatever the transformer is
rated at. Load management makes the interval between them genuinely free — two
150 kW points behind a 200 kW connection is the normal case, not a mistake.

**`iso15118` in this profile means ISO 15118-**20**.** The dictionary says so in
as many words `[DATEX-II-Profil Tab. A.130]`, and the enumeration has no literal
for -2 at all. A point that speaks only the 2016 generation and publishes
`iso15118` is claiming readiness for exactly the duty `[DA-656 Anh. 2.1.3]`
phases in from 2027 — in the record a regulator reads to check it. It goes out
as `other`, and so does a DIN SPEC 70121 point: `none` means "No communication
between vehicle and the grid", which is false about a charger that talks to the
car over PLC.

The ambiguity is in the profile's vocabulary, not in the protocol.

## Checked against the Mobilithek's own reference

There is no XSD in the profile release. The two published example instances are
the only artefact that says what a conformant message looks like, so they are
what `tests/the_profile.rs` tests against: **every JSON path this crate emits is
a path the reference instance also contains**, with array indices collapsed so a
path names a shape rather than an occurrence. A misspelled key, a level of
nesting too few, an attribute hung on the wrong class — all of them fail there.

A short list of attributes is attested by the profile's *dictionary* instead,
because the example does not exercise them; each one names the class and
multiplicity it comes from.

## No I/O, no clock

Nothing here opens a socket, reads a file or asks the time — the publication
time is an argument, so an export replayed two years later produces the same
bytes. `just purity` fails the build if that stops being true. The Mobilithek's
`snapshotPush` is a service's business; a publication here is a value and a
string.

## License

MIT OR Apache-2.0.
