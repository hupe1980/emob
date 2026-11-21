"""The station, the car and the eMSP — what can be held without a CSMS.

Driving a real one is `test_against_emob.py`, with `--emob-ocpp`.
"""

import json
import time
import urllib.request

import pytest

import emobtest as et
from emobtest import conformance


def test_a_car_is_a_car():
    assert et.Car.compact().phases == 1
    assert et.Car.suv(soc=10).battery_kwh == 100
    with pytest.raises(ValueError, match="percentage"):
        et.Car(soc=120)
    with pytest.raises(ValueError, match="6 A"):
        et.Car(max_amps=5)


def test_a_station_that_cannot_reach_its_csms_says_so():
    with pytest.raises(ConnectionError, match="could not boot"):
        et.Station("ws://127.0.0.1:9/ocpp", "ET-0", password="a-password-of-16-chars")


def test_a_station_on_an_unknown_version_is_a_test_bug():
    with pytest.raises(ValueError, match=r"1\.6, 2\.0\.1 or 2\.1"):
        et.Station("ws://127.0.0.1:9/ocpp", "ET-0", password="x" * 16, ocpp="2.0")


def test_the_meter_of_the_example_estate_is_reproducible():
    meter = et.Meter(seed=2026, index=1, stream="estate")
    assert meter.public_key.startswith("042D356388AE4082")
    assert meter.serial == "SIM-METER-00001"


def test_the_emsp_conforms_to_the_contract_it_simulates(emsp):
    emsp.on("04A1B2C3D4E5F6").accepts().on("04DEADBEEF0001").blocks()
    report = conformance.authorize(
        emsp.url, emsp.token, known="04A1B2C3D4E5F6", blocked="04DEADBEEF0001"
    )
    report.assert_passed()
    assert "04A1B2C3D4E5F6" in emsp.asked


def _ask(emsp, uid):
    request = urllib.request.Request(
        f"{emsp.url}/v1/authorize",
        data=json.dumps({"uid": uid}).encode(),
        headers={"authorization": f"Bearer {emsp.token}", "content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as answer:
            return answer.status, json.load(answer)
    except urllib.error.HTTPError as answer:
        return answer.code, {}


def test_the_emsp_misbehaves_on_cue(emsp):
    emsp.on("04510A").stalls(0.3).on("04FA11").fails(503).on("04E0").expired()
    started = time.monotonic()
    assert _ask(emsp, "04510A")[1]["status"] == "Accepted"
    assert time.monotonic() - started >= 0.3
    assert _ask(emsp, "04FA11")[0] == 503
    assert _ask(emsp, "04E0")[1]["status"] == "Expired"


def test_without_a_csms_the_station_fixture_skips(pytester):
    pytester.makepyfile("def test_it(station):\n    station('CP-1', password='x')\n")
    pytester.runpytest("-p", "no:cacheprovider").assert_outcomes(skipped=1)
