"""`emobtest` on the command line — the toolkit for whoever is debugging a real
record, a real price or a real integration, not only for test authors.

    emobtest verify 'OCMF|…' 3059…              does this key sign this record?
    emobtest price --energy 0.59 --kwh 30        what does a session cost?
    emobtest feed table.json                     will the Mobilithek take it?
    emobtest inbox --port 9199                   show emob's events as they arrive
    emobtest conformance receiver URL --secret …
    emobtest conformance authorize URL --token …

Exit status: 0 held · 1 the thing checked is wrong · 2 usage.
"""

from __future__ import annotations

import argparse
import sys
import time

from . import __version__, conformance
from .core import Tariff, check_feed, verify
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
    if args.suite == "receiver":
        if not args.secret:
            print("receiver needs --secret whsec_… (the subscriber's)", file=sys.stderr)
            return 2
        report = conformance.receiver(args.url, args.secret)
    else:
        if not args.token:
            print("authorize needs --token (the one csmsd presents)", file=sys.stderr)
            return 2
        report = conformance.authorize(args.url, args.token, known=args.known, blocked=args.blocked)
    print(report)
    return 0 if report.passed else 1


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
    p.add_argument("suite", choices=sorted(conformance.SUITES))
    p.add_argument("url")
    p.add_argument("--secret", help="receiver: the subscriber's whsec_… secret")
    p.add_argument("--token", help="authorize: the service token csmsd presents")
    p.add_argument("--known", help="authorize: a uid the backend accepts")
    p.add_argument("--blocked", help="authorize: a uid the backend blocks")
    p.set_defaults(run=_conformance)

    args = parser.parse_args(argv)
    try:
        return args.run(args)
    except ValueError as why:
        print(f"error: {why}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
