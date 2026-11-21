"""Assertions that say why.

Each one fails with what was expected, what was found, and the rule behind the
difference — a red test is a bug report, not a riddle.
"""

from __future__ import annotations

from decimal import Decimal

from .core import (
    Check,
    Evidence,
    Number,
    Tariff,
    Verdict,
    check_feed,
    event_problems,
    evidence,
    verify,
)


def assert_verifies(record: str, key: str) -> Check:
    """The record is signed by this key and unchanged."""
    check = verify(record, key)
    if check.verdict is not Verdict.VERIFIED:
        raise AssertionError(f"the OCMF record {check.verdict.sentence}\n  record: {record[:120]}…")
    return check


def assert_tampered(record: str, key: str) -> Check:
    """The record does **not** verify — for testing that a platform notices."""
    check = verify(record, key)
    if check.verdict is Verdict.VERIFIED:
        raise AssertionError("the record verifies: it was expected to have been altered")
    return check


def assert_bills(records: list[str], key: str, *, kwh: Number | None = None) -> Evidence:
    """The session may be billed under `[MessEG §33]` — and, given `kwh`, for exactly that."""
    found = evidence(records, key)
    if not found.billable:
        reasons = "\n  ".join(found.reasons) or "(no reason given)"
        raise AssertionError(f"the session may not be billed:\n  {reasons}")
    if kwh is not None and found.kwh != Decimal(str(kwh)):
        raise AssertionError(
            f"the signed records bill {found.kwh} kWh, not {kwh} kWh — the energy on an "
            "invoice is the difference the meter signed, not a figure beside it"
        )
    return found


def assert_not_billable(records: list[str], key: str) -> Evidence:
    """The session may **not** be billed — and the reasons say why."""
    found = evidence(records, key)
    if found.billable:
        raise AssertionError("the session would be billed: it was expected to be refused")
    return found


def assert_price(
    charged: Number,
    tariff: Tariff,
    *,
    kwh: Number,
    power_kw: Number = 22,
    at: str | None = None,
) -> None:
    """A platform charged what the tariff rates a session at — to the cent."""
    expected = tariff.total(kwh=kwh, power_kw=power_kw, at=at)
    if Decimal(str(charged)) != expected:
        shown = " · ".join(tariff.lines(at))
        raise AssertionError(
            f"charged {charged}, but the tariff rates {kwh} kWh at {power_kw} kW at {expected}\n"
            f"  shown to the driver: {shown} (Preisstand {tariff.preisstand})\n"
            "  [PAngG §3] fines a price not, or not correctly, shown"
        )


def assert_event(event: str | bytes | dict, *, type: str | None = None) -> None:
    """A CloudEvent matches its published schema (and, given `type`, is that type)."""
    problems = event_problems(event)
    if problems:
        raise AssertionError("the event does not match its schema:\n  " + "\n  ".join(problems))
    if type is not None:
        import json

        parsed = event if isinstance(event, dict) else json.loads(event)
        if parsed.get("type") != type:
            raise AssertionError(f"expected a {type} event, got {parsed.get('type')}")


def assert_feed(document: bytes | str) -> None:
    """The Mobilithek profile takes every publication of a DATEX II file."""
    report = check_feed(document)
    if not report.refusals:
        raise AssertionError("the file holds no publication")
    refused = [f"publication {i + 1}: {why}" for i, why in enumerate(report.refusals) if why]
    if refused:
        raise AssertionError("the Mobilithek would refuse:\n  " + "\n  ".join(refused))
