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

import base64
import json
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from html.parser import HTMLParser
from pathlib import Path

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


def _decision(identity: str, transaction: str) -> dict:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        for row in _api(f"/v1/csmsd/stations/{identity}")["recent"]:
            if row["transaction"] == transaction:
                return row
        time.sleep(0.05)
    raise AssertionError(f"{identity} did not record a decision for {transaction}")


@pytest.fixture(scope="module", autouse=True)
def live_estate_selected(request):
    if not request.config.getoption("--emob-ocpp"):
        pytest.skip("no CSMS to drive: pass --emob-ocpp wss://…/ocpp")


@pytest.fixture(scope="module")
def onboarded() -> dict[str, tuple[str, str]]:
    """CP-1, and a 2.0.1 and a 2.1 station registered at CP-1's location."""
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
                "publicly_accessible": True,
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
    assert _decision(cp.identity, session.transaction)["billed"], (
        "csmsd bills a session its own check would"
    )


def test_reconnected_station_records_a_distinct_transaction(station, onboarded):
    identity, password = onboarded["2.1"]
    first = station(identity, password=password, ocpp="2.1", **ESTATE_METER)
    billed = first.charge(card=KNOWN, minutes=5).assert_accepted()
    first.close()
    second = station(identity, password=password, ocpp="2.1", **ESTATE_METER)
    refused = second.charge(card=KNOWN, minutes=5, fault="tampered").assert_accepted()
    assert billed.transaction != refused.transaction
    previous = _decision(identity, billed.transaction)
    current = _decision(identity, refused.transaction)
    assert (
        previous["position"] != current["position"] and previous["billed"] and not current["billed"]
    )


def test_the_station_page_says_it_is_emobtest(cp):
    reported = _api(f"/v1/csmsd/stations/{cp.identity}")["reported_identity"]
    assert reported["vendor"] == "emobtest" and reported["firmware"] == "emobtest", reported


@pytest.mark.parametrize("fault", ["tampered", "dropped"])
def test_a_session_the_meter_cannot_vouch_for_is_not_billed(cp, fault):
    session = cp.charge(card=KNOWN, minutes=15, fault=fault).assert_accepted()
    with pytest.raises(AssertionError):
        session.assert_bills()
    last = _decision(cp.identity, session.transaction)
    assert not last["billed"], f"a {fault} reading was billed [MessEG §33]"
    receipt = urllib.request.urlopen(CSMSD + last["receipt"], timeout=10).read().decode()
    assert "Nicht abgerechnet" in receipt, "the receipt tells the driver"


def test_a_blocked_card_does_not_charge(cp):
    cp.charge(card=BLOCKED, minutes=5).assert_refused("Blocked")


@pytest.mark.parametrize("cp", ["2.0.1", "2.1"], indirect=True)
def test_a_2x_display_shows_the_receipt_and_the_price(cp):
    """SetDisplayMessage exists on the two 2.x protocols."""
    cp.expect_display("Preisstand")  # the price, once the station has booted
    session = cp.charge(card=KNOWN, minutes=10).assert_accepted()
    assert _decision(cp.identity, session.transaction)["receipt"] in cp.expect_display("Beleg:")


@pytest.mark.parametrize(
    ("tariff", "gross", "wire"),
    [("station-display", "1,19", True), ("ad-hoc", "0,49", False)],
)
def test_a_station_tariff_has_a_separate_receiver_result(station, tariff, gross, wire):
    """SR-209: a display acknowledgement and an unsupported OCPP tariff are separate facts."""
    from datetime import datetime

    site = _api("/v1/csmsd/stations/CP-1")["location"]
    identity = f"ET-TARIFF-{uuid.uuid4().hex[:6].upper()}"
    evse = f"DE*ABC*E{uuid.uuid4().int % 10**8:08d}"
    registered = _api(
        "/v1/csmsd/stations",
        {
            "identity": identity,
            "ocpp": "2.1",
            "security_profile": 2,
            "evse": evse,
            "rated_power_kw": "60",
            "tariff": tariff,
            "connector": "iec62196_t2",
            "location": site,
            "show_price": True,
            "publicly_accessible": True,
        },
    )
    cp = station(identity, password=registered["password"], ocpp="2.1", **ESTATE_METER)
    assert gross in cp.expect_display("Preisstand", within=15)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        delivery = _api(f"/v1/csmsd/stations/{identity}")["station_tariff"]
        if delivery:
            break
        time.sleep(0.1)
    else:
        pytest.fail("No OCPP 2.1 tariff receipt for this boot")
    if wire:
        assert cp.asked.count("SetDefaultTariff") == 1
        assert delivery["outcome"] == "CALLERROR NotSupported", delivery
    else:
        assert "SetDefaultTariff" not in cp.asked
        assert "eighteen decimals" in delivery["outcome"] and "0.49" in delivery["outcome"]
    assert delivery["retry"] is False
    assert len(bytes.fromhex(delivery["version"])) == 32
    assert datetime.fromisoformat(delivery["at"]) >= datetime.fromisoformat(delivery["boot"])
    assert "physical" not in delivery
    points = _api("/v1/csmsd/public-points")
    point = next(p for p in points["points"] if p["identity"] == identity)
    assert point["evse"] == evse and point["version"] == delivery["facility_version"]
    assert point["site_version"] >= 1
    assert datetime.fromisoformat(point["status_changed_at"]) >= datetime.fromisoformat(
        point["changed_at"]
    )
    assert not {"password_hash", "reservation", "notes"}.intersection(point)


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


def test_public_provider_price_and_saved_terms(request):
    """SR-200, SR-203, SR-205. Lookup uses public identifiers and no operator credentials."""
    if not request.config.getoption("--emob-ocpp"):
        pytest.skip("no live estate: pass --emob-ocpp wss://…/ocpp")
    point = "/p/DE*ABC*E00001"
    offer = point + "/providers/DE*MSP/standard"
    with urllib.request.urlopen(CSMSD + point, timeout=10) as response:
        assert offer in response.read().decode()
    request = urllib.request.Request(CSMSD + offer, headers={"Accept-Language": "en"})
    with urllib.request.urlopen(request, timeout=10) as response:
        terms = response.read().decode()
        assert response.headers["Cache-Control"] == "no-store"
        assert "E-Roaming" in terms and "6.30 EUR" in terms and "MSP Standard" in terms
        assert KNOWN not in terms and BLOCKED not in terms
        reference = re.search(r'href="(/p/offers/[a-f0-9]{64})"', terms)
        assert reference
    with urllib.request.urlopen(CSMSD + reference.group(1) + "?lang=en", timeout=10) as response:
        assert "Archived terms" in response.read().decode()


class _DeskPage(HTMLParser):
    def __init__(self, html: str):
        super().__init__()
        self.links: list[str] = []
        self.options: dict[str, list[str]] = {}
        self.select: str | None = None
        self.forms: list[tuple[str, dict[str, str]]] = []
        self.current_form: dict[str, str] | None = None
        self.feed(html)

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if tag == "select":
            self.select = attributes.get("name")
        if tag == "option" and self.select and attributes.get("value"):
            self.options.setdefault(self.select, []).append(attributes["value"])
        if tag == "a" and "href" in attributes:
            self.links.append(attributes["href"])
        if tag == "form" and attributes.get("method", "").lower() == "post":
            self.current_form = {}
            self.forms.append((attributes["action"], self.current_form))
        if tag == "input" and attributes.get("type") == "hidden":
            if self.current_form is not None:
                self.current_form[attributes["name"]] = attributes["value"]

    def handle_endtag(self, tag):
        if tag == "select":
            self.select = None
        if tag == "form":
            self.current_form = None

    def form(self, path, **matching):
        return next(
            (action, fields)
            for action, fields in self.forms
            if action.split("?")[0] == path and matching.items() <= fields.items()
        )


def _read_desk(path, fields=None, *, token=TOKEN, origin=None):
    desk = os.environ.get("EMOB_DESKD", "http://127.0.0.1:9587")
    headers = {"Authorization": "Basic " + base64.b64encode(f"desk:{token}".encode()).decode()}
    data = None
    if fields is not None:
        headers["Origin"] = origin or desk
        headers["Sec-Fetch-Site"] = "same-origin" if headers["Origin"] == desk else "cross-site"
        data = urllib.parse.urlencode(fields).encode()
    req = urllib.request.Request(desk + path, headers=headers, data=data)
    with urllib.request.urlopen(req, timeout=10) as reply:
        page = reply.read()
        assert len(page) < 150_000
        assert "no-store" in reply.headers["Cache-Control"]
        return page.decode()


def test_follow_a_refused_session_from_the_operator_desk(station, onboarded):
    """around-a-session/SR-030, around-a-session/SR-031, around-a-session/SR-032."""
    desk = os.environ.get("EMOB_DESKD", "http://127.0.0.1:9587")
    identity, password = onboarded["2.1"]
    cp = station(identity, password=password, ocpp="2.1", **ESTATE_METER)
    session = cp.charge(card=KNOWN, minutes=15, fault="tampered").assert_accepted()
    last = _decision(identity, session.transaction)
    assert not last["billed"]
    decision = next(
        item
        for item in _api("/v1/csmsd/today")["refused"]["items"]
        if item["position"] == last["position"]
    )
    assert decision["identity"] == identity and decision["transaction"] == last["transaction"]

    home = _DeskPage(_read_desk("/?lang=en"))
    path = f"/sessions/{decision['position']}?lang=en"
    assert path in home.links
    evidence = _read_desk(path)
    assert "Session evidence" in evidence and "Not billed" in evidence and KNOWN not in evidence
    subject = _DeskPage(evidence)
    station_path = f"/station/{identity}?lang=en"
    assert station_path in subject.links and identity in _read_desk(station_path)
    assert CSMSD + last["receipt"] in subject.links
    with urllib.request.urlopen(CSMSD + last["receipt"], timeout=10) as receipt:
        assert "Nicht abgerechnet" in receipt.read().decode()
    card = _read_desk(*subject.form("/lookup", kind="reference"))
    assert "Card unblocked" in card and KNOWN not in card
    visual = re.search(r"EM-[A-F0-9]{12}", card)
    assert visual
    for kind, value in (("visual", "MOB-1001"), ("emaid", "DE-MSP-C00000001-W")):
        found = _read_desk("/lookup?lang=en", {"kind": kind, "value": value})
        assert visual[0] in found and value not in found and KNOWN not in found
    hits = _DeskPage(
        _read_desk("/lookup?lang=en", {"kind": "transaction", "value": last["transaction"]})
    )
    assert path in hits.links
    with pytest.raises(urllib.error.HTTPError) as denied:
        urllib.request.urlopen(desk + path, timeout=10)
    with denied.value as reply:
        assert reply.code == 401


def _open_card(printed, *, token=TOKEN):
    listing = _DeskPage(
        _read_desk("/lookup?lang=en", {"kind": "visual", "value": printed}, token=token)
    )
    return _read_desk(*listing.form("/lookup", kind="reference"), token=token)


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "private", reason="private estate only")
def test_named_card_actions_change_online_charging(station):
    """Block one card and recover another. Leave both decisions for the real restart test."""
    cp = station("CP-1", password=CP1_KEY, ocpp="1.6", **ESTATE_METER)
    reader = os.environ["EMOB_DESK_READ_TOKEN"]
    for printed, uid, blocking, reason in (
        ("MOB-1001", KNOWN, True, "lost"),
        ("MOB-1003", BLOCKED, False, "recovered"),
    ):
        preview_path, fields = _DeskPage(_open_card(printed)).form("/cards/preview")
        assert fields["blocked"] == str(blocking).lower()
        assert "/cards/preview" not in _open_card(printed, token=reader)
        fields["reason"] = reason
        with pytest.raises(urllib.error.HTTPError) as foreign:
            _read_desk(preview_path, fields, origin="https://other.example")
        with foreign.value as reply:
            assert reply.code == 403
        review = _read_desk(preview_path, fields)
        assert "offline" in review.lower() and uid not in review and TOKEN not in review
        confirm_path, operation = _DeskPage(review).form("/cards/confirm")
        # Review alone has no charging effect.
        attempt = cp.charge(card=uid, minutes=5)
        if blocking:
            attempt.assert_accepted()
        else:
            attempt.assert_refused("Blocked")
        result = _read_desk(confirm_path, operation)
        assert ("Card blocked" if blocking else "Card unblocked") in result
        assert "demo operator" in result
        assert operation["operation"] not in confirm_path
        attempt = cp.charge(card=uid, minutes=5)
        if blocking:
            attempt.assert_refused("Blocked")
        else:
            attempt.assert_accepted()


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "replayed", reason="after private provider restart"
)
def test_named_card_actions_survive_provider_restart(station):
    """heute-acts/SR-033. Query the restored owner and charge on its replayed standing."""
    cp = station("CP-1", password=CP1_KEY, ocpp="1.6", **ESTATE_METER)
    for printed, uid, blocked in (("MOB-1001", KNOWN, True), ("MOB-1003", BLOCKED, False)):
        page = _open_card(printed, token=os.environ["EMOB_DESK_READ_TOKEN"])
        assert ("Card blocked" if blocked else "Card unblocked") in page
        assert "demo operator" in page and uid not in page
        assert "/cards/confirm" not in page and "/cards/preview" not in page
        attempt = cp.charge(card=uid, minutes=5)
        if blocked:
            attempt.assert_refused("Blocked")
        else:
            attempt.assert_accepted()


STOCK_CODE = "STOCK-1004"
STOCK_UID = "04AB0000000004"


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "private", reason="private estate only")
def test_enrol_a_stocked_card_at_its_agreed_point(station, onboarded):
    """native-driver-enrolment/SR-303 and native-driver-enrolment/SR-306."""
    home = _DeskPage(_read_desk("/?lang=en"))
    entry = next(href for href in home.links if href.split("?")[0] == "/contracts")
    listing = _DeskPage(_read_desk(entry.split("?")[0] + "?lang=en"))
    selected = next(
        value for value in listing.options["selection"] if value.startswith("DE*ABC*E00001|")
    )
    preview_path, fields = listing.form("/contracts/preview")
    fields.update(selection=selected, stock=STOCK_CODE)
    for token, origin in (
        (os.environ["EMOB_DESK_READ_TOKEN"], None),
        (TOKEN, "https://other.example"),
    ):
        with pytest.raises(urllib.error.HTTPError) as denied:
            _read_desk(preview_path, fields, token=token, origin=origin)
        with denied.value as reply:
            assert reply.code == 403
    review = _read_desk(preview_path, fields)
    assert "Review driver contract" in review and "DE*ABC*E00001" in review
    assert STOCK_UID not in review and STOCK_CODE not in review and TOKEN not in review
    cp = station("CP-1", password=CP1_KEY, ocpp="1.6", **ESTATE_METER)
    cp.charge(card=STOCK_UID, minutes=5).assert_refused("Invalid")
    confirm_path, form = _DeskPage(review).form("/contracts/confirm")
    receipt = _read_desk(confirm_path, form)
    operation = form["operation"]
    assert re.fullmatch(r"[0-9a-f]{64}", operation)
    assert "Agreement issued" in receipt and "demo operator" in receipt and operation in receipt
    assert STOCK_UID not in receipt and STOCK_CODE not in receipt
    assert all(operation not in href for href in _DeskPage(receipt).links)
    assert _read_desk(confirm_path, form) == receipt
    billed = cp.charge(card=STOCK_UID, minutes=5).assert_accepted()
    assert _decision("CP-1", billed.transaction)["billed"]
    identity, password = onboarded["2.1"]
    other = station(identity, password=password, ocpp="2.1", **ESTATE_METER)
    other.charge(card=STOCK_UID, minutes=5).assert_refused("Invalid")
    Path(os.environ["EMOB_TEST_AGREEMENT"]).write_text(operation)


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "replayed", reason="after private provider restart"
)
def test_native_agreement_survives_restart_and_offer_withdrawal(station):
    """native-driver-enrolment/SR-304. Reopen the same agreement after real replay."""
    operation = Path(os.environ["EMOB_TEST_AGREEMENT"]).read_text()
    reader = os.environ["EMOB_DESK_READ_TOKEN"]
    page = _DeskPage(_read_desk("/contracts?lang=en", token=reader))
    assert not any(action.startswith("/contracts/preview") for action, _ in page.forms)
    path, fields = page.form("/contracts/lookup")
    receipt = _read_desk(path, {**fields, "agreement": operation}, token=reader)
    repeated = _read_desk("/contracts/confirm?lang=en", {"operation": operation})
    assert receipt == repeated and operation in receipt and "Agreement issued" in receipt
    cp = station("CP-1", password=CP1_KEY, ocpp="1.6", **ESTATE_METER)
    cp.charge(card=STOCK_UID, minutes=5).assert_accepted()
    # Withdraw publication from the live registry, then present the same physical card again.
    edit = _DeskPage(_read_desk("/station/CP-1/edit?lang=en"))
    path, fields = edit.form("/station/CP-1/edit")
    fields.update(
        identity="CP-1",
        ocpp="1.6",
        security_profile="2",
        evse="DE*ABC*E00001",
        rated_power_kw="150",
        tariff="ad-hoc",
        connector="iec62196_t2_combo",
        location_id="LOC-1",
        location_name="Depot Mitte",
        street="Invalidenstrasse",
        house_number="1",
        postal_code="10115",
        city="Berlin",
        country="DE",
        latitude="52.5318",
        longitude="13.3849",
        time_zone="Europe/Berlin",
        reason="End the local offer",
        publicly_accessible="false",
        show_price="false",
        fully_renewable="false",
    )
    _read_desk(path, fields)
    cp.charge(card=STOCK_UID, minutes=5).assert_refused("Invalid")
    cp.charge(card=OTHER, minutes=5).assert_accepted()
    with pytest.raises(urllib.error.HTTPError) as refused:
        _api(
            "/v1/csmsd/stations/CP-1/reservations",
            {"uid": STOCK_UID, "minutes": 5, "reason": "Check withdrawn terms"},
        )
    with refused.value as reply:
        assert reply.code == 422
    assert not cp.reservations

    assert (
        _read_desk("/contracts/lookup?lang=en", {"agreement": operation}, token=reader) == receipt
    )
