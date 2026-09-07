+++
title = "§ 14a and the grid"
weight = 9
description = "A grid operator's § 14a setpoint off the EEBUS wire as an exact decimal, shared out under a ceiling nothing can raise."

[extra]
state = "built"
nav = "§ 14a"
+++

Since 1 January 2024 a German distribution grid operator may reduce the draw of
a *steuerbare Verbrauchseinrichtung* — a controllable consumer device — and a
charge point over 4.2 kW in low voltage is one of them. In exchange the operator
of the site keeps a claim to a minimum draw, and pays a reduced grid fee.

That is `[EnWG §14a]` and the Festlegung `[BK6-22-300]` underneath it, and it is
the first subject in this workspace whose unit is power rather than money. It is
also three separate problems that a single "smart charging" module runs together
and gets wrong: **which** points the statute reaches, **what** the site may draw,
and **how** the instruction gets there.

## The condition every summary drops

`[BK6-22-300 Ziff. 2.4.1]` defines the device it reaches as

> ein Ladepunkt für Elektromobile, **der kein öffentlich zugänglicher Ladepunkt
> im Sinne des § 2 Nr. 5 der Ladesäulenverordnung (LSV) ist**

A **publicly accessible** point is not a steuerbare Verbrauchseinrichtung at all.
The fast charger on the forecourt is outside § 14a and always will be; the depot
behind the fence, the workplace car park and the fleet yard are inside it. Every
popular summary of the rule says the opposite, and a platform that curtails a
public charger because a grid operator asked has broken a contract with a driver
over a duty that does not exist.

The conditions are read off facts this workspace already holds about a charge
point — the same facts the obligation calendar judges — so a compliance answer
and a load manager cannot disagree about whether the statute binds a point.

## Five states, and three of them are full power

`[LPC §2.3.2]` — the EEBUS use case the Festlegung is carried over — gives a
controllable system five states. Three of them allow full power, and they are
three different facts:

| State | What it means | What an operator does |
|---|---|---|
| no Energy Guard | nobody has commissioned a control box | nothing |
| `limited` | a setpoint is in force | obey it |
| `unlimited/controlled` | the grid operator is speaking and allowing full power `[LPC-905]` | nothing |
| `failsafe state` | the Energy Guard has been silent past the Failsafe Duration Minimum `[LPC-912]` | replace the control box |
| `unlimited/autonomous` | the silence has outlasted `[LPC-022]`'s release, two to 24 hours | tell the grid operator it cannot curtail this site |

A model with *limited* and *unlimited* cannot write the difference down, let
alone report it. A depot at full power because its grid operator says so and one
at full power because the box died four hours ago are the same number and
different problems — and the last row is the one every summary drops, because it
is counter-intuitive: a site **leaves** its failsafe. A building throttled to
4.2 kW for a week because a control box died is not what § 14a asks for, and a
grid operator that believes it can still curtail such a site is wrong about
something it needs to be right about.

## Sixty watts

`[BK6-22-300 Ziff. 4.5]`'s Mindestleistung is 4.2 kW. Three-phase at 6 A —
the lowest a `DIN EN 61851-1` pilot can signal — is 4.14 kW. Sixty watts, and a
platform that cannot tell them apart cannot say whether it honoured an Anspruch.

SPINE carries power as a `ScaledNumber`: an integer mantissa and a power of ten,
which *is* a decimal. The use-case library converts it to a binary float, which
is right for a library whose implementation guides talk in watts and wrong for
the one thing this workspace does not do — 70 % of a 9.9 kW array is
`6930.000000000001` in `f64`, and one statute along that is a figure a feed-in
payment is computed against.

So the value that binds an allocation comes off the datagram, exactly:

```rust
assert_eq!(watts_of(&ScaledNumber::new(42,  2)), Some(Decimal::from(4200)));
assert_eq!(watts_of(&ScaledNumber::new(414, 1)), Some(Decimal::from(4140)));
```

A scale no decimal can hold is a **NACK**, not an approximation. A `scale` is a
signed 16-bit number the peer chooses, so an unrepresentable figure is one
well-formed message away — and a substituted value is a limit nobody asked for
applied to a site that will obey it.

This is the same line the CSMS keeps with OCPP: the protocol library answers the
*communication* question from a numeric register, and the number that decides
anything comes from somewhere else — off the signed record there, off the
`ScaledNumber` here.

## The ceiling is the only door

The allocation has no constructor. One function returns it, that function takes
the grid limit, and a market plan — a tariff optimiser, a departure schedule, a
fleet scheduler — is a `Demand` going **in**:

```rust
let allocation = allocate(&site, &market_plan, &grid_limit);
assert!(allocation.total_kw() <= allocation.ceiling_kw());
```

That is `[BK6-22-300 Ziff. 4.6]`'s own priority rule — the grid operator's
reduction outranks *marktliche Laststeuerung* — made structural rather than
remembered. There is no code path in which a plan asks for more than the grid
allows, because there is no way to build an allocation that is not the result of
that call.

Two more rules ride on it. A setpoint **below** the Mindestleistung is answered
by holding at the Mindestleistung and saying so, because obeying quietly is
giving up a legal claim nobody would ever find out had been given up. And a
point is at its own 6 A floor or at zero and never between: `DIN EN 61851-1` has
no duty cycle that means *charge slowly*, so a proportional share is a site
drawing nothing while reporting that it allocated everything.

## A regulator's priority rule is a purpose, not a level

The allocation leaves as an OCPP 2.1 charging profile, and here the same
priority has to survive a profile stack another actor also writes into.

OCPP resolves overlapping profiles by **purpose first and stack level second**,
and `ChargingStationExternalConstraints` is the purpose reserved for an external
actor — which is what a distribution grid operator is. A setpoint sent as a
`TxDefaultProfile` — the obvious choice, and the one a smart-charging backend
reaches for — sits in the same tier as the optimiser it is supposed to outrank,
and the station then resolves the two by a stack level somebody typed: a number
an operator chose, deciding a limit a regulator set.

So the purpose is fixed by the crossing and is not a parameter, and the stack
level is **zero**, deliberately the lowest: level separates two profiles of the
same purpose, and there is only ever one external constraint here.

Three facts an allocation holds have no field on that wire, and each is reported
rather than dropped — the site ceiling, which OCPP has no message for because it
addresses EVSEs; the headroom, which is the difference between *"we held you
down"* and *"nobody was plugged in"*; and which of the full-power states the site
is in.

## The record the statute actually asks for

`[LPC §4.1.5]` makes the **pair** the operator's evidence: what the grid operator
asked for, and what was answered. Neither half alone is worth anything — a
setpoint nobody can show was honoured is a setpoint nobody honoured, and an
allocation with no instruction behind it is a site throttling itself.

The site controller keeps that pair in order, refusals included, and it is the
one piece of state it holds that is not a timer.

## The edge is where the WAN is not

The controller is the only service in this workspace designed to run **at a
site** rather than in a data centre, and the reason is in the timers. A
controller behind a broken WAN link is a site that will be held at 4.2 kW because
*that process* could not be reached, not because a grid operator asked. The
decision belongs next to the connection point.

Everything else is a socket: SHIP over TLS 1.2 northbound, an OCPP call
southbound. The rules are in the crates, tested there, and the daemon holds none
of them — which is why the whole exchange, including the commissioning
`[LPC §3.3]` prescribes, runs as an ordinary test with two engines, real
datagrams and time as a variable.

## What is not here 📐

`Ziff. 4.4.a` *Direktansteuerung*, where a grid operator addresses a **device**
rather than a site — a different actor, not a different message. DER control on
OCPP 2.1, behind a capability flag until certification catches up. The other
three device families § 14a names — heat pumps, space cooling and storage — which
belong to a household energy manager, and whose formula this workspace meets at
`Ziff. 4.5.2` rather than at a socket. And the V2G flow model, which waits on a
regulator's decision rather than on a crate.
