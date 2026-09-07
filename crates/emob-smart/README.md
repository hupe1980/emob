# emob-smart

**§ 14a EnWG as executable law**: which charge points a grid operator may switch
down, what their operator keeps a claim to, and a site allocation that cannot
exceed the setpoint.

```console
cargo add emob-smart
```

## The condition every summary of the Festlegung drops

`[BK6-22-300 Ziff. 2.4.1]` defines a *steuerbare Verbrauchseinrichtung* as

> ein Ladepunkt für Elektromobile, **der kein öffentlich zugänglicher Ladepunkt
> im Sinne des § 2 Nr. 5 der Ladesäulenverordnung (LSV) ist**

— over 4.2 kW, in low voltage. A **publicly accessible** point is not one at all.
The fast charger on the forecourt is outside `[EnWG §14a]`; the depot behind the
fence, the workplace car park and the fleet yard are inside it.

```rust
SteuVe::of(&forecourt, kw(150), GridLevel::LowVoltage, Control::Ems)   // Err: publicly accessible
SteuVe::of(&depot,     kw(11),  GridLevel::LowVoltage, Control::Ems)   // Ok
```

Three conditions, three refusals, because they are three different situations: a
public point is outside § 14a and always will be, a small one becomes
controllable the day it is uprated, and a medium-voltage site answers to its
connection contract instead. All three are read off facts the workspace already
holds about a point rather than a flag beside them.

**And charge points are counted one by one.** `Ziff. 2.4.2` lets several
installations behind one connection count as *one* SteuVE — and names only the
heat-pump and space-cooling groups. So a yard of nine wall boxes is nine
steuerbare Verbrauchseinrichtungen, which is what the Mindestleistung formula's
`n` is.

## What the operator keeps a claim to

`Ziff. 4.5` gives the operator an *Anspruch* whatever the grid operator asks for:
4.2 kW per **directly** controlled device, and for the ones on an
energy-management system

```text
Pmin = 4,2 kW + (n − 1) × GZF × 4,2 kW
```

where `n` counts **only** those — the Festlegung says so in as many words — and
`GZF` runs 0.8 at two devices down to 0.45 at nine or more.

Folding the directly controlled boxes into the group is the mistake, and it falls
in the direction that hurts: a yard of two direct and six on the EMS is owed
`8.4 + 16.8 = 25.2 kW`, and counting all eight as one group gives `18.9`. **Six
and a third kilowatts under-claimed**, because a directly controlled device is
owed its 4.2 kW whole with no simultaneity factor on it — and an operator that
under-claims accepts a curtailment deeper than the Festlegung permits, with
nothing anywhere reporting that it did.

A setpoint below the minimum is one the grid operator may not give, so the site
holds at the minimum and **says so**: obeying it silently is giving up a legal
claim that nobody would ever find out had been given up.

## The ceiling is a door, not a check

`Ziff. 4.6` is explicit about the conflict:

> Der Betreiber hat technisch sicherzustellen, dass im Fall konkurrierender
> Anforderungen mit anderweitigen Steuerungsmaßnahmen, insbesondere
> **marktlicher Laststeuerung**, der Reduzierung nach dieser Festlegung stets
> insoweit Vorrang eingeräumt wird …

A dynamic tariff, a fleet's departure schedule and a driver in a hurry are all
*"anderweitige Steuerungsmaßnahmen"*, and every one of them is a reason somebody
writes code that reaches past the limit. So the limit is not something this crate
checks — it is the only door an allocation exists through:

- `Allocation` has no constructor and no public fields;
- `allocate` is the one function that returns one, and it takes the `GridLimit`;
- a market plan is a `Demand`, which is an **input**.

A property over four thousand generated sites asserts it, and there is no
arrangement of these types in which a caller states what it wants and gets it.

## Silence from a grid operator is not permission

`[EnWG §14a]` is carried over EEBUS's **Limitation of Power Consumption** use
case, certifiable since July 2026, and its `[LPC §2.3.2]` gives a controllable
site five states where a summary gives it two. Three of them allow full power,
and they are not one fact:

| State | What it means |
|---|---|
| `limited` | a setpoint is in force |
| `unlimited/controlled` | the grid operator is there and is allowing full power |
| `failsafe state` | it has not been heard from for 120 seconds `[LPC-912]` |
| `unlimited/autonomous` | it has been gone past the Failsafe Duration Minimum `[LPC-906]` |
| — | no control box is commissioned for this site at all |

`GridLimit` is those five and not an `Option`: every way of saying *"nothing is
limiting this site"* has to say which of the three it means. A depot drawing
60 kW because its grid operator says it may and one drawing 60 kW because its
control box died at two in the afternoon are the same number and different
problems, so two of the five put a note on the allocation.

**The failsafe is a fallback, not a punishment.** `[LPC-901/1]` starts a site at
the Failsafe Consumption Active Power Limit after a restart, and `[LPC-022]`
releases it again after two to 24 hours: a building throttled to 4.2 kW for a
week because a box died is not what § 14a asks for. `RunningAutonomously` is
therefore the note an operator most needs — a grid operator that believes it can
still curtail that site cannot.

**And the Anspruch holds against a failsafe as against a setpoint.** `Ziff. 4.5`
guarantees the Mindestleistung is a draw the grid can always carry, so falling
back below it gives the claim up for a reason the Festlegung does not recognise,
at the one moment nobody is watching.

The timers are **not** here. `[LPC-912]`'s 120 seconds and `[LPC-022]`'s
two-to-24 hours belong beside the socket that would notice the silence; the
[`eebus`](https://github.com/hupe1980/eebus) crate runs that state machine
against a virtual clock. What crosses into this one is the state it reached.

## Why 4.2 kW is 4.14 kW rounded up

The threshold and the floor are the same number, and it comes from a product norm
rather than from the grid. `DIN EN 61851-1` signals the charging current over the
pilot's PWM duty cycle and cannot express less than **6 A**, so a three-phase
point cannot be told to draw under `3 × 230 V × 6 A = 4.14 kW`. The
Bundesnetzagentur's first draft said 3.7 kW; the consultation reply was that a
point told to draw it would *abort the session* and could not restart itself, and
the figure was raised for that reason and about charge points specifically.

Which is why a share is never proportional here. Dividing 10 kW across four boxes
gives each of them a number none of them can signal, and the site draws nothing
while reporting that it allocated everything. Points are seated at their **own**
floor first, in the order the operator hands them — `Ziff. 4.5.2` makes that the
operator's to decide — and one that cannot be seated is held at zero and named.

A box that can drop to a single phase mid-session reaches ~1.38 kW; one that
cannot stops at 4.14. That is `Ziff. 4.6`'s *"nächstgeringerer Wert, der
technisch möglich ist"* with a number in it, and it is a fact about the hardware
rather than about the regulation.

## And the table is still the table

`Ziff. 4.5.2` states its calculation as a presumption *"bis zum Inkrafttreten
einer anderweitigen Empfehlung"*, and VDE FNN published a recommendation on
exactly that Tenorziffer in January 2025. It concluded the other way — *"ohne das
Vorliegen praktischer Erfahrungen, keine generelle Anpassung der Mindestleistung
vorzunehmen"* — so the figures here are the ones in force, and that was worth
checking rather than assuming.

## What is not here

**The wire, and the timers on it.** A setpoint arrives over EEBUS SHIP/SPINE —
`emob-eebus` — and an allocation leaves as an OCPP charging profile —
`emob-ocpp`; the clock that decides when a site has gone quiet is `sited`'s. None
of it is this crate's: the rule lives where a test can run it, and the socket
lives in a daemon. This crate reads no clock and opens nothing, which is also
what lets `mako` and `hems` consume it without a protocol stack.

**The other three device families.** The Festlegung also names heat pumps, space
cooling and storage, and the minimum has a limb for a heat pump over 11 kW on the
same energy-management system. A site with one is owed more than `minimum_for`
returns, which is the direction an under-claim falls in. Household devices are
`hems`'s.

## Licence

MIT OR Apache-2.0
