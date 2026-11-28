"""`emobtest` on the command line — the toolkit for whoever is debugging a real
record, a real price or a real integration, not only for test authors.

    emobtest verify 'OCMF|…' 3059…              does this key sign this record?
    emobtest price --energy 0.59 --kwh 30        what does a session cost?
    emobtest feed table.json                     will the Mobilithek take it?
    emobtest inbox --port 9199                   show emob's events as they arrive
    emobtest conformance receiver URL --secret-env WEBHOOK_SECRET
    emobtest conformance authorize URL --token-env SERVICE_TOKEN
    emobtest conformance mcp emob mcp

Exit status: 0 held · 1 the thing checked is wrong · 2 usage.
"""

from __future__ import annotations

import argparse
import asyncio
import os
import sys
import time

from . import __version__, conformance
from .core import EVENT_TYPES, Tariff, check_feed, verify
from .inbox import Inbox


def _verify(args: argparse.Namespace) -> int:
    record = sys.stdin.read().strip() if args.record == "-" else args.record
    check = verify(record, args.key)
    print(f"{'✓' if check else '✗'} {check.verdict.sentence}")
    for reading in check.readings:
        print(f"  {reading.marker} {reading.value} {reading.unit} · {reading.time}")
    return 0 if check else 1


def _price(args: argparse.Namespace) -> int:
    tariff = Tariff(
        energy=args.energy,
        vat=args.vat,
        per_minute=args.per_minute,
        occupancy_per_minute=args.occupancy_per_minute,
        session_fee=args.session_fee,
        currency=args.currency,
    )
    print(" · ".join(tariff.lines()))
    print(f"Preisstand {tariff.preisstand}")
    for kwh in args.kwh:
        total = tariff.total(kwh=kwh, power_kw=args.power)
        print(f"  {kwh:>4} kWh at {args.power} kW → {total} {args.currency}")
    return 0


def _feed(args: argparse.Namespace) -> int:
    with open(args.file, "rb") as file:
        report = check_feed(file.read())
    for warning in report.warnings:
        print(f"  ⚠ {warning}")
    for index, refusal in enumerate(report.refusals, 1):
        print(
            f"  {'✓' if refusal is None else '✗'} publication {index}"
            + (f": {refusal}" if refusal else "")
        )
    verdict = "takes" if report else "refuses"
    print(f"{'✓' if report else '✗'} {args.file}: the Mobilithek profile {verdict} it")
    return 0 if report else 1


def _inbox(args: argparse.Namespace) -> int:
    with Inbox(args.secret, host=args.host, port=args.port) as inbox:
        print(f"listening on {inbox.url}")
        print(f"secret       {inbox.secret}")
        print("add it to the estate as a [[subscribers]] entry; Ctrl-C to stop\n")
        shown = 0
        try:
            while True:
                time.sleep(0.2)
                for event in inbox.events[shown:]:
                    mark = "✓" if not event.problems else "✗"
                    test = " (test)" if event.is_test else ""
                    print(f"{mark} {event.type}{test} · {event.subject}")
                    for problem in event.problems:
                        print(f"    {problem}")
                shown = len(inbox.events)
        except KeyboardInterrupt:
            return 0


def _conformance(args: argparse.Namespace) -> int:
    if args.suite == "mcp":
        from .mcp import probe

        try:
            command = args.server[1:] if args.server[:1] == ["--"] else args.server
            report = asyncio.run(probe(command))
        except OSError as error:
            print(f"MCP server did not start: {error}", file=sys.stderr)
            return 1
    elif args.suite == "receiver":
        secret = os.getenv(args.secret_env) if args.secret_env else args.secret
        if not secret:
            print("The receiver secret is empty or unset.", file=sys.stderr)
            return 2
        report = conformance.receiver(args.url, secret, event_type=args.event_type)
    elif args.suite == "tariff":
        report = conformance.tariff(args.url, args.tariff_id)
    else:
        token = os.getenv(args.token_env) if args.token_env else args.token
        if not token:
            print("The provider service token is empty or unset.", file=sys.stderr)
            return 2
        report = conformance.authorize(args.url, token, known=args.known, blocked=args.blocked)
    print(report)
    return 0 if report.passed else 1


def _recover(args: argparse.Namespace) -> int:
    from .recovery import recover_event

    secret = os.environ.get(args.secret_env)
    if not secret:
        print(
            "recovery signing secret is absent from the named environment variable", file=sys.stderr
        )
        return 2
    try:
        body = recover_event(args.url, args.subscriber, args.event_type, args.id, secret=secret)
    except ValueError as error:
        print(f"recovery failed: {error}", file=sys.stderr)
        return 1
    sys.stdout.buffer.write(body)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="emobtest", description=__doc__.split("\n\n")[0])
    parser.add_argument("--version", action="version", version=f"emobtest {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("verify", help="check one OCMF record against a public key")
    p.add_argument("record", help="the OCMF| line, or - to read it from stdin")
    p.add_argument("key", help="the meter's public key: hex, Base64 or Base32")
    p.set_defaults(run=_verify)

    p = sub.add_parser("price", help="what a session costs, rated by the function that bills")
    p.add_argument("--energy", required=True, help="gross price per kWh, e.g. 0.59")
    p.add_argument("--vat", default="19")
    p.add_argument("--per-minute")
    p.add_argument(
        "--occupancy-per-minute", help="gross price per minute connected without requesting energy"
    )
    p.add_argument("--session-fee")
    p.add_argument("--currency", default="EUR")
    p.add_argument("--kwh", nargs="+", default=["10", "30", "50"])
    p.add_argument("--power", default="22", help="kW")
    p.set_defaults(run=_price)

    p = sub.add_parser("feed", help="hold a DATEX II AFIR file to the Mobilithek profile")
    p.add_argument("file")
    p.set_defaults(run=_feed)

    p = sub.add_parser("inbox", help="receive and check emob's signed events, live")
    p.add_argument("--secret", help="whsec_…; a fresh one when absent")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=9199)
    p.set_defaults(run=_inbox)

    p = sub.add_parser("conformance", help="hold a replacement service to emob's contract")
    suites = p.add_subparsers(dest="suite", required=True)
    p.set_defaults(run=_conformance)
    receiver = suites.add_parser("receiver", help="check a signed event receiver")
    receiver.add_argument("url")
    key = receiver.add_mutually_exclusive_group(required=True)
    key.add_argument("--secret-env", metavar="VAR", help="variable holding the subscriber secret")
    key.add_argument("--secret", help="the subscriber's whsec_… secret")
    receiver.add_argument(
        "--event-type",
        choices=["de.emob.cdr.issued", "de.emob.invoice.issued"],
        default="de.emob.cdr.issued",
        help="event a replacement receiver accepts",
    )
    tariff = suites.add_parser("tariff", help="check current and versioned seller tariffs")
    tariff.add_argument("url")
    tariff.add_argument("--tariff-id", required=True, help="a published tariff id")
    authorize = suites.add_parser("authorize", help="check a provider's authorization endpoint")
    authorize.add_argument("url")
    key = authorize.add_mutually_exclusive_group(required=True)
    key.add_argument("--token-env", metavar="VAR", help="variable holding the service token")
    key.add_argument("--token", help="the service token csmsd presents")
    authorize.add_argument("--known", help="a uid the backend accepts")
    authorize.add_argument("--blocked", help="a uid the backend blocks")
    mcp = suites.add_parser("mcp", help="check the meter verifier over local stdio")
    mcp.add_argument(
        "server", nargs=argparse.REMAINDER, help="trusted local server command and arguments"
    )

    p = sub.add_parser("recover", help="retrieve and authenticate one originally addressed event")
    p.add_argument("url", help="issuer base URL or recovery endpoint")
    p.add_argument("--subscriber", required=True, help="original subscriber name")
    p.add_argument("--event-type", required=True, choices=EVENT_TYPES)
    p.add_argument("--id", required=True, help="original event ID")
    p.add_argument(
        "--secret-env",
        required=True,
        metavar="VAR",
        help="environment variable containing the subscriber secret",
    )
    p.set_defaults(run=_recover)

    args = parser.parse_args(argv)
    try:
        return args.run(args)
    except ValueError as why:
        print(f"error: {why}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
