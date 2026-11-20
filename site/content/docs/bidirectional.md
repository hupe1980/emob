+++
title = "Bidirectional charging and MiSpeL"
weight = 11
description = "Bidirectional EV charging under MiSpeL: levy netting per month on quarter-hour values (Abgrenzungsoption A2/A11), the Pauschaloption and Rumpfmonate, in Rust."

[extra]
state = "built"
nav = "MiSpeL"
+++

**MiSpeL** is the Bundesnetzagentur's ruling (Festlegung 618-25-02, adopted
01.10.2026) on how much of a storage site's grid draw is relieved of levies —
and a bidirectional charge point is a store `[MiSpeL A1 3.2.5]`. Energy a car
feeds back is *saldierungsfähig*: the levies on that much of the site's grid
draw fall to zero `[MiSpeL Tenor 1]`; everything else drawn stays levied,
including what the cars drove away with. `emob_session::mispel` computes that
split for a month or a year, to the digit of the ruling's own worked examples,
and refuses by name whatever the ruling does not allow it to net.

Too large a figure keeps money the levies were owed; too small pays twice. So
the module keeps every intermediate figure, numbered as in the Festlegung,
because a *Nachweis* (the evidence filed with the grid operator) is checked by
somebody re-deriving it.

## Two options: Abgrenzungsoption and Pauschaloption

| Option | Period | Function | Formelsatz |
|---|---|---|---|
| **Abgrenzungsoption** (Anlage 1) — measured, per quarter hour | calendar month, or **Rumpfmonat** | `emob_session::mispel::month` | **A2**: grid meter Z1 plus a meter Z2 behind the points, formulas (1)–(21); **A11**: Z1 alone, where no other generation sits behind the connection |
| **Pauschaloption** (Anlage 2) — a flat allowance | calendar year, or Rumpfjahr | `emob_session::mispel::pauschal_year` | **P2**: one bidirectional point and a solar plant of at most 30 kWp, factor **0,2** |

Under A2, the grid draw in each quarter hour goes to the points first, and the
feed-in comes out of the points first. Energy a car brought from elsewhere
(*Fremdtankstrom*) is taken back out, and a charge point's storage efficiency
is presumed to be 85 % `[MiSpeL A1 (14)]`. The levy-reducing quantity is never
larger than the month's grid draw. Under P2, the allowance
is 500 kWh per kWp of solar `[MiSpeL A2 (P1)]`, prorated to the period, and only
feed-in in quarter hours with a non-negative spot price counts `[MiSpeL A2 (P5)]`.

Only the **levy** side is modelled. The other half of the ruling, the
förderfähige Netzeinspeisung, is a premium-supported plant operator's market
premium and plays no part without such a plant. A stationary battery is not
modelled either.

## Netting a month

```rust
use emob_core::Formelsatz;
use emob_session::mispel::{self, Conditions, Site};
use std::collections::BTreeSet;
use time::macros::date;

let site = Site { formelsatz: Formelsatz::A2, covered: BTreeSet::from([evse.clone()]) };
let conditions = Conditions {
    assigned_from: date!(2027-11-01),
    first_assignment: false,
    operator_consent: false,          // not needed from 01.10.2027
    state_aid_approved_on: None,
};

// `grid`: every quarter hour of the grid meter Z1; `sessions`: (EvseId, SessionSplit).
let month = mispel::month(2027, time::Month::November, &site, &conditions, &grid, &sessions)?;
println!("relieved {} kWh, still levied {} kWh", month.levy_reducing, month.levied_grid_draw);
```

The grid series (`GridQuarter`: draw, feed-in, whether the spot price was
negative) comes from the metering operator and is never derived from sessions.
The point's quarter hours are the same `SessionSplit` that
[settlement](@/docs/settlement.md) bills, with import and export kept apart until
the formula takes the minimum — nothing nets a draw against a discharge before
the Festlegung does.

## When the rules govern: the Tenor window

`emob_session::mispel::governed` turns the ruling's operative part (*Tenor*)
into the one window a settlement may cover. Every condition is a declared
input in `Conditions`; nothing is assumed.

| Date or condition | Rule |
|---|---|
| **21.10.2026** | the ruling takes effect with its publication `[MiSpeL Tenor 8]` |
| until **30.09.2027** | it applies only with the grid and metering operators' consent `[MiSpeL Tenor 9]` |
| Pauschaloption | only from the first day of the month after the European Commission's state-aid approval of § 19 Abs. 3c EEG — and not at all while there is none `[MiSpeL Tenor 9]` |
| a switch of an installation already assigned | only on the first day of a month |
| a first assignment mid-month | allowed; the rest of the month is a **Rumpfmonat**, which takes the calendar month's place `[MiSpeL A1 11]` |

Months are counted in German legal time: a month's quarter hours are those
starting on a Europe/Berlin day of the window, so the 92- and 100-quarter-hour
days of a clock change are counted, not assumed.

## What is refused, and why

Each refusal is a `mispel::Refusal` variant whose message names the rule:

- **Before the rules or the assignment** — a period that ends before
  21.10.2026, or before the installation was assigned.
- **Missing consent** — a period before 01.10.2027 without the operators'
  consent.
- **No state aid** — the Pauschaloption while no approval date is known, or
  before the month after it. There is no approval yet, so today every
  Pauschaloption year is refused.
- **Mid-month switch** — an installation already assigned that switches on any
  day but the first.
- **Uncovered discharge** — a point the declared metering concept does not
  cover fed back: a discharge no approved Formelsatz sees cannot be netted
  `[MiSpeL A1 3.2.5]`. A unidirectional point may only draw and stays out of Z2.
- **Interpolated quarter hour** — a covered point's slot whose energy was
  interpolated, not measured, under A2: the ruling nets measurements.
- **Incomplete grid series** — a grid meter series missing a quarter hour of
  the window, or holding one twice: a missing quarter hour of draw is levied
  draw that vanishes.
- **No solar, or more than 30 kWp** — outside the Pauschaloption.

## In the compliance calendar

Two calendar entries are entitlements, not duties: a point that feeds back
without them breaks no law but pays the levies on all of its grid draw.
`ChargePointProfile::mispel` (a `MispelPosture`: bidirectional, the
`Formelsatz`, operator consent) decides both.

| Entry | Source | From | Satisfied when |
|---|---|---|---|
| `mispel-metering-concept` | `[MiSpeL A1 3.2.5]` | 21.10.2026 | a bidirectional point's site has a Formelsatz (A2 or A11) |
| `mispel-operator-consent` | `[MiSpeL Tenor 9]` | 21.10.2026–30.09.2027 | the grid and metering operators consented |

## Status

✅ The month and the year are built in `emob-session` and tested against
Anlage 1's Beispielrechnungen 1 and 2 to the digit, Anlage 2's Rumpfjahr
example, every refusal, and a property over 500 seeded months. ⏳ No daemon
nets a month and Heute shows no MiSpeL figures: that needs a grid-meter series,
which no daemon receives.

**Related:** [Sessions and settlement](@/docs/settlement.md) ·
[The obligation calendar](@/docs/compliance.md) ·
[§ 14a and the grid](@/docs/smart-charging.md) · [Glossary](@/docs/glossary.md)
