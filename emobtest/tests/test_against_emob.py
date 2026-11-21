"""emob, driven from outside: stations on every OCPP version charge cars, and
what the platform bills — or refuses to — is what a driver and an auditor
would see.

Against a running estate; `just e2e` starts one and runs this:

    pytest tests/test_against_emob.py --emob-ocpp wss://127.0.0.1:9000/ocpp \\
        --emob-ca ../target/dev-tls/ca.pem   # with EMOB_OPERATOR_TOKEN set

The example estate's CP-1 is a 1.6 station; a 2.0.1 and a 2.1 station are
onboarded through csmsd's registry, as an operator would. Every station's
meter is emob-sim's `Meter(2026, 1, stream="estate")`, the estate's
registered one.
"""

from __future__ import annotations

import json
import os
import urllib.request
import uuid

import pytest

import emobtest as et

CSMSD = os.environ.get("EMOB_CSMSD", "http://127.0.0.1:9001")
TOKEN = os.environ.get("EMOB_OPERATOR_TOKEN")
KNOWN = "04A1B2C3D4E5F6"  # ALLOWED in the example estate
BLOCKED = "04DEADBEEF0001"
OTHER = "04C0FFEE000002"  # a second driver on the same contract
CP1_KEY = "6d6f636b2d73746174696f6e2d736563726574"  # hex of "mock-station-secret"
ESTATE_METER = {"seed": 2026, "stream": "estate", "meter": 1}

pytestmark = pytest.mark.skipif(not TOKEN, reason="needs EMOB_OPERATOR_TOKEN for csmsd")


def _api(path: str, body: dict | None = None) -> dict:
    request = urllib.request.Request(
        CSMSD + path,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"authorization": f"Bearer {TOKEN}", "content-type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=10) as answer:
        return json.load(answer)


def _last(identity: str) -> dict:
    return _api(f"/v1/csmsd/stations/{identity}")["recent"][0]


@pytest.fixture(scope="module")
def onboarded(request) -> dict[str, tuple[str, str]]:
    """CP-1, and a 2.0.1 and a 2.1 station registered at CP-1's location."""
    if not request.config.getoption("--emob-ocpp"):
        pytest.skip("no CSMS to drive: pass --emob-ocpp wss://…/ocpp")
    stations = {"1.6": ("CP-1", CP1_KEY)}
    cp1 = _api("/v1/csmsd/stations/CP-1")
    for version in ("2.0.1", "2.1"):
        identity = f"ET-{uuid.uuid4().hex[:6].upper()}"
        answer = _api(
            "/v1/csmsd/stations",
            {
                "identity": identity,
                "ocpp": version,
                "security_profile": 2,
                "evse": f"DE*ABC*E{uuid.uuid4().int % 10**8:08d}",
                "rated_power_kw": "22",
                "tariff": cp1["tariff"],
                "connector": "iec62196_t2",
                "location": cp1["location"],
                "show_price": True,
                "receipt_display_minutes": 5,
            },
        )
        stations[version] = (identity, answer["password"])
    return stations


@pytest.fixture(params=["1.6", "2.0.1", "2.1"])
def cp(request, station, onboarded):
    identity, password = onboarded[request.param]
    return station(identity, password=password, ocpp=request.param, **ESTATE_METER)


def test_a_session_bills_what_the_meter_signed(cp):
    session = cp.charge(card=KNOWN, car=et.Car.compact(soc=20), minutes=20).assert_accepted()
    session.assert_bills()
    assert _last(cp.identity)["billed"], "csmsd bills a session its own check would"


def test_the_station_page_says_it_is_emobtest(cp):
    reported = _api(f"/v1/csmsd/stations/{cp.identity}")["reported_identity"]
    assert reported["vendor"] == "emobtest" and reported["firmware"] == "emobtest", reported


@pytest.mark.parametrize("fault", ["tampered", "dropped"])
def test_a_session_the_meter_cannot_vouch_for_is_not_billed(cp, fault):
    session = cp.charge(card=KNOWN, minutes=15, fault=fault).assert_accepted()
    with pytest.raises(AssertionError):
        session.assert_bills()
    last = _last(cp.identity)
    assert not last["billed"], f"a {fault} reading was billed [MessEG §33]"
    receipt = urllib.request.urlopen(CSMSD + last["receipt"], timeout=10).read().decode()
    assert "Nicht abgerechnet" in receipt, "the receipt tells the driver"


def test_a_blocked_card_does_not_charge(cp):
    cp.charge(card=BLOCKED, minutes=5).assert_refused("Blocked")


def test_a_2x_display_shows_the_receipt_and_the_price(cp):
    if cp.ocpp == "1.6":
        pytest.skip("SetDisplayMessage is OCPP 2.x")
    cp.expect_display("Preisstand")  # the price, once the station has booted
    cp.charge(card=KNOWN, minutes=10).assert_accepted()
    assert _last(cp.identity)["receipt"] in cp.expect_display("Beleg:")


def test_a_held_point_charges_only_its_card(cp):
    held = _api(
        f"/v1/csmsd/stations/{cp.identity}/reservations",
        {"uid": KNOWN, "minutes": 30, "reason": "emobtest holds it"},
    )
    assert [r.id for r in cp.reservations] == [held["number"]], "the station was told"
    assert cp.reservations[0].card == KNOWN
    assert cp.reservations[0].until > et.utc(), "held into the future"
    cp.charge(card=OTHER, minutes=5).assert_refused("Invalid")
    cp.charge(card=KNOWN, minutes=10).assert_accepted().assert_bills()
    assert _api(f"/v1/csmsd/stations/{cp.identity}")["reservation"] is None, "honoured"
