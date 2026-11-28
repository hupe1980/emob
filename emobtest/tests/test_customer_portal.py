"""Customer and staff browsers against the runner's disposable invoice estate."""

import http.cookiejar
import json
import os
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import pytest

from test_against_emob import _DeskPage, _read_desk

ROOT = os.environ.get("EMOB_PORTALD", "")
EN = "/customer/?lang=en"


def state():
    return json.loads(Path(os.environ["EMOB_TEST_INVOICE"]).read_text())


class Customer:
    def __init__(self, subject):
        self.cookies = http.cookiejar.CookieJar()
        self.browser = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.cookies))
        self.subject = subject

    def read(self, path=EN, fields=None, expected=200, origin=ROOT):
        headers = {} if fields is None else {"Origin": origin}
        data = None if fields is None else urllib.parse.urlencode(fields).encode()
        request = urllib.request.Request(ROOT + path, headers=headers, data=data)
        try:
            response = self.browser.open(request, timeout=6)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            assert response.status == expected
            assert "no-store" in response.headers["Cache-Control"]
            return response.read().decode()

    def login(self):
        secret = os.environ["PORTAL_TEST_" + self.subject.upper().replace("-", "_")]
        path, form = _DeskPage(self.read()).form("/customer/login")
        page = self.read(path, {**form, "subject": self.subject, "secret": secret})
        assert self.subject in page and secret not in page
        (cookie,) = self.cookies
        assert cookie.path == "/customer/" and cookie.has_nonstandard_attr("HttpOnly")
        return page

    def document(self, number, status=200):
        return self.read("/customer/document?lang=en", {"number": number}, status)


def grant(subject, kind, reference, granted=True):
    entry = _DeskPage(_read_desk("/customer-access?lang=en"))
    action, form = entry.form("/customer-access/open")
    current = _DeskPage(_read_desk(action, {**form, "subject": subject}))
    kwargs = {"granted": str(granted).lower()}
    if not granted:
        kwargs.update(kind=kind, reference=reference)
    review, form = current.form("/customer-access/review", **kwargs)
    decision = _read_desk(review, {**form, "kind": kind, "reference": reference})
    return _DeskPage(decision).form("/customer-access/confirm")


def save(*args):
    route, form = grant(*args)
    assert "Access saved" in _read_desk(route, form)
    return route, form


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "portal", reason="issued private estate")
def test_customer_access_and_revocation():
    """customer-invoice-access/SR-345. Cross two live owners and the customer browser."""
    invoice = state()
    fleet = invoice["number"]
    private = {"account": "PAYER-PRIVATE", "period": invoice["period"]}
    issued = _read_desk(
        "/retail-invoices/lookup?lang=en", private, token=os.environ["EMOB_FINANCE_TOKEN"]
    )
    download, private_form = _DeskPage(issued).form("/retail-invoices/document")
    original = _read_desk(download, private_form, token=os.environ["EMOB_FINANCE_TOKEN"])
    first, second = invoice["agreements"]
    for subject, kind, reference in [
        ("mika", "account", "PAYER-PRIVATE"),
        ("mika", "agreement", invoice["private_agreement"]),
        ("driver-one", "agreement", first),
        ("driver-two", "agreement", second),
    ]:
        save(subject, kind, reference)
    assignment = save("finance", "account", "PAYER-FLEET")
    customers = {
        name: Customer(name) for name in ("mika", "nico", "driver-one", "driver-two", "finance")
    }
    pages = {name: person.login() for name, person in customers.items()}
    assert customers["mika"].document(private_form["number"]) == original
    assert customers["finance"].document(fleet) == invoice["document"]
    balance = customers["finance"].read("/customer/balance?lang=en", {"number": fleet})
    assert "<dt>Payments applied</dt><dd>0.00 EUR</dd>" in balance
    assert invoice["private_agreement"] in pages["mika"] and fleet not in pages["mika"]
    for name, own, other in [("driver-one", first, second), ("driver-two", second, first)]:
        assert own in pages[name] and other not in pages[name] and fleet not in pages[name]
        receipt = next(href for href in _DeskPage(pages[name]).links if "/r/" in href)
        with urllib.request.urlopen(receipt, timeout=5) as reply:
            assert reply.status == 200
        customers[name].document(fleet, 404)
    assert not any(a in pages["finance"] for a in (first, second))
    assert not any(number in pages["nico"] for number in (fleet, private_form["number"]))
    customers["nico"].document(private_form["number"], 404)
    customers["finance"].read("/customer/document", {"number": fleet}, 403, "https://other.example")
    save("finance", "account", "PAYER-FLEET", False)
    with pytest.raises(urllib.error.HTTPError) as stale:
        _read_desk(*assignment)
    with stale.value as response:
        assert response.code == 409
    assert fleet not in customers["finance"].read()
    customers["finance"].document(fleet, 404)
    assert customers["mika"].document(private_form["number"]) == original
    who = customers["driver-one"]
    out, fields = _DeskPage(who.read()).form("/customer/logout")
    assert "My overview" not in who.read(out, fields)
    assert not list(who.cookies)
    (cookie,) = customers["mika"].cookies
    Path(os.environ["EMOB_TEST_AGREEMENT"]).with_suffix(".session").write_text(
        json.dumps({"name": cookie.name, "value": cookie.value, "number": private_form["number"]})
    )


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "offline", reason="billing stopped")
def test_unavailable_billing():
    page = Customer("nico").read()
    assert "Sign in" in page
    cookie = json.loads(Path(os.environ["EMOB_TEST_AGREEMENT"]).with_suffix(".session").read_text())
    request = urllib.request.Request(
        ROOT + EN, headers={"Cookie": cookie["name"] + "=" + cookie["value"]}
    )
    with pytest.raises(urllib.error.HTTPError) as unavailable:
        urllib.request.urlopen(request, timeout=8)
    with unavailable.value as response:
        assert response.code == 503
        body = response.read().decode()
        assert "Billing temporarily unavailable" in body and cookie["number"] not in body


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "authority-offline", reason="provider stopped"
)
def test_unavailable_authority():
    """customer-invoice-access/SR-345. Cached pages cannot authorize the next request."""
    prior = json.loads(Path(os.environ["EMOB_TEST_AGREEMENT"]).with_suffix(".session").read_text())
    headers = {"Cookie": prior["name"] + "=" + prior["value"], "Origin": ROOT}
    for path, data in [(EN, None), ("/customer/document?lang=en", {"number": prior["number"]})]:
        body = urllib.parse.urlencode(data).encode() if data else None
        request = urllib.request.Request(ROOT + path, data=body, headers=headers)
        with pytest.raises(urllib.error.HTTPError) as offline:
            urllib.request.urlopen(request, timeout=8)
        with offline.value as response:
            assert response.status == 503 and "no-store" in response.headers["Cache-Control"]
            page = response.read().decode()
            assert "Service temporarily unavailable" in page and prior["number"] not in page


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "restored", reason="owners replayed")
def test_grants_and_archive_after_replay():
    prior = json.loads(Path(os.environ["EMOB_TEST_AGREEMENT"]).with_suffix(".session").read_text())
    req = urllib.request.Request(
        ROOT + EN, headers={"Cookie": prior["name"] + "=" + prior["value"]}
    )
    with pytest.raises(urllib.error.HTTPError) as expired:
        urllib.request.urlopen(req, timeout=5)
    with expired.value as response:
        assert response.code == 401
    finance = Customer("finance")
    finance.login()
    finance.document(state()["number"], 404)
    mika = Customer("mika")
    assert prior["number"] in mika.login()
    assert "Mika Muster" in mika.document(prior["number"])
