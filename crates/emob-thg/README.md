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

So a record with no evidence behind it is **refused** rather than summed. A
notification that included it would be one the third party's own declaration
`[38k §6(4)]` contradicts — and that declaration is kept for three years.

The ledger is read through `CdrLedger::live`, so a corrected record contributes
once and the superseded original contributes nothing. An export is excluded with
a note: `[38k §5(1)]` counts electricity *withdrawn* for use in the vehicle, and
V2G runs the other way.

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
