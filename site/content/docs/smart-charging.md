+++
title = "§ 14a and the grid"
weight = 10
description = "§ 14a EnWG smart charging: which charge points a grid operator may curtail, EEBUS LPC setpoints as exact decimals, and allocation under an OCPP 2.1 ceiling."

[extra]
state = "built"
nav = "§ 14a"
+++

Since 1 January 2024, **§ 14a EnWG** lets a German distribution grid operator
reduce the draw of a *steuerbare Verbrauchseinrichtung* (SteuVE, a controllable
consumer device) — which includes a non-public charge point over 4.2 kW on the
low-voltage grid. In return the site keeps a guaranteed minimum draw and pays a
reduced grid fee `[EnWG §14a]` `[BK6-22-300]`. emob splits this into three
crates: `emob-smart` decides **which** points the rule reaches and **what** the
site may draw; `emob-eebus` reads the grid operator's setpoint off the EEBUS
wire; `emob-ocpp` sends the result to each charge point as an OCPP 2.1 charging
profile.

## Which charge points § 14a covers: public ones are excluded

`[BK6-22-300 Ziff. 2.4.1]` — the Bundesnetzagentur's ruling under § 14a —
defines the device it covers as

> ein Ladepunkt für Elektromobile, **der kein öffentlich zugänglicher Ladepunkt
> im Sinne des § 2 Nr. 5 der Ladesäulenverordnung (LSV) ist**

A **publicly accessible** charge point is not a SteuVE. The forecourt fast
charger is outside § 14a; the depot, the workplace car park and the fleet yard
are inside it. A platform that curtails a public charger because a grid operator
asked breaks its promise to a driver over a duty that does not exist.

`SteuVe::of` reads the conditions — accessibility, grid level, rated power above
`THRESHOLD_KW` — off the same charge-point facts the compliance calendar judges,
and returns `NotSteuVe` with the reason otherwise. A compliance answer and the
load manager therefore cannot disagree about whether the statute applies.

## The five EEBUS LPC states, three of them at full power

The Festlegung is implemented over the EEBUS use case *Limitation of Power
Consumption* `[LPC §2.3.2]`, which gives a controllable system five states:

| State | What it means | What the operator does |
|---|---|---|
| no Energy Guard | no control box commissioned | nothing |
| `limited` | a setpoint is in force | obey it |
| `unlimited/controlled` | the grid operator is connected and allows full power `[LPC-905]` | nothing |
| `failsafe state` | the Energy Guard has been silent past the Failsafe Duration Minimum `[LPC-912]` | replace the control box |
| `unlimited/autonomous` | the silence has outlasted `[LPC-022]`'s release, two to 24 hours | tell the grid operator it cannot curtail this site |

A *limited / unlimited* flag cannot tell these apart. A depot at full power
because its grid operator allows it and one at full power because the box died
four hours ago draw the same number and are different problems. The last row
matters most: a site **leaves** failsafe. `emob_eebus::Watchdog::limit_at`
computes the state from the last time the Energy Guard was heard and the
instant asked about; `FAILSAFE_AFTER` defaults to two hours.

## Reading the EEBUS setpoint as an exact decimal

The minimum draw (*Mindestleistung*) is 4.2 kW `[BK6-22-300 Ziff. 4.5]`.
Three-phase at 6 A — the lowest current the `DIN EN 61851-1` control pilot can
signal — is 4.14 kW. Sixty watts apart, and a platform must tell them apart to
show it honoured the claim.

SPINE (the EEBUS data model) carries power as a `ScaledNumber`: an integer
mantissa and a power of ten, which is a decimal. Converting it to `f64` (as the
upstream use-case library does) loses that — 70 % of 9.9 kW is
`6930.000000000001` in `f64`. `emob_eebus::watts_of` reads it exactly:

```rust
use emob_eebus::watts_of;

assert_eq!(watts_of(&ScaledNumber::new(42,  2)), Some(Decimal::from(4200)));
assert_eq!(watts_of(&ScaledNumber::new(414, 1)), Some(Decimal::from(4140)));
```

A scale no decimal can hold returns `None` and the write is **NACKed**, not
approximated: `scale` is a signed 16-bit number the peer chooses, and a
substituted value would be a limit nobody asked for, applied to a site that will
obey it. This mirrors the OCPP seam — the protocol library handles the
communication; the number that decides anything comes from an exact source.

## Allocating power under the grid operator's ceiling

`Allocation` has no public constructor. `emob_smart::allocate` is the only way to
get one, it takes the grid limit, and every market plan — a tariff optimiser, a
departure schedule, a fleet scheduler — enters as a `Demand`:

```rust
use emob_smart::allocate;

let allocation = allocate(&site, &market_plan, &grid_limit);
assert!(allocation.total_kw() <= allocation.ceiling_kw());
assert!(allocation.within_ceiling());
```

This is `[BK6-22-300 Ziff. 4.6]`'s priority rule — the grid operator's reduction
outranks market-driven load control (*marktliche Laststeuerung*) — made
structural: no allocation exists that did not pass through the ceiling.

Two more rules:

- A setpoint **below** the Mindestleistung is answered by holding at the
  Mindestleistung and saying so in a `SmartNote`; obeying silently would give up
  a legal claim without anyone noticing.
- A charge point gets its own 6 A floor or zero, never something in between.
  `DIN EN 61851-1` has no duty cycle meaning "charge slowly", so a proportional
  share below the floor is a site drawing nothing while reporting it allocated
  everything.

## Sending the limit as an OCPP 2.1 charging profile

`emob_ocpp::to_charging_profiles` turns an allocation into OCPP 2.1 charging
profiles. OCPP resolves overlapping profiles by **purpose first, stack level
second**. `ChargingStationExternalConstraints` is the purpose reserved for an
external actor such as a distribution grid operator. Sent as a
`TxDefaultProfile` instead, the grid limit would share a tier with the optimiser
it must outrank, and the station would decide between them by a stack level
someone typed.

So the purpose is fixed, not a parameter, and the stack level is **zero**:
there is only ever one external constraint.

Three facts an allocation holds have no field on that wire, and each is reported
in the crossing's notes rather than dropped: the site ceiling (OCPP addresses
EVSEs, not sites), the headroom (*"we held you down"* versus *"nobody was
plugged in"*), and which full-power state the site is in.

## The evidence § 14a asks for

`[LPC §4.1.5]` makes the **pair** the operator's evidence: what the grid operator
asked for, and what was answered. A setpoint nobody can show was honoured was not
honoured; an allocation with no instruction behind it is a site throttling
itself. The `sited` library keeps that pair in order, refusals included
(`Sited::log`).

## sited: the site controller runs at the edge

`sited` is the one service designed to run **at the site** rather than in a data
centre. A controller behind a broken WAN link would hold a site at 4.2 kW
because the process could not be reached, not because a grid operator asked.

Northbound it speaks SHIP over TLS 1.2 (EEBUS); southbound, OCPP. The rules live
in the crates and the daemon holds none of them, so the whole exchange —
including the commissioning `[LPC §3.3]` prescribes — runs as an ordinary test
with two engines, real datagrams and time as a variable. The `sited` daemon
starts with an empty site and shows its LPC state to operators (`GET /v1/sited`);
⏳ the SHIP session and the OCPP leg are not wired.

**Related:** [The OCPP seam](@/docs/ocpp.md) ·
[Compliance](@/docs/compliance.md) ·
[Sessions and settlement](@/docs/settlement.md) · [Glossary](@/docs/glossary.md)
