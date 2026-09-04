# emob-roam

**The roaming edge** — one canonical charge detail record carried onto OCPI
2.3.0 and 2.2.1 with an explicit account of what the crossing cost, routed to
the partner the contract identifier itself names.

Part of [emob](https://github.com/hupe1980/emob), the open-source e-mobility
operating stack.

```console
cargo add emob-roam
```

📖 The reasoning behind this crate, with the regulation it cites, is in
**[Roaming](https://hupe1980.github.io/emob/docs/roaming/)**.
The signatures are on [docs.rs](https://docs.rs/emob-roam).


## Translation is where roaming money goes missing

A CDR is a claim sent to somebody who was not there and who will pay against
it. When that somebody is another company on another protocol version, the
claim has to survive a translation — and a hand-written `From` impl makes every
one of those decisions once, silently, at the moment nobody is looking. The
consequence surfaces six weeks later as two companies holding two different
numbers for one session, with nothing in either document explaining the gap.

So every translation here returns a `Crossing` — `emob_core::Crossing`, the same
account the OCPP 2.1 and DATEX II seams return: the value, and what it cost.

```rust
let crossing = emob_roam::ocpi::cdr::to_ocpi(&cdr, partner, &context)?;

for reason in crossing.reasons() {
    eprintln!("{reason}");
    // /total_time: 1200 s is 0.3333 h rounded to 4 places: an hour has 3600
    //              seconds and 3600 has two factors of three, so most
    //              durations have no exact decimal in hours. The cost beside
    //              it was computed from whole seconds, so re-deriving it from
    //              this figure will not reproduce it
}
```

The account composes with `ocpi-kit`'s own: a partner on 2.2.1 gets one report
of what reaching them cost, rather than two half-reports that only mean
something read together. That fold is the one part of it that is OCPI's alone,
so it is an extension trait here rather than a method on the shared type.

## A duration in hours is usually not a decimal

OCPI types `total_time`, and every period's `TIME` and `PARKING_TIME`, as a
number of **hours** `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. An hour is 3600
seconds and `3600 = 2⁴ · 3² · 5²` — only the twos and fives divide out, so a
duration has an exact decimal spelling exactly when nine divides its seconds.
Twenty-one minutes does: `0.35`. Twenty minutes does not. Twenty-two does not.

This is the same arithmetic that makes an occupancy fee of €2.50 an hour
unlawful under `[AFIR Art. 5(4)]` — sixty has a factor of three — met again one
layer out, and the resolution is the same. The money on the record was computed
from **whole seconds**, multiplied before it was divided. A partner re-deriving
it from the rounded `total_time` gets something else, and without the note the
difference has no explanation anywhere in the document.

The quarter-hour grid this workspace settles on is always exact — `900` is
divisible by nine — which is why the failure is invisible until a session
starts or stops between two boundaries, which is every session.

## A charging period has a start and no end

OCPI gives a period only `start_date_time`; a reader takes each period as
running to the next one's start, and the last as running to `end_date_time`. A
canonical `ChargingPeriod` has both ends, because a station that authorises at
10:00 and sends its first meter value at 10:20 must not produce a record
claiming twenty minutes of measurement that never happened.

Nothing is invented to fill the hole. A zero-energy bridging period would
assert that no energy moved, which is precisely what nobody measured — so the
uncovered span is reported instead, and the receiver is told that a reader will
attribute it to the period before it.

## Energy has no direction, and that one is a refusal

`ENERGY_EXPORT` exists in `CdrDimensionType` and the specification marks it
*Session Only* `[OCPI 2.3.0 §mod_cdrs_cdrdimensiontype_enum]`. What a CDR has
is `ENERGY`, and `total_energy` carries no sign convention at all.

So a V2G discharge crossing to OCPI arrives at the provider as an ordinary
draw, and the settlement runs backwards: the provider pays the operator for
energy the **driver** supplied. Import and export never net — enforced one
layer down against the OBIS code the meter signed — and a translation that
quietly re-signed one as the other would be that invariant broken at the last
possible moment, by us.

```rust
to_ocpi(&discharge, partner, &context)
// Err: an export CDR cannot be expressed in OCPI: ENERGY_EXPORT is
//      Session-only and `total_energy` has no sign, so the partner would read
//      3.400 kWh as a draw and pay the wrong way round
```

**The refusal points both ways.** A partner's CDR carrying an `ENERGY_EXPORT`
volume blocks in the pre-flight rather than being netted into the sum or dropped
from it — netting breaks the invariant above at the last moment, dropping settles
a record while ignoring energy the record itself reports. A quantity this crate
refuses to *write* into OCPI is not one it should silently *read* out of it.

`ENERGY_IMPORT` is Session-only too, and the conservation sum reads it beside
`ENERGY`. The schema check reports the deviation as a warning, so a permissive
decoder cannot make a page of CDRs unpayable over a spelling — but a warning must
not take a **quantity** with it, and reading one spelling would hide a partner's
own kilowatt-hours from the check whose job is to find them.

Two more are refusals for the same reason. An **unrated** record cannot cross,
because `total_cost` is required and the specification gives the obvious
placeholder its own meaning — *"a `total_cost` of 0.00 means free of charge"* —
so sending zero does not defer the question, it answers it, permanently and in
the partner's favour. And a **tariff element carrying a restriction this build
cannot evaluate** cannot cross, because dropping the restriction does not
narrow the element, it *widens* it: at the partner it then matches wherever the
rest of the conditions hold, and their re-rating disagrees with ours in the
driver's disfavour, from a document we published.

## Plug & Charge and AutoCharge are one value

`AUTH_REQUEST` covers both: a contract certificate the vehicle presented, and a
MAC address, which is not a standard, not authenticated and trivially
spoofable. This workspace keeps them apart precisely because the market
conflates them, so the crossing says the distinction was lost — and points at
the one place on the record where it survives, the identification strength read
off the signed meter data.

## A restriction that will not fit is not a restriction to drop

The tariff crossing refuses an element carrying a restriction this build cannot
evaluate, because publishing it stripped does not narrow the element — it
**widens** it, and the partner then prices the session under conditions nobody
checked.

The same argument covers a restriction the build *can* evaluate and OCPI cannot
carry as written. A bound that does not fit its field has two silent outcomes:
dropped, the element widens; defaulted, it moves. A `start_time` falling back to
midnight publishes a night tariff as an all-day one — not a wider price but a
different one, from a document this operator signed off. Both are
`RoamError::RestrictionNotExpressible`, and a test asserts the property that
matters: every restriction this build can express arrives at the partner with
the value it was set to.

## The zone a `22:00` is read in is not in the tariff document

OCPI writes a tariff's time, date and weekday restrictions in **local civil time
at the charge point** `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, and
puts the zone they are read in on the **Location** — `time_zone`, an IANA name,
cardinality 1 `[OCPI 2.3.0 §mod_locations_location_object]`. Not on the Tariff.

So a Tariff object carries `22:00` and nothing that says which `22:00`, and that
is not a gap a sender can close inside the object: it is a constraint on what the
sender publishes *beside* it. Every Location a tariff applies at has to carry the
zone the tariff was written in.

That is worth a note rather than a shrug, because it is the one fact a partner
needs to reproduce a price and the one this document structurally cannot carry.
`to_ocpi` names the tariff's own zone by JSON Pointer, so an operator has the
value to check its Locations against and a partner settling a disputed session
has it in the account of the crossing.

## Routing is a question the identifier already answers

`DE-ABC-C00122045-6` states, in its first five characters, the provider that
issued the contract — the party that holds the driver and will pay this CDR.
Most platforms route settlement off a hand-maintained map from something else
entirely, and that map is where roaming money goes to the wrong company: it is
edited by hand, it drifts, and nothing reconciles it against the identifiers on
the records.

```rust
let registry = PartnerRegistry::new("DE*CPO".parse()?)
    .with(Partner::hub("DE*HUB".parse()?))
    .with(Partner::emsp("NL*TNM".parse()?).on_signed_data());

registry.route(&"NL-TNM-C00122045-K".parse()?);   // Reach::Direct(NL*TNM)
registry.route(&"FR-XYZ-C00000001-4".parse()?);   // Reach::Hub { issuer: FR*XYZ }
```

A direct peer beats a hub, because sending to a hub is sending to whoever the
hub decides and giving up knowing which provider settled it. And the prefix is
the **default**, not the rule: OCPI says outright that a party id has no direct
link with the eMSP that issued a contract, so a partner that has been acquired
or issues under another namespace declares it, and that declaration wins.

## The check digit is verified where anybody last looks

Once a CDR is at the eMSP, a contract id that has lost a character still
parses, still routes, and bills the session to somebody else's contract — and
the CPO has already been paid, so nobody has a reason to look. `ocpi-kit`
parses the digit and does not check it, which is right for a wire library: a
peer's malformed id must still decode, or one bad record poisons a page.
Checking is this layer's job, and `emob-core` knows all three grammars and both
algorithms.

```rust
RoamingToken::new(issuer, "045F2C", TokenType::Rfid, "NL-TNM-000122045-X".parse()?)
// Err: the contract id `NL-TNM-000122045-X` fails its own check digit, and it
//      is what routes the money
```

The token itself is an **argument** rather than a field on the CDR, and that is
a design rather than an omission: `emob_session::Authorization` refuses to store
a raw token UID — it keeps a keyed hash — because a UID is a lifelong
identifier of a physical object a person carries. OCPI requires the real thing.
So the party that holds the mapping presents it, at the edge that has to send
it, on the records that are leaving, and nowhere else.

## The location a partner reads is the one the feed publishes

A CPO states where a charge point is twice: to a roaming partner in OCPI, and
to the public in the national access point feed `[AFIR Art. 20(2)(c)]`. Almost
every stack generates the two from different systems, and the drift is
invisible because nobody compares them.

This crate has no location model. It reads `emob_poi::site`, which is what the
DATEX II publication is built from — the same argument that crate makes about
the price, one field over. And OCPI's length bounds are enforced rather than
truncated: 45 characters is not generous for a German street, and a truncated
address names a different building.

## A correction is two documents, and neither is the other

OCPI has no way to amend a CDR. It reverses one with a Credit CDR — `credit =
true`, `credit_reference_id` naming the original, and **only** `total_cost`
negated — and then sends "a new CDR with a new unique ID and the fields `credit`
and `credit_reference_id` omitted" `[OCPI 2.3.0 §mod_cdrs_cdr_object]`:

```rust
let reversal = to_ocpi_credit(&original, &partner, &context, "cdr-1-C")?;
let replacement = to_ocpi(&corrected, &partner, &context)?;   // names nothing
```

The replacement's account says what it replaces, because the document cannot.
Inbound, a Credit CDR is refused by name rather than read: it repeats the
original's kilowatt-hours with the money negated, and read as a session it would
put them in the ledger twice.

## What it proves

`tests/the_same_session.rs` runs one genuinely signed session — real ECDSA over
real payload bytes, verified through the real verifier — out over three paths
and asserts they settle at the same money: self-roaming, OCPI 2.3.0, and OCPI
2.2.1. The signed records arrive verbatim and re-verify at the far end against
the receiver's own registry, never against the key the document carries.

## Status

The OCPI half of the roaming node: CDRs, tariffs and locations across 2.3.0 and
2.2.1, outbound and inbound. OICP (Hubject) and eMIP (GIREVE legacy) are 📐.

## License

MIT OR Apache-2.0, at your option.
