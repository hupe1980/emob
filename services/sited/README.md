# sited

**The site controller: the § 14a wire, both ends of it.** A grid operator writes
a setpoint to a site over EEBUS; the site shares that power out among its charge
points and tells each one what it may draw over OCPP.

Three crates own every rule in that sentence:

| Crate | Owns |
|---|---|
| `emob-eebus` | the value, off SPINE's `ScaledNumber`, exactly — and which of `[LPC §2.3.2]`'s five states a silence has put the site in |
| `emob-smart` | what the site may draw, and what it keeps a claim to `[BK6-22-300 Ziff. 4.5]` |
| `emob-ocpp` | the charging profile a station obeys, at a purpose an optimiser cannot outrank `[BK6-22-300 Ziff. 4.6]` |

What is here is the half none of them can have: **when**. A setpoint is a
message and a failsafe is a silence, and telling those apart needs a clock —
the one thing a domain crate in this workspace may not have.

## The edge is where the WAN is not

This is the only service designed to run *at a site* rather than in a data
centre, and the reason is in the statute. `[LPC-912]` puts a site into its
failsafe when the Energy Guard goes quiet and `[LPC-021]` gives that timer a
floor of two hours — so a site whose controller is on the far side of a broken
WAN link is held at 4.2 kW because **this process** could not be reached, not
because a grid operator asked. The decision belongs next to the connection
point.

## Silence is not permission

`GridLimit::none()` would stand for three different facts: nobody has ever
asked, the operator released the site, and the control box is dead. A site at
60 kW because it may and one at 60 kW because its controller died are the same
number and different problems, so the timer walks `[LPC-912]`'s failsafe and
`[LPC-022]`'s release after two to 24 hours, and each carries a note saying
which it is (D302).

## The record § 14a asks for

`[LPC §4.1.5]` makes the **pair** the operator's evidence: what was asked for
beside what was answered, refusals included. A setpoint nobody can show was
honoured is a setpoint nobody honoured.

## No rule lives here

`Sited::on_limit` reads a write through `emob_eebus::read_limit_write` and does
not look at the number; `Sited::allocate` calls `emob_smart::allocate` and does
not choose a ceiling; `Sited::charging_profiles` calls
`emob_ocpp::to_charging_profiles` and does not choose a purpose. A daemon is the
worst place to keep a rule, because CI does not run it.

## Configuration

| Variable | Default | What |
|---|---|---|
| `SITED_HTTP_BIND` | `127.0.0.1:9586` | the readiness and health endpoint |
| `EMOB_HTTP_BIND` | — | the estate-wide bind address, which `SITED_HTTP_BIND` wins over. Every default here is **loopback**, and loopback inside a container is reachable by nothing |

The SHIP session itself is a socket a deployment supplies; this daemon opens
none yet (M12).

## License

MIT OR Apache-2.0.
