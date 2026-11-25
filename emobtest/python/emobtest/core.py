"""The domain, from emob's own crates: meters, verdicts, Eichrecht, prices,
identifiers and events.

`AssertionError` means the system under test is wrong. `ValueError` means the
test is.
"""

from __future__ import annotations

import enum
import json
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from typing import Any

from . import _native

Number = str | int | Decimal


def _text(value: Number) -> str:
    if isinstance(value, float):
        raise ValueError(
            f"{value!r} is a float: pass a str, an int or a Decimal — money and energy are exact"
        )
    return str(value)


def _when(at: datetime | str | None) -> str:
    if at is None:
        at = datetime.now(UTC)
    if isinstance(at, datetime):
        if at.tzinfo is None:
            raise ValueError(f"{at!r} has no time zone: an instant needs one")
        return at.isoformat()
    return at


# ── Meters and verdicts ───────────────────────────────────────────────────────


class Verdict(enum.IntEnum):
    """What a signed value's check answers — the receipt verifier's codes."""

    VERIFIED = 0
    NOT_VERIFIED = 1
    UNAVAILABLE = 2
    UNREADABLE_RECORD = 3
    UNREADABLE_KEY = 4
    KEY_MISMATCH = 5

    @property
    def sentence(self) -> str:
        return {
            0: "verified — this key signed exactly these bytes",
            1: "does not verify — the record was changed, or the key is another meter's",
            2: "cannot be checked here — a curve this build does not implement",
            3: "the record is not readable OCMF",
            4: "the key is not readable",
            5: "the key is for another curve than the record names",
        }[self.value]


@dataclass(frozen=True)
class Reading:
    """One reading, as the checked bytes state it."""

    marker: str
    value: Decimal
    unit: str
    time: str


@dataclass(frozen=True)
class Check:
    """One record's verdict and its readings."""

    verdict: Verdict
    readings: tuple[Reading, ...]

    def __bool__(self) -> bool:
        return self.verdict is Verdict.VERIFIED


def verify(record: str, key: str) -> Check:
    """Check one OCMF record against one public key, offline."""
    code, summary = _native.verify(record, key)
    readings = []
    for line in filter(None, summary.split("\n")):
        marker, value, unit, when = [*line.split("\t"), "", "", "", ""][:4]
        readings.append(Reading(marker, Decimal(value or "0"), unit, when))
    return Check(Verdict(code), tuple(readings))


class Meter:
    """A calibrated meter that signs OCMF — `emob-sim`'s, seeded.

    >>> meter = Meter(seed=7)
    >>> records = meter.charge("30", start="2026-12-01T10:00:00+01:00", minutes=12)
    """

    def __init__(self, seed: int = 0, index: int = 0, *, stream: str = "emobtest") -> None:
        self._native = _native.Meter(seed, index, stream)

    @property
    def public_key(self) -> str:
        return self._native.public_key

    @property
    def evse(self) -> str:
        return self._native.evse

    @property
    def serial(self) -> str:
        return self._native.serial

    def charge(
        self, kwh: Number, *, start: datetime | str | None = None, minutes: int = 30
    ) -> list[str]:
        """Charge `kwh` over `minutes`; the signed records, in meter order."""
        return self._native.charge(_text(kwh), _when(start), minutes)

    def sign(
        self, kwh: Number, *, at: datetime | str | None = None, marker: str = "C", page: int = 1
    ) -> str:
        """One signed reading: the register at `kwh` at `at`. `marker` is `B`
        (begin), `C` (during) or `E` (end); `page` the meter's pagination.

        For the record a session does not produce on its own — a second value
        for one instant, a gap, a reading after the end.
        """
        return self._native.sign(page, marker, _when(at), _text(kwh))

    def __repr__(self) -> str:
        return f"Meter(evse={self.evse!r}, serial={self.serial!r})"


@dataclass(frozen=True)
class Evidence:
    """A session's records as `[MessEG §33]` reads them before anything bills."""

    billable: bool
    billable_for_time: bool
    kwh: Decimal | None
    reasons: tuple[str, ...]

    def __bool__(self) -> bool:
        return self.billable


def evidence(records: list[str], key: str) -> Evidence:
    """What emob's Eichrecht chain says about a session's records."""
    billable, for_time, kwh, reasons = _native.evidence(list(records), key)
    return Evidence(billable, for_time, Decimal(kwh) if kwh else None, tuple(reasons))


# ── Prices ────────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Tariff:
    """A tariff version as an estate's `[[tariffs]]` states it — gross prices.

    >>> Tariff(energy="0.59", session_fee="0.50").total(kwh=30, power_kw=150)
    Decimal('18.20')
    """

    energy: Number
    vat: Number = "19"
    per_minute: Number | None = None
    session_fee: Number | None = None
    currency: str = "EUR"
    time_zone: str = "Europe/Berlin"
    id: str = "ad-hoc"
    occupancy_per_minute: Number | None = None

    def _spec(self) -> str:
        spec: dict[str, Any] = {
            "id": self.id,
            "currency": self.currency,
            "time_zone": self.time_zone,
            "energy": _text(self.energy),
            "vat": _text(self.vat),
            "per_minute": None if self.per_minute is None else _text(self.per_minute),
            "session_fee": None if self.session_fee is None else _text(self.session_fee),
            "occupancy_per_minute": (
                None if self.occupancy_per_minute is None else _text(self.occupancy_per_minute)
            ),
        }
        return json.dumps(spec)

    def total(
        self, *, kwh: Number, power_kw: Number = 22, at: datetime | str | None = None
    ) -> Decimal:
        """A session of `kwh` at `power_kw`, rated by the function that bills."""
        return Decimal(_native.tariff_total(self._spec(), _text(kwh), _text(power_kw), _when(at)))

    def lines(self, at: datetime | str | None = None) -> list[str]:
        """What a driver must be shown, in `[AFIR Art. 5(4)]`'s order."""
        return [
            f"{price} {self.currency} / {unit}"
            for _, price, unit in _native.tariff_lines(self._spec(), _when(at))
        ]

    @property
    def fingerprint(self) -> str:
        return _native.tariff_version(self._spec())[0]

    @property
    def preisstand(self) -> str:
        """The four characters a driver compares on page, display and receipt."""
        return _native.tariff_version(self._spec())[1]


# ── Identifiers ───────────────────────────────────────────────────────────────


def evse_id(text: str) -> str:
    """An EVSE id, normalised; `ValueError` naming why it is not one."""
    return _native.evse_id(text)


def emaid(text: str) -> str:
    """An eMAID with its check digit computed and checked; `ValueError` if wrong."""
    return _native.emaid(text)


# ── Events ────────────────────────────────────────────────────────────────────

EVENT_TYPES: tuple[str, ...] = tuple(_native.event_types())


@dataclass(frozen=True)
class Event:
    """One delivered CloudEvent, and what was wrong with it, if anything."""

    id: str
    type: str
    subject: str
    data: dict[str, Any]
    raw: bytes
    headers: dict[str, str] = field(repr=False)
    problems: tuple[str, ...] = ()

    @property
    def is_test(self) -> bool:
        """A sample `emob subscribers test` or `emobtest` sent — never book it."""
        return bool(json.loads(self.raw).get("emobtest"))


def event_problems(event: str | bytes | dict[str, Any]) -> list[str]:
    """Every way a structured-mode CloudEvent misses its published schema."""
    if isinstance(event, dict):
        event = json.dumps(event)
    if isinstance(event, bytes):
        event = event.decode()
    return _native.event_errors(event)


@dataclass(frozen=True)
class FeedReport:
    """A DATEX II AFIR file, held to the Mobilithek profile."""

    warnings: tuple[str, ...]
    refusals: tuple[str | None, ...]

    def __bool__(self) -> bool:
        return bool(self.refusals) and all(r is None for r in self.refusals)


def check_feed(document: bytes | str) -> FeedReport:
    """What the Mobilithek profile says about a DATEX II file — XML or JSON."""
    if isinstance(document, str):
        document = document.encode()
    warnings, refusals = _native.feed_report(document)
    return FeedReport(tuple(warnings), tuple(refusals))


def utc(minutes_from_now: int = 0) -> datetime:
    """An aware instant, `minutes_from_now` from now — for `start=` and `at=`."""
    return datetime.now(UTC) + timedelta(minutes=minutes_from_now)
