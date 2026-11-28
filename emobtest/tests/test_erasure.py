"""A pre-erasure provider, invoice and cash backup restored against current key custody."""

import json
import os
import urllib.error
from pathlib import Path

import pytest

from test_against_emob import _DeskPage, _provider_decision, _read_desk
from test_customer_portal import Customer, state
from test_receivables import owner

ROOT = "/customer-erasure"
SUBJECT = {"subject": "mika"}
SCOPE = {"account": "PAYER-PRIVATE", "currency": "EUR"}


def privacy(path, fields=None):
    return _read_desk(path, fields, token=os.environ["EMOB_PRIVACY_TOKEN"])


def record():
    return Path(os.environ["EMOB_TEST_INVOICE"]).with_suffix(".privacy")


def retained(saved):
    doc = saved["invoice"]
    endpoint = "/retail-invoices/document?lang=en"
    assert _read_desk(endpoint, doc, token=os.environ["EMOB_FINANCE_TOKEN"]) == saved["ubl"]
    for key in ("EMOB_DESK_READ_TOKEN", "EMOB_OPERATOR_TOKEN", "EMOB_PRIVACY_TOKEN"):
        with pytest.raises(urllib.error.HTTPError) as rejected:
            _read_desk(endpoint, doc, token=os.environ[key])
        with rejected.value as response:
            assert response.code == 403
    assert owner("balance", SCOPE) == saved["balance"]
    assert _provider_decision("04AB0000000005")["status"] == "Accepted"
    assert state()["agreements"][1] in Customer("driver-two").login()


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "erasure-cut", reason="before customer erasure"
)
def test_capture_an_active_grant_and_partially_paid_invoice():
    mika = Customer("mika")
    assert state()["private_agreement"] in mika.login()
    session = Path(os.environ["EMOB_TEST_AGREEMENT"]).with_suffix(".session")
    number = json.loads(session.read_text())["number"]
    xml = mika.document(number)
    entry = {
        "reference": "CASH-037-CUT",
        "amount_minor": 100,
        "direction": "credit",
        "status": "booked",
    }
    source = "LOCAL-BANK"
    owner("import", {"version": 1, **SCOPE, "source": source, "entries": [entry]})
    selection = {
        **SCOPE,
        "source": source,
        "reference": entry["reference"],
        "number": number,
        "amount_minor": 100,
    }
    owner("confirm", {**selection, "review": owner("review", selection)["review"]})
    balance = owner("balance", SCOPE)
    assert any(
        d["number"] == number and 0 < d["open_minor"] < d["amount_minor"]
        for d in balance["documents"]
    )
    saved = {
        "invoice": {"account": SCOPE["account"], "period": state()["period"], "number": number},
        "ubl": xml,
        "balance": balance,
    }
    retained(saved)
    record().write_text(json.dumps(saved))


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "erasure", reason="privacy case")
def test_review_erase_and_reopen_the_durable_receipt():
    """customer-erasure-on-restore/SR-371, SR-373 and SR-374."""
    mika = Customer("mika")
    assert state()["private_agreement"] in mika.login()
    saved = json.loads(record().read_text())
    landing = privacy(ROOT + "?lang=en")
    lookup, base = _DeskPage(landing).form(ROOT + "/status")
    status = privacy(lookup, {**base, **SUBJECT})
    review, choice = _DeskPage(status).form(ROOT + "/review")
    preview = privacy(review, choice)
    confirm, operation = _DeskPage(preview).form(ROOT + "/confirm")
    assert "Review customer erasure" in preview and saved["invoice"]["number"] not in preview
    assert state()["private_agreement"] in mika.read()
    for token in ("EMOB_OPERATOR_TOKEN", "EMOB_FINANCE_TOKEN", "EMOB_DESK_READ_TOKEN"):
        with pytest.raises(urllib.error.HTTPError) as denied:
            _read_desk(review, choice, token=os.environ[token])
        with denied.value as response:
            assert response.code == 403
    with pytest.raises(urllib.error.HTTPError) as foreign:
        _read_desk(
            confirm,
            operation,
            token=os.environ["EMOB_PRIVACY_TOKEN"],
            origin="https://other.example",
        )
    with foreign.value as response:
        assert response.code == 403
    receipt = privacy(confirm, operation)
    assert (
        receipt == privacy(confirm, operation)
        and "privacy officer" in receipt
        and "Erasure receipt" in receipt
    )
    assert saved["invoice"]["number"] not in receipt and state()["private_agreement"] not in receipt
    mika.read(expected=401)
    denied_login()
    saved.update(confirm=confirm, operation=operation, receipt=receipt)
    retained(saved)
    record().write_text(json.dumps(saved))


def denied_login():
    former = Customer("mika")
    url, form = _DeskPage(former.read()).form("/customer/login")
    result = former.read(
        url, {**form, "subject": "mika", "secret": os.environ["PORTAL_TEST_MIKA"]}, expected=401
    )
    assert "My overview" not in result


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "erasure-restored",
    reason="pre-erasure operational owners replayed",
)
def test_an_old_operational_cut_cannot_reinstate_erased_links():
    """customer-erasure-on-restore/SR-372, SR-373, SR-374 and SR-375."""
    saved = json.loads(record().read_text())
    current = privacy(ROOT + "/status?lang=en", SUBJECT)
    assert current == saved["receipt"] and current == privacy(saved["confirm"], saved["operation"])
    denied_login()
    retained(saved)
