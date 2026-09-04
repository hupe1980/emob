# emob-thg

The German **greenhouse-gas quota** for public charging as executable law: the
four conditions `[38k §6(3)]` puts between a kilowatt-hour and the money it is
worth a second time, and a notification built only from energy a meter signed.

```console
cargo add emob-thg
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Compliance](https://hupe1980.github.io/emob/docs/compliance/)**. The
signatures are on [docs.rs](https://docs.rs/emob-thg).

## The revenue that fails silently

Every kilowatt-hour a public point delivers is worth money twice: once from the
driver, and once from a fuel supplier that has to reduce the emissions of what
it sells. The second is the *THG-Quote*, and it is settled from a notification
filed for the obligation year.

`[38k §6(3)]` states **four cumulative conditions**, and what makes them
dangerous is that failing them is invisible. The point charges cars. The
sessions bill. The money arrives. The quota does not — and an operator finds
out a year later, from the competent authority, about energy nobody can go back
and measure differently.

So a point that fails any of them is a refusal that names the remedy, not a line
quietly missing from a file:

```text
DEABCE00042 is not eligible: publish the register entry or consent to its
publication, sign the conformity declaration the authority provides, and obtain
an operator identification code — or forgo the quota
```

## Notified is not published

The condition most implementations get wrong is the first one. `[38k §6(3)
Nr. 1]` asks whether the regulator has **published** the notified point, or the
third party has **consented** to its publication. It does not ask whether the
Anzeige was made.

Those are different facts from different regulations. `[LSV26 §4(1)]` requires
the notice and says nothing at all about publication — a point can be duly
notified, sitting on the register, and not publishable. Reading eligibility off
the notice date is a rule that passes every point that has one, which is every
point in a compliant estate.

```rust
point.registration = Registration::notified_on(date!(2025 - 03 - 10));
point.quota.publication = RegisterPublication::Withheld;   // not eligible
point.quota.publication = RegisterPublication::ConsentGiven; // eligible
```

The other three are the lawful determination of the quantity under the measuring
and calibration law `[38k §6(3) Nr. 2]`, an identification code issued to the
operator by an ID registration organisation `[AFIR Art. 20(1)]`, and whatever
further identifying features the authority has since announced in the
Bundesanzeiger — a three-state answer, because "none were announced" and "the
announced ones are missing" are opposite outcomes and a `bool` has to pick one
of them to mean both.

## Only what a meter signed

`[38k §6(3) Nr. 2]` wants the energetic quantity determined in conformity with
the measuring and calibration law. This workspace already knows which
kilowatt-hours those are: `emob-eichrecht` decided it per record and
`emob-cdr` carries the answer.

So a record with no evidence behind it is **refused** rather than summed — and so
is one whose evidence is *there and failed*, which is the worse of the two and the
one a presence check lets through. A chain that did not hold up still produces an
`EvidenceRef`; it carries `energy_billable: false` inside it, and asking only
whether the reference exists asked the weaker half of the question. A
notification that included either would be one the third party's own declaration
`[38k §6(4)]` contradicts — and that declaration is kept for three years.

The ledger is read through `CdrLedger::live`, so a corrected record contributes
once and the superseded original contributes nothing. An export is excluded with
a note: `[38k §5(1)]` counts electricity *withdrawn* for use in the vehicle, and
V2G runs the other way.

## A session belongs to the year it was withdrawn in, not the year it started

`[38k §5(1)]` counts *die im Verpflichtungsjahr entnommene Strommenge* — the
electricity withdrawn **in** the obligation year. A charge from 23:45 on
31 December to 00:15 on 1 January withdrew half its kilowatt-hours in each, and
selecting records by `started_at` files the January half under December and files
nothing at all in January.

```rust
// One 30 kWh session across midnight, filed twice and counted once.
assert_eq!(filed(2026).megawatt_hours(), dec("0.015"));
assert_eq!(filed(2027).megawatt_hours(), dec("0.015"));
```

Nothing needed inventing: a CDR carries the quarter-hour periods
`emob_session::split` produced, and **that split conserves exactly**, so the two
halves sum to the record's own total to the last digit. A session that does not
cross the boundary yields its whole energy on one side and zero on the other.

The record carries a note when it contributes to both, because an operator
reconciling a THG file against a billing file will find two different figures for
one session and needs to know that is deliberate.

**And a settlement period is half-open.** The quarter hour running 23:45 to 00:00
withdrew nothing on the 1st, so `[38k §6(1) S. 2 Nr. 5]`'s window takes both
bounds from the quarter hour's **start** — reading its exclusive end as an
inclusive day made the December file state a window ending `2027-01-01`. That is
the `[PTB-A 50.7 §3.1.7.2]` footnote about which timestamp names a Messperiode,
arriving as a tax question: German metrology labels that quarter hour
`2027-01-01T00:00`, and reading the label as the withdrawal day moves a quarter
hour of every New Year's Eve into the following obligation year.

## Three factors, and not one of them is a constant

`[38k §5(3)]` multiplies four things: the energetic quantity, a counting factor
that steps down in 2035 and again in 2036, the average emissions per energy unit
of German electricity, and Anlage 3's adjustment factor for drive efficiency —
`0.4` for a battery-electric drive.

The emissions value is **announced annually in the Bundesanzeiger, by 31
October, for the following obligation year** `[38k §5(4)]`. A crate holding it
as a constant would be right for one year and wrong for every other, so it is an
argument — carried with the announcement it came from, because the first
question anyone asks of a two-year-old notification is which notice a figure
came out of.

```rust
let basis = EmissionsBasis::grid_average(dec("96"), "BAnz AT 31.10.2025 B5")?;
let mut claim = ClaimBuilder::new(2026, Attribution::own("AB7"), basis,
                                  DriveEfficiency::BatteryElectric)?;
claim.point(&profile, "Musterstraße 1, 10115 Berlin", &ledger)?;

let filed = claim.build()?;
filed.value.megawatt_hours();      // 0.063333 — exact, to the watt-hour
filed.value.emissions_kg_co2e();   // 26.26546176
for reason in filed.reasons() { println!("{reason}"); }
```

Exact decimal throughout. The only unit conversion is `× 3.6` megajoules per
kilowatt-hour, which is exact, so two runs of one year agree to the last digit.

## Renewable is a conjunction with a date inside it

`[38k §5(5)]` lets a claim use the value for one renewable source instead of the
grid average — but only when the electricity is drawn **directly from a plant
behind the same grid connection point** rather than from the grid, proved by the
metering point operator's quarter-hourly measurements of simultaneous
consumption. Both conditions, not either.

And the list of sources carries its own date in the middle of it: wind and sun
count now, biomass, landfill gas, sewage gas and biogas only **from obligation
year 2028**.

```rust
RenewableSource::Wind.countable_from();     // 2024
RenewableSource::Biomass.countable_from();  // 2028 — letter c
```

Where the proof is incomplete the paragraph's own remedy is the grid average, so
the constructor returns the missing condition **by name** and the caller reaches
for `grid_average`. The fallback is stated rather than performed silently: a
notification calculated on the wrong basis is one the authority recalculates,
and the operator learns about it from the difference.

## The deadline is the whole claim

`[38k §8(1) S. 1]` gives the two routes two dates, and one of them falls
**inside** the obligation year:

> … 1. nach § 6 … bis zum Ablauf des **28. Februar des Folgejahres** oder
> 2. nach § 7 … bis zum Ablauf des **15. November des jeweiligen
> Verpflichtungsjahres**.

```rust
Route::PublicChargePoints.deadline(2026)   // 2027-02-28
Route::EstimatedPerVehicle.deadline(2026)  // 2026-11-15
```

There is no late filing and no partial credit. A year of a fleet's public
kilowatt-hours is a five- or six-figure sum and it is worth nothing on 1 March,
so a `[38k §7]` filer holding the § 6 date in their head misses by four and a
half months in the direction that cannot be recovered. It is a `Date` rather than
a check, because this crate reads no clock: the service that files compares it
against today.

`[38k §8(1) S. 3]` is the other half — *"Mitteilungen … können für den jeweiligen
Ladepunkt für das jeweilige Verpflichtungsjahr nur einmal erfolgen"* — so adding
a point to a notification twice is a refusal rather than a silent replacement of
the first line, which is how a filer assembling a claim from two overlapping
inventories loses a window's energy without anything failing.

## Both routes, and the paragraph that makes a bus worth a third more

`[38k §6]` is the route a public charge point files: metered kilowatt-hours, the
operator as claimant. `[38k §7]` is the other one — *„in anderen Fällen"* — and
it is not the first with a flag on it:

| | `[38k §6]` | `[38k §7]` |
|---|---|---|
| the *Ladepunktbetreiber* | the operator of the point | the person the vehicle is registered to |
| the quantity | a mess- und eichrechtskonform measured value | a published **Schätzwert**, once per vehicle |
| the evidence | a signed meter record | a Zulassungsbescheinigung Teil I |
| the deadline | 28 February of the **following** year | 15 November **inside** the obligation year |
| the counting factor | `[38k §5(3)]` — three steps | `[38k §7(6)]` — seven, and M3/N3 reach 4 |

No charge point holds a single fact in that right-hand column, which is why it is
a second claim type.

**The factor is why it is worth building.** `[38k §7(6)]` opens *"Abweichend von
§ 5 Absatz 3 Satz 1"* and gives classes **M3 and N3** — buses and heavy goods
vehicles — a factor of **4 from 2027**, stepping down 3.5 (2035), 3 (2036), 2.5
(2037), 2 (2038), 1.5 (2039), 1 (2040). Against § 5(3)'s 3 that is a third more
counted energy on the same kilowatt-hours, and it lands exactly where a depot
operator is **both parties at once**: its posts are not publicly accessible so
§ 6 refuses them, and its buses are registered to it so § 7 counts them.

Three things fall out of the text rather than out of a summary of it. The
deviation begins in 2027, so a bus counted in 2026 counts at § 5(3)'s factor and
the notification **says so** rather than leaving an operator to discover it. A
mixed fleet is counted at two factors in one notification, because the class is a
fact about the vehicle. And a vehicle is counted once per obligation year
`[38k §7(4) S. 2]`, which needs an identifier unique inside one filer's records
and nothing more — never a registration plate, which is a lifelong identifier of
a thing a person drives.

## Where it stops

At the figure the Verordnung defines. What an operator *sells* is the difference
against the reference value in § 37a of the Bundes-Immissionsschutzgesetz, and
that is the competent authority's arithmetic over a fuel supplier's whole
balance — not a fact about a charge point. A crate that produced a price here
would be inventing the half of the calculation it cannot see.

## It reads no clock

The obligation year is an argument, the announcement is an argument, and every
point's eligibility is judged on 31 December of the year being filed for. A
notification rebuilt three years later, when the register has moved on and half
the points are decommissioned, is the same file.

## Licence

MIT OR Apache-2.0
