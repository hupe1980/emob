"""A charging station and a car, to drive a CSMS the way the field does.

The station speaks OCPP 1.6, 2.0.1 or 2.1 over `ws://` or `wss://` (security
profiles 1 and 2), boots, and charges a car with a card — the physics and the
protocol are `ocpp-kit`'s wallbox, every register reading is signed OCMF by an
`emob-sim` meter. What the platform makes of the session is then the test:

>>> with Station("wss://csms.example/ocpp", "CP-1", password="…", ocpp="1.6") as cp:
...     session = cp.charge(card="04A1B2C3D4E5F6", car=Car.compact(soc=20), minutes=30)
...     session.assert_accepted()
...     session.assert_bills()

A session is played as having happened over the last `minutes`, in a second —
no timestamp is in the future, no test waits half an hour.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from datetime import datetime
from decimal import Decimal
from typing import Literal

from . import _native
from .assertions import assert_bills
from .core import Evidence

Fault = Literal["tampered", "dropped"]


@dataclass(frozen=True)
class Car:
    """A vehicle, as `ocpp-kit` simulates one: a battery that tapers, a target."""

    battery_kwh: int = 60
    soc: int = 30
    target_soc: int = 80
    max_amps: int = 32
    phases: int = 3

    def __post_init__(self) -> None:
        if not 0 <= self.soc <= 100 or not 0 <= self.target_soc <= 100:
            raise ValueError("a state of charge is a percentage, 0 to 100")
        if self.phases not in (1, 3):
            raise ValueError("a car charges on 1 or 3 phases")
        if self.max_amps < 6:
            raise ValueError("below 6 A no IEC 61851 car charges")

    @classmethod
    def compact(cls, *, soc: int = 30) -> Car:
        """A small city car: 40 kWh, one phase, 16 A."""
        return cls(battery_kwh=40, soc=soc, max_amps=16, phases=1)

    @classmethod
    def suv(cls, *, soc: int = 20) -> Car:
        """A large car: 100 kWh, three phases, 32 A."""
        return cls(battery_kwh=100, soc=soc, max_amps=32, phases=3)


@dataclass(frozen=True)
class Reservation:
    """A point the CSMS holds for one card, as the station was told."""

    id: int
    card: str
    until: datetime


@dataclass(frozen=True)
class Session:
    """One session as the station played it, and what the CSMS answered."""

    status: str
    transaction: str
    kwh: Decimal
    records: tuple[str, ...]
    meter_key: str
    fault: Fault | None = None

    @property
    def accepted(self) -> bool:
        return self.status == "Accepted"

    def assert_accepted(self) -> Session:
        if not self.accepted:
            raise AssertionError(f"the CSMS answered the card {self.status}, not Accepted")
        return self

    def assert_refused(self, status: str | None = None) -> Session:
        """The card was not let charge — and, given `status`, for that reason."""
        if self.accepted:
            raise AssertionError("the CSMS let the card charge: it was expected to refuse it")
        if status is not None and self.status != status:
            raise AssertionError(f"the CSMS answered {self.status}, not {status}")
        return self

    def assert_bills(self) -> Evidence:
        """The signed records bill exactly the energy the car took."""
        return assert_bills(list(self.records), self.meter_key, kwh=self.kwh)


class Station:
    """A station on a CSMS's OCPP endpoint, booted and ready to charge.

    `password` is the station's Basic Auth secret — on 1.6 its hex
    AuthorizationKey, or the text it is the hex of. `ca`
    is a PEM file to trust for `wss://`, for a CSMS on a private CA. The meter
    is `Meter(seed, index, stream=stream)`'s: register its `meter_key` with
    the CSMS, or the platform is right to refuse to bill.
    """

    def __init__(
        self,
        url: str,
        identity: str,
        *,
        password: str,
        ocpp: Literal["1.6", "2.0.1", "2.1"] = "2.0.1",
        ca: str | None = None,
        seed: int = 0,
        meter: int = 1,
        stream: str = "emobtest",
    ) -> None:
        self.identity = identity
        self.ocpp = ocpp
        self._native = _native.NativeStation(url, identity, password, ocpp, ca, seed, meter, stream)

    @property
    def meter_key(self) -> str:
        """The signing meter's public key, SEC1 hex — what the registry holds."""
        return self._native.meter_key

    @property
    def meter_serial(self) -> str:
        return self._native.meter_serial

    @property
    def display(self) -> list[str]:
        """What the CSMS put on the station's display, oldest first."""
        return self._native.display

    def expect_display(self, text: str, *, within: float = 5.0) -> str:
        """The first display message containing `text`, waiting up to `within` s.

        Fails with what the display did show: "nothing" and "the wrong thing"
        are different bugs.
        """
        deadline = time.monotonic() + within
        while True:
            for shown in self.display:
                if text in shown:
                    return shown
            if time.monotonic() >= deadline:
                seen = "\n  ".join(self.display) or "(nothing)"
                raise AssertionError(
                    f"{self.identity}'s display showed no {text!r} within {within:g} s:\n  {seen}"
                )
            time.sleep(0.05)

    @property
    def reservations(self) -> list[Reservation]:
        """What the CSMS holds this station for (`ReserveNow`), until cancelled."""
        return [
            Reservation(id, card, datetime.fromisoformat(until))
            for id, card, until in self._native.reservations
        ]

    @property
    def asked(self) -> list[str]:
        """Every request the CSMS sent the station, oldest first."""
        return self._native.asked

    def charge(
        self,
        *,
        card: str,
        car: Car | None = None,
        minutes: int = 30,
        fault: Fault | None = None,
    ) -> Session:
        """Plug `car` in, present `card`, charge for `minutes`, unplug.

        `fault="tampered"` changes the closing reading after it was signed;
        `fault="dropped"` signs a reading and never sends it. A platform that
        bills either has a bug.
        """
        if minutes < 1:
            raise ValueError("a session lasts at least a minute")
        car = car or Car()
        status, transaction, kwh, records = self._native.charge(
            card,
            minutes,
            car.battery_kwh * 1000,
            car.soc,
            car.target_soc,
            car.max_amps,
            car.phases,
            fault or "",
        )
        return Session(status, transaction, Decimal(kwh), tuple(records), self.meter_key, fault)

    def close(self) -> None:
        self._native.close()

    def __enter__(self) -> Station:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def __repr__(self) -> str:
        if not hasattr(self, "_native"):
            return f"Station({self.identity!r}, ocpp={self.ocpp!r}, not connected)"
        return f"Station({self.identity!r}, ocpp={self.ocpp!r}, meter={self.meter_serial!r})"
