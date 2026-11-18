+++
title = "Glossary"
weight = 14
description = "EV charging terms explained: CPO, eMSP, OCPP, OCPI, OCMF, Eichrecht, AFIR, CDR, eMAID, THG-Quote, MiSpeL, Rumpfmonat, signed journal head, witness and more."

[extra]
nav = "Glossary"
+++

The words this documentation uses, each in one or two sentences. Terms from
German law keep their German name, because that is the name in the text that
binds.

## Market roles

### CPO — charge point operator
Runs charging stations: installs them, connects them to a back office, sets the
ad-hoc price, and is responsible for the meter and its calibration.

### eMSP / EMP — e-mobility service provider
Holds the driver's contract and charging card or app, authorises sessions on
other operators' stations through roaming, and invoices the driver.

### Roaming hub
A platform that connects many CPOs and eMSPs so each does not need a contract
with every other — Hubject (OICP) and GIREVE are the large European ones.

### Durchleitung (Modell 2)
A German model in which the driver's own electricity supplier delivers the
energy at a public charge point and the CPO is paid for the charging service.
It separates the station from the supplier and is settled in quarter hours
through market communication.

## Protocols and formats

### OCPP — Open Charge Point Protocol
The protocol between a charging station and its back office (the **CSMS**,
charging station management system): boot, authorisation, transactions, meter
values, tariffs, smart-charging profiles. Versions 1.6J, 2.0.1 and 2.1.

### OCPI — Open Charge Point Interface
The roaming protocol between CPOs and eMSPs: locations, tariffs, tokens,
sessions and charge detail records. 2.3.0 is the current version; 2.2.1 is
widely deployed.

### OICP — Open InterCharge Protocol
Hubject's roaming protocol, spoken through its hub over mutual TLS.

### CDR — charge detail record
The record of one finished session — energy, time, location, price and
identifiers — that a CPO and an eMSP settle against.

### OCMF — Open Charge Metering Format
The format in which a calibrated meter **signs** each reading, so anyone with
the meter's public key can check later that the value was not altered.

### eMAID and EVSE ID
The contract identifier of a driver (eMAID, e.g. `DE-ABC-C12345678-X`) and the
identifier of a charging point (EVSE ID, e.g. `DE*ABC*E00001`).

### ISO 15118 and Plug & Charge
The protocol between vehicle and station. Its **Plug & Charge** mode lets the
car authorise itself with a contract certificate, issued under a public key
infrastructure (PKI) such as Hubject's.

### EEBUS
The protocol a German grid operator's control box speaks to reduce a site's
power under § 14a EnWG.

### DATEX II
The European format for traffic data, used for the charging data AFIR requires
operators to publish through the national access point (in Germany: the
Mobilithek).

### EN 16931, XRechnung and ZUGFeRD
EN 16931 is the European standard for electronic invoices, with two syntaxes:
UBL and CII. XRechnung is the German profile of it; ZUGFeRD wraps a CII invoice
inside a PDF.

### SEPA direct debit (pain.008)
The ISO 20022 message a bank accepts to collect money from a customer's account
under a signed mandate.

## Law and regulation

### AFIR
The EU Alternative Fuels Infrastructure Regulation (EU) 2023/1804: ad-hoc
payment, price transparency, card payment at fast chargers, and the data
operators must publish `[AFIR Art. 5(1)]` `[AFIR Art. 20(2)]`.

### Eichrecht
German calibration law (MessEG and MessEV). For charging it means a measured
value may only be billed if the customer can check it, long after the session
`[MessEG §33]`.

### PTB-A 50.7 and the transparency file
The PTB's requirements for charging equipment that meters and signs values
`[PTB-A 50.7]`. The **transparency file** carries the signed values and the key,
and the free **S.A.F.E. Transparenzsoftware** verifies it.

### Ladesäulenverordnung (LSV)
The German charging-point ordinance: technical minimums and the duties to notify
the Bundesnetzagentur `[LSV26 §4]`.

### PAngV
The German price indication ordinance: the price per kilowatt-hour must be shown
at the charge point `[PAngV §14(2)]`.

### § 14a EnWG and SteuVE
The German rule that lets a grid operator reduce the power of a controllable
consumer (*steuerbare Verbrauchseinrichtung*, SteuVE) — such as a non-public
wallbox — while guaranteeing it a minimum.

### THG-Quote
The German greenhouse-gas quota: electricity delivered at public charge points
can be credited to fuel suppliers, under conditions set by the 38. BImSchV
`[38k §6(3)]`.

### MiSpeL
The Bundesnetzagentur's ruling for storage and bidirectional charging points
(in effect 21.10.2026): how much of a site's grid draw is relieved of levies
because a store fed it back `[MiSpeL A1 3.2.5]`. See
[Bidirectional charging](@/docs/bidirectional.md).

### Abgrenzungsoption
MiSpeL's measured option (Anlage 1): the fed-back energy is told apart from the
drawn per calendar month on quarter-hour meter values, under a Formelsatz — A2
with a meter behind the points, or A11 with the grid meter alone.

### Pauschaloption
MiSpeL's flat option (Anlage 2): a yearly allowance from the installed solar
power and a factor of 0,2 for a charge point. It applies only from the month
after the European Commission's state-aid approval, so emob refuses it until
then `[MiSpeL Tenor 9]`.

### Rumpfmonat
A part month: when an installation is first assigned mid-month, the rest of
that month takes the calendar month's place `[MiSpeL A1 11]`. The yearly
counterpart is a Rumpfjahr.

### NIS2 and the Cyber Resilience Act
NIS2 sets cybersecurity duties for undertakings in the energy sector, including
charge point operators `[NIS2 Art. 21(2)]`. The Cyber Resilience Act sets duties
for software and devices placed on the EU market, including vulnerability
reporting `[CRA Art. 14]`.

## Running the estate

### Estate (`estate.toml`)
The one file every emob daemon reads its own sections of: stations, meter keys,
tariffs, partners, contracts, operators and addresses. Secrets are not in it —
a field ending in `_env` names the environment variable that holds one.

### Outbox
`csmsd`'s journalled list of deliveries: each settled session is sent to
`billd` and `roamd` as a signed webhook and retried until acknowledged, so a
restart loses none.

### Read model
A daemon's operator view of its state, `GET /v1/<daemon>`, readable only with
an operator token. The operator page is built from them.

### Actor
Who caused a journal fact or a logged request: `station:<identity>`,
`partner:<party>`, `operator:<name>`, `service:<daemon>`, `timer` or `estate`.
Taken from what the daemon authenticated, and part of each record's digest.

### Signed head
Where a journal ends — its position and the hash there — signed with the
daemon's Ed25519 key after every append, kept in `<journal>.head` and served at
`GET /v1/<daemon>/head`. A daemon refuses to start from a journal shorter than
its signed head.

### Signed note
The [C2SP](https://c2sp.org/signed-note) text format a signed head is written
in: an origin line, the size, the base64 hash, and a signature line any
signed-note verifier can check.

### Witness
A copy of the signed heads that no daemon's volume holds — kept by `deskd` and
`emob witness`. It alarms when a head goes backwards, forks or comes under
another key, and the alarm stays until its file is moved aside.

### Security profile
How an OCPP station proves who it is: 1 — HTTP Basic, 2 — Basic over TLS,
3 — a TLS client certificate. Set per station in the estate.

### Data key and crypto-erasure
Each daemon's 32-byte key that seals what it writes to its journal, so no card
UID or credential is on disk in clear. Destroying a key makes what it sealed
unreadable — for `empd`'s card UIDs that is deliberate erasure
(*crypto-erasure*), because an append-only journal cannot be edited.

### Heute
"Today" — the operator's page (`deskd`): what needs a person today, in order,
headed by the charging success rate.

### Ladeerfolg — charging success rate
Records settled ÷ sessions attempted, counted from the ledger that bills, per
calendar day in the estate's time zone (each attempt dated when the platform
decided it, not by the station's clock), with failures by cause: authorisation,
evidence, station fault, cancelled. Unlike uptime, it counts what the driver
experienced.

### Warum?
"Why?" — on Heute, a refused session's evidence findings, worded, each citing
the paragraph that refused it and saying what would have made it billable.

### Ladebeleg — the driver receipt
The page at `/r/<token>` for every session: the verdict, energy, price lines,
the meter key, the transparency file and an in-browser check of every signed
value.

### Transparenzdatei — transparency file
The S.A.F.E. XML file holding a session's signed OCMF records and the meter's
public key, which the independent Transparenzsoftware (or `emob verify`)
checks.

### Zähler-Schlüssel — meter key
The calibrated meter's public key. The receipt prints it in groups of four so
the driver can compare it with the label on the charge point.

## Related

[Getting started](@/docs/getting-started.md) ·
[Run the estate](@/docs/run-the-estate.md) ·
[Eichrecht](@/docs/eichrecht.md) ·
[Compliance](@/docs/compliance.md)
