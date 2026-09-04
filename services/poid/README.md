# poid

**The service that publishes an operator's charge points** to the national
access point. It decides *when* a snapshot goes out, and never *what* it says.

`[AFIR Art. 20(2)]` makes static and dynamic point data an operator's duty to
publish free of charge, and `[AFIR Art. 20(3)]` makes the API registered there
free and unrestricted. In Germany the format is not a choice: from 14.04.2026 the
feed speaks the **DATEX II Recharging profile** `[DATEX-II-Profil]`.

The documents are `emob-poi`'s — every JSON path it emits is one the Mobilithek's
own published reference instance contains. What is here is the half a domain
crate cannot have: when a snapshot goes out, which dynamic updates have
accumulated since, and whether the access point took it.

## A snapshot is refused before it is sent, not after

`Feed::check` runs before every publication, and the half that is otherwise
silent is the second one:

```rust
poid.snapshot(now)
// Err: the rate is published in Europe/Berlin at a site in Europe/Lisbon
```

A well-formed document, a lawful tariff, a real site — and a `22:00` night price
that starts an hour after the driver standing there thinks it does. Nothing fails
until somebody compares a bill against a map.

## The dynamic half references the static half at a version

A status message addresses a facility **at the version the table published it
at**, and the profile gives a consumer no way to resolve a reference to a version
it never received. So a status that outran its own table is discarded without a
word: the charger reads `available` here and is missing from every map.

`status` therefore refuses before a snapshot has been accepted, rather than
sending it and hoping.

## A feed nobody refreshed is named

```rust
poid.stale(now, within);   // Some(…) → route planners are reading a stalled feed
```

That is the failure mode of published data: nothing errors, and the map is simply
wrong. `informationStatus` is the same shape of hazard, one field along — a
production feed published as `test` is invisible, and a test feed published as
`real` sends drivers to a fiction.

## No I/O in the library

`snapshot` and `status` return the documents; the daemon pushes them, and
`accepted` records a push the access point **took**. A push that failed leaves
the feed unconfirmed and `stale` reports it — the same separation `tarifd` keeps
between attempting a delivery and recording one.

The `snapshotPush` to the Mobilithek itself is the one leg CI cannot run: it is
somebody else's server, behind credentials. Everything up to the socket is under
test.

## License

MIT OR Apache-2.0.
