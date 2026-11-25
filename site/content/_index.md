+++
title = "emob"
template = "index.html"
description = "Open-source EV charging software for Europe: CSMS and eMSP back office in Rust — OCPP 2.1, OCPI 2.3.0, Eichrecht-verified billing, AFIR compliance as code."
+++

## The operator desk and driver receipt

`just demo` starts five emob services, three simulated stations and a local OCPI peer. Open Heute to trace a refused session to its evidence, open the driver’s receipt, issue a stocked card on a published plan, or review a lost-card block.

<div class="shots">
<figure>
<picture><source media="(prefers-color-scheme: dark)" srcset="img/heute-dark.webp"><img src="img/heute-light.webp" width="1280" height="800" loading="lazy" alt="Heute, the operator page: charging success, a refused session and an invoice awaiting acceptance"></picture>
<figcaption><strong>Heute</strong> — what needs a person today, headed by the charging success rate counted from the ledger that bills. <a href="docs/run-the-estate/">The operator page →</a></figcaption>
</figure>
<figure>
<picture><source media="(prefers-color-scheme: dark)" srcset="img/receipt-dark.webp"><img src="img/receipt-light.webp" width="390" height="844" loading="lazy" alt="A driver receipt on a phone: measured, signed, verified — 26.984 kWh, 22 minutes, 13.22 euros, with the price line and the meter key"></picture>
<figcaption><strong>The receipt</strong> — the verdict, the energy, the price and the meter key, checked on the driver's phone. <a href="docs/run-the-estate/#the-driver-receipt">The receipt →</a></figcaption>
</figure>
</div>
