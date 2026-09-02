+++
title = "Documentation"
description = "How emob is put together: the Eichrecht evidence chain, the OCPP seam, the quarter-hour split, tariffs that rate what they display, the location feed, the roaming edge, and the obligation calendar."
sort_by = "weight"
template = "section.html"
+++

# Documentation

Nine pages, in the order they build on each other — the chain a kilowatt-hour
travels, from the meter to the money, out to everyone owed a copy of it, and on
to the invoice somebody pays.

```mermaid
flowchart LR
    M["meter<br/>signed OCMF"] --> E["Eichrecht<br/>chain"]
    E --> O["the OCPP<br/>seam"]
    O --> S["sessions &<br/>settlement"]
    S --> T["tariffs &<br/>pricing"]
    T --> L["locations &<br/>the access point"]
    T --> R["roaming"]
    T -->|"OCPP 2.1"| O
    S --> INV["invoice ·<br/>SEPA · books"]
    R --> INV
    C["compliance"] -.->|"binds every step"| E
    C -.-> S
    C -.-> T
    C -.-> R

    classDef step fill:#b8410f22,stroke:#b8410f
    classDef rule fill:#88888818,stroke:#888,stroke-dasharray:4 3
    class E,O,S,T,L,R,INV step
    class C rule
```

Start with **[Getting started](@/docs/getting-started.md)** if you want running
code, or with **[The Eichrecht chain](@/docs/eichrecht.md)** if you want to know
why a signed meter value is harder than it looks.

Every page marks ✅ built and 📐 designed. The gap between the two is the most
useful thing this documentation can tell you, so it is never blurred.

## Conventions

**Regulatory claims cite their source.** `[AFIR Art. 5(4)]` is Regulation (EU)
2023/1804 Article 5(4); `[DA-656 Anh. 2.1.1]` is Delegated Regulation (EU)
2025/656; `[LSV26 §4]` the Ladesäulenverordnung; `[MessEG §33]`, `[PTB-A 50.7]`
and `[REA 6-A]` the German metrology instruments; `[NIS2 Art. 21(2)]` Directive
(EU) 2022/2555 and `[CRA Art. 14]` Regulation (EU) 2024/2847; `[OCMF Tab. 7]` the
Open Charge Metering Format's own tables. A build guard fails if the source tree
cites a document the index cannot produce, so every claim on these pages can be
followed to a paragraph.

**German terms stay German where the regulation uses them** — Ladepunkt,
Eichrecht, Bilanzkreis, Durchleitung — because translating a legal term of art is
how its meaning drifts.

**The code samples are drawn from the crates' own tests**, abridged for reading
rather than invented for the page. The compiled versions are the doc-tests in the
[API documentation](https://docs.rs/emob-core), which CI builds with warnings
denied; a snippet here is an excerpt, so reach for `docs.rs` when you want the
signature rather than the shape.
