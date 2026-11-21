"""The pytest plugin, loaded through the `pytest11` entry point.

Fixtures: `emob_seed` (from `--emob-seed`, default 0), `meter` (a signing meter
on that seed), `inbox` (a signed-event receiver on loopback), `emsp` (an eMSP
answering authorisations on loopback), `tariff` (the reference ad-hoc tariff,
0.59 EUR/kWh gross), and `station` — a factory for stations on the CSMS at
`--emob-ocpp`, each closed after the test; without the option a test that
asks for one is skipped, not failed.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator

import pytest

from .core import Meter, Tariff
from .emsp import Emsp
from .inbox import Inbox
from .station import Station


def pytest_addoption(parser: pytest.Parser) -> None:
    group = parser.getgroup("emobtest")
    group.addoption(
        "--emob-seed",
        type=int,
        default=0,
        help="the seed every emobtest meter signs with (default 0): one seed, the same bytes",
    )
    group.addoption(
        "--emob-ocpp",
        default=None,
        metavar="URL",
        help="the CSMS's OCPP endpoint the `station` fixture dials, e.g. wss://127.0.0.1:9000/ocpp",
    )
    group.addoption(
        "--emob-ca",
        default=None,
        metavar="PEM",
        help="a CA to trust for --emob-ocpp, for a CSMS on a private CA",
    )


@pytest.fixture
def emob_seed(request: pytest.FixtureRequest) -> int:
    return request.config.getoption("--emob-seed")


@pytest.fixture
def meter(emob_seed: int) -> Meter:
    return Meter(seed=emob_seed)


@pytest.fixture
def inbox() -> Iterator[Inbox]:
    with Inbox() as receiver:
        yield receiver


@pytest.fixture
def emsp() -> Iterator[Emsp]:
    with Emsp() as backend:
        yield backend


@pytest.fixture
def tariff() -> Tariff:
    return Tariff(energy="0.59")


@pytest.fixture
def station(request: pytest.FixtureRequest, emob_seed: int) -> Iterator[Callable[..., Station]]:
    """`station(identity, password=…, ocpp="2.0.1", **kwargs)` — booted on `--emob-ocpp`."""
    url = request.config.getoption("--emob-ocpp")
    if not url:
        pytest.skip("no CSMS to drive: pass --emob-ocpp wss://…/ocpp")
    ca = request.config.getoption("--emob-ca")
    opened: list[Station] = []

    def connect(identity: str, **kwargs: object) -> Station:
        kwargs.setdefault("ca", ca)
        kwargs.setdefault("seed", emob_seed)
        opened.append(Station(url, identity, **kwargs))  # type: ignore[arg-type]
        return opened[-1]

    yield connect
    for each in opened:
        each.close()
