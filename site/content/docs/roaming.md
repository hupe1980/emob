+++
title = "Roaming"
weight = 7
description = "One canonical model, every wire native, and translation cost recorded rather than absorbed: OCPI 2.3.0 and 2.2.1, the crossings that are refusals, and the pre-flight an eMSP owes itself before paying a partner's record."

[extra]
nav = "Roaming"
+++

# Roaming ✅

A driver plugs into somebody else's charger. The operator issues a record, the
driver's provider pays it, and the two companies have to agree on a number
neither of them can independently re-measure.

Everything on this page is about the gap between those two copies.

## One canonical model, every wire native

Almost every stack picks one protocol version as its internal model and converts
at the edges. The versions then disagree about what a field means, and the
internal one wins by accident — so a partner on an older version silently gets a
narrower record, and nobody can point at where it narrowed.

Here the canonical model is `emob`'s own, and each wire is a translation *out of*
it with the cost written down:

```mermaid
flowchart LR
    subgraph canon["canonical model"]
        CDR["Cdr<br/>exact decimals, direction,<br/>signed evidence, its own price"]
    end

    CDR --> X23["OCPI 2.3.0"]
    CDR --> X22["OCPI 2.2.1"]
    CDR --> SELF["self-roaming<br/>own EMP"]
    CDR -.-> OICP["OICP · Hubject"]
    CDR -.-> EMIP["eMIP · GIREVE"]

    X23 --> ACC["Crossing&lt;T&gt;<br/>the value and the account,<br/>by JSON Pointer"]
    X22 --> ACC
    SELF --> ACC
    X23 -->|"a partner's record,<br/>read back unpriced"| CDR

    classDef built fill:#0a7d3322,stroke:#0a7d33
    classDef planned fill:#88888818,stroke:#888,stroke-dasharray:4 3
    class X23,X22,SELF,ACC built
    class OICP,EMIP planned
```

Solid is built; dashed is designed and not yet written.

A session between an operator's own CPO and its own EMP takes the same path as a
stranger's. Going multi-party then changes the transport and nothing else, which
is a property worth having before the first partner rather than after.

## Translation cost is recorded, not absorbed

A hand-written `From` impl makes every lossy decision once, silently, at the
moment nobody is looking. Six weeks later two companies hold two numbers for one
session and neither document explains the gap.

So every translation returns a `Crossing`: the value, and the account of what
reaching this partner cost — each note carrying a JSON Pointer into the document
the partner actually reads.

```rust
let crossing = ocpi::cdr::to_ocpi(&cdr, partner, &context)?;

for reason in crossing.reasons() {
    eprintln!("{reason}");
    // /total_time: 1200 s is 0.3333 h rounded to 4 places: an hour has 3600
    //              seconds and 3600 has two factors of three, so most durations
    //              have no exact decimal in hours. The cost beside it was
    //              computed from whole seconds, so re-deriving it from this
    //              figure will not reproduce it
}
```

### The note nobody else reports

OCPI carries `total_time`, and every period's `TIME` and `PARKING_TIME`, as a
number of **hours**. An hour is 3600 seconds and `3600 = 2⁴ · 3² · 5²` — only
the twos and fives divide out, so a duration has an exact decimal spelling
exactly when nine divides its seconds. Twenty-one minutes does (`0.35`). Twenty
does not. Twenty-two does not.

It is the same factor of three that makes an occupancy fee of €2.50 an hour
unshowable per minute under `[AFIR Art. 5(4)]`, met again one layer out. The
money on the record was computed from whole seconds, multiplied before it was
divided; a partner re-deriving it from the rounded figure gets something else.

The quarter-hour grid this workspace settles on is always exact — 900 is
divisible by nine — which is precisely why the failure stays invisible until a
session starts or stops between two boundaries, which is every session.

## Some crossings are a falsehood, and those are refused

A note explains a number that is *approximately* right. Where the number would
be *wrong*, a note is worse than useless, because it attaches an explanation to
something a partner will act on.

| Refused | Why a note would not do |
|---|---|
| A **V2G discharge** | `ENERGY_EXPORT` is Session-only and `total_energy` carries no sign, so the partner reads a discharge as a draw and settles backwards — paying the operator for energy the *driver* supplied |
| An **unrated** record | `total_cost` is required, and the specification gives the obvious placeholder its own meaning: `0.00` is *free of charge*. Sending zero answers the question permanently, in the partner's favour |
| A tariff element stripped of an **unevaluable restriction** | Dropping a condition does not narrow the element, it *widens* it — at the partner it then matches wherever the rest holds, and their re-rating disagrees with ours in the driver's disfavour, from a document we published |
| A **gross price bound** on a tariff mixing VAT rates | `min_price`/`max_price` bound the cost *before taxes* and the field is mandatory. The gross figure written there is a minimum the partner enforces a VAT rate too high, against the driver. Where the components agree on a rate the bound is converted at it; where they state **none**, there is no tax to strip and the two figures are the same number; only where they state *different* rates is there no pre-tax figure, and inventing one is the failure |
| A restriction that does **not fit** its field | OCPI's local time and local date are narrower than the types a tariff restricts on, and a bound that will not fit has two silent outcomes: dropped, the element widens; **defaulted, it moves**. A start time falling back to midnight publishes a night tariff as an all-day one — not a wider price but a different one |

The unevaluable-restriction rule is the outward face of a rule
[the rating engine](@/docs/pricing.md) applies inward: a condition this build
cannot judge never matches, so it can neither price a session nor be published as
though it were absent.

### The same line, one wire along

[The OCPP seam](@/docs/ocpp.md#what-the-wire-cannot-say-is-a-refusal-not-a-note)
is not a roaming wire and draws the line in exactly the same place, because the
argument is not about roaming: **a loss in the driver's disfavour that the
document does not show is a refusal.** Three seams now return the same
`Crossing` — the roaming edge, the charge point's own screen, and the national
access point feed — and the type lives in the shared vocabulary crate for that
reason rather than in whichever one needed it first.

## …and the record comes back

`from_ocpi` reads a partner's CDR into the canonical model, and the test that
matters is the round trip: the record this crate writes is one it can read back
— same key, same window, same periods, same energy — and a pre-flight that
settles it.

The two directions are not mirror images:

| Canonical | OCPI | Coming back |
|---|---|---|
| a period's `end` | a start and no end | the next period's start, and the last from `end_date_time` |
| `charging` | the period's dimensions | `TIME` says charging, `PARKING_TIME` says not. A period stating neither is **reported** — the fallback reads a taper as occupancy, and `[AFIR Art. 5(4)]` prices the two differently |
| `provenance` | nothing at all | interpolated, for every period: a number whose provenance nobody stated is not one this side may call measured |
| `auth_path` | three values for six paths | narrowed by the token type, and noted where it cannot be |
| `cost` | totals, no unit prices | **not** rebuilt |

The last one is the argument. A canonical price carries one line per distinct
price, each reproducing its own amount from its own quantity and unit price.
Rebuilding that from totals means inventing the numbers that make it add up —
and then the pre-flight checks the arithmetic of a document *this* crate wrote
rather than the one that arrived. So the record comes back unpriced, the
partner's own figure comes back beside it, and an eMSP re-rates with its own
tariff and compares.

Verification stays where it belongs: the payloads come back for the Eichrecht
crate to check against **this side's** registry, and the verified result is an
argument to the conversion rather than something it produces.

## Routing comes out of the identifier

A contract identifier names its issuer in its first five characters, and that is
the party holding the driver and paying the bill. A hand-maintained map from
anything else is where roaming money reaches the wrong company.

OCPI itself warns that a party id has no direct link with a contract's issuer, so
the prefix is the default and an explicit issuer list overrides it — which is how
an acquired partner is routed without editing every record.

The check digit is verified at that edge, because once the record has reached the
provider a transcribed identifier still parses, still routes, and bills somebody
else's contract while the operator has already been paid.

## The pre-flight an eMSP owes itself

A record arriving from a partner is not a canonical CDR yet. It is JSON that
decoded, and the questions worth asking are OCPI's own — asked of the document
that *arrived*, before any conversion has had the chance to repair it.

```rust
let report = ocpi::inbound::preflight(&arrived, SignedDataPolicy::Required);

if !report.is_settleable() {
    for reason in report.reasons() { eprintln!("blocked: {reason}"); }
}
```

It asks whether the periods sum to the stated total, whether the durations in the
document agree with its own timestamps, whether the parts of the cost add to the
whole, whether the periods are in order — OCPI gives a period no end, so a reader
derives every span from the *next* one's start, and out of order every duration
is silently wrong — and whether the contract identifier passes its own check
digit.

Findings are separated into blocking and warning, because settlement is a
business decision and a spelling deviation is not the same as a missing
kilowatt-hour. A warning must never carry a *quantity* out of the check, though:
`ENERGY_IMPORT` is read alongside `ENERGY` so that a partner using the wrong
dimension name cannot make their own energy invisible to the sum meant to find
it.

### Signatures are verified against *this* side's registry

The pre-flight verifies nothing itself. It hands back the signed records and the
key the document claims, and the Eichrecht chain checks them against the registry
**this** side holds — never the key travelling in the file, which is the artefact
under examination. A reader that verified against the document's own key would
have proven only that whoever wrote it owned a private key.

The claimed key is still worth reading. One that matches narrows a dispute to the
numbers; one that differs names a meter swapped without the registry hearing
about it. The checks themselves are [the Eichrecht chain](@/docs/eichrecht.md).

## What is proven

One genuinely signed session settles at the **same money** over three paths —
self-roaming, OCPI 2.3.0 and OCPI 2.2.1 — with the signed records arriving
verbatim and re-verifying at the far end against the receiver's own registry.

The tariff crosses too, and the same session's price reads identically to the
partner, to the driver at the point and to the national access point — see
[Tariffs](@/docs/pricing.md#one-price-three-audiences).

OICP (Hubject) and eMIP (GIREVE legacy) are 📐: designed, not written. So are
OCPI's Sessions and Tokens modules, which are a service's publishing surface
rather than a settlement question.
