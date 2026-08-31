+++
title = "The obligation calendar"
weight = 3
description = "AFIR, DA-656, LSV 2026, MessEG and the THG preconditions as dated, cited, executable rules — and why applicability and satisfaction have to stay separate questions."
+++

# The obligation calendar ✅

European charging regulation is a set of duties with dates attached. Most of
them arrive between 2024 and 2028, several bind only some points, and a few
carve out explicit exemptions. "Are we ready?" is therefore a question with a
date, a point and an answer per duty — which is a query, not a document.

```rust
let report = assess(&point, date!(2027-01-01));

for finding in report.failing() {
    println!("{}  {}", finding.obligation.citation, finding.obligation.remedy);
}
```

## What is in it

| Duty | Source | From |
|---|---|---|
| Ad-hoc charging without a contract | `[AFIR Art. 5(1)]` | 13.04.2024 |
| No roaming surcharge on the ad-hoc price | `[AFIR Art. 5(4)]` | 13.04.2024 |
| Price per kWh shown before the session | `[AFIR Art. 5(4)]` | 13.04.2024 |
| Card reader at new DC ≥ 50 kW | `[AFIR Art. 5(2)]` | 13.04.2024 |
| Card reader retrofit on TEN-T ≥ 50 kW | `[AFIR Art. 5(2)]` | 01.01.2027 |
| Data to the National Access Point | `[AFIR Art. 20]` | 14.04.2025 |
| …in the DATEX II Recharging profile | `[AFIR Art. 20]` | 14.04.2026 |
| EN ISO 15118-2 at new/renovated public points | `[DA-656]` | 08.01.2026 |
| EN ISO 15118-20, public **and** private | `[DA-656]` | 01.01.2027 |
| Plug & Charge points support both generations | `[DA-656]` | 01.01.2027 |
| Registration and operation reporting | `[LSV26]` | 01.01.2026 |
| Conformity-assessed meter for energy billing | `[MessEG §33]` | standing |
| Verifiable measured values | `[PTB-A 50.7]` | standing |
| THG-Quote: register entry + third-party access | `[38k]` | standing |

## Three properties that make it trustworthy

### Every duty carries its citation

`cargo xtask check-citations` walks the source, finds every citation, and fails
the build if one names a document `specs/README.md` does not index. A rule
citing a Verordnung nobody can produce is indistinguishable from a rule somebody
invented, and this is the difference made mechanical.

The `specs/` directory itself is gitignored — the documents are third-party and
copyrighted — but the index that lists them, with a retrieval URL each, is
tracked. The corpus rebuilds from a fresh clone.

### Every duty carries its window

`applies_from`, and `applies_until` for the duties that get superseded. Asking
the calendar about a date is the *only* way to use it, so a duty cannot be
applied a year before it exists:

```rust
// DATEX II starts on 14.04.2026.
assert_eq!(status_on(date!(2026-04-13)), Status::NotYetInForce);
assert_eq!(status_on(date!(2026-04-14)), Status::Failing);
```

This is also what makes the LSV 2016 → LSV 2026 transition representable at all,
and what the Eichrecht reform now in flight will need: the revised MID gains an
Annex Va for charging infrastructure, and when it lands it is a data change
rather than a redesign.

### Applicability and satisfaction are different questions

A private depot is **not failing** the ad-hoc payment duty. The duty does not
bind it. So the calendar has four outcomes, not two:

| Status | Meaning |
|---|---|
| `Satisfied` | binds this point, and it is met |
| `Failing` | binds this point, and it is not |
| `NotApplicable` | does not bind this point |
| `NotYetInForce` | not in force on the date asked about |

Merging the middle two produces reports that cry wolf, which is how compliance
dashboards come to be ignored.

The sharpest example is the PWM exemption. `[DA-656]` excludes existing points
that only do basic PWM signalling from the ISO 15118 duties, in as many words.
Modelling that as "satisfied" would claim a legacy point implements a standard
it does not; modelling it as "failing" would send a technician to a point the
law does not touch. It is `NotApplicable`, and the two stay distinguishable.

```rust
// A 2019 PWM-only point, judged in 2027.
assert_eq!(status(&legacy, ObligationId::Da656Iso15118Dash20), Status::NotApplicable);

// The same point once it does high-level communication.
assert_eq!(status(&modern, ObligationId::Da656Iso15118Dash20), Status::Failing);
```

## A renovation is a second birth date

AFIR attaches several duties to points "newly installed **or renovated**" after
a date. So a 2019 point left alone is outside the 2024 card-reader duty, and the
same point renovated in 2026 is inside it:

```rust
point.renovated_on = Some(date!(2026-03-01));
// now NotApplicable → Failing
```

`effective_installation_date()` is that rule, in one place, cited once.

## Planning

```rust
// What lands on the fleet between now and the end of 2027?
for o in starting_between(date!(2026-09-01), date!(2027-12-31)) {
    println!("{}  {}", o.applies_from, o.title);
}
```

## What is not here yet 📐

NIS2 and the Cyber Resilience Act are operator-level rather than point-level
duties, and belong to a second calendar keyed to the deployment rather than the
charge point. The AFIR TEN-T flag is currently an input: resolving which points
sit "along" the network from corridor data is an operations question, not a
domain one.
