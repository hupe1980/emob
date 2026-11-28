"""A paid standalone agreement from the driver's tap through withdrawal and credit."""

import json
import os
import time
import urllib.error
import urllib.request
from datetime import UTC, datetime
from decimal import Decimal
from pathlib import Path
from xml.etree import ElementTree

import pytest

from test_against_emob import (
    CP1_KEY,
    ESTATE_METER,
    _decision,
    _DeskPage,
    _provider_decision,
    _read_desk,
    _set_point_offer,
)
from test_customer_portal import Customer, state
from test_receivables import owner

CASE = "/contracts/withdrawal"
ACCOUNT = "PAYER-PRIVATE"
SCOPE = {"account": ACCOUNT, "currency": "EUR"}
PRIVATE_CARD = "04AB0000000007"
DECLARATION = "POSTROOM-036-431"
DOSSIER = "FILE-036-721"
PAYMENT = {"source": "FIXTURE-BANK", "reference": "PAY-036-451"}


def receipt_file():
    return Path(os.environ["EMOB_TEST_INVOICE"]).with_suffix(".withdrawal")


def submit(page, action, *, token=None, **data):
    path, fields = _DeskPage(page).form(CASE + "/" + action)
    return _read_desk(path, {**fields, **data}, token=token or os.environ["EMOB_OPERATOR_TOKEN"])


def lookup(agreement, token=None):
    return _read_desk(
        CASE + "/lookup?lang=en",
        {"agreement": agreement},
        token=token or os.environ["EMOB_OPERATOR_TOKEN"],
    )


def document(number):
    xml = Customer("mika")
    xml.login()
    return xml.document(number)


def payable(xml):
    return Decimal(ElementTree.fromstring(xml).findtext("./{*}LegalMonetaryTotal/{*}PayableAmount"))


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "withdrawal", reason="private issued agreement"
)
def test_a_paid_driver_withdraws_and_finance_issues_one_reviewed_credit(station):
    """contract-withdrawal-review/SR-361, SR-362, SR-363, SR-364 and SR-365."""
    agreement = state()["private_agreement"]
    finance_writer = os.environ["EMOB_CASE_FINANCE_TOKEN"]
    moment = datetime.fromisoformat(os.environ["EMOB_TEST_WITHDRAWAL_USE"])
    _set_point_offer(True)
    cp = station("CP-1", password=CP1_KEY, ocpp="1.6", **ESTATE_METER)
    run = cp.charge(card=PRIVATE_CARD, minutes=5, started_at=moment).assert_accepted()
    assert _decision("CP-1", run.transaction)["billed"]
    period = {"account": ACCOUNT, "period": moment.strftime("%Y-%m")}
    deadline = time.monotonic() + 15
    while True:
        preview = _read_desk("/retail-invoices/review?lang=en", period, token=finance_writer)
        if "Charging session ·" in preview:
            break
        assert time.monotonic() < deadline, "The charged session did not reach the invoice"
        time.sleep(0.1)
    issued = _read_desk(*_DeskPage(preview).form("/retail-invoices/close"), token=finance_writer)
    route, args = _DeskPage(issued).form("/retail-invoices/document")
    number, original = args["number"], _read_desk(route, args, token=finance_writer)
    assert payable(original) > Decimal("5.95")
    assert document(number) == original
    owner(
        "import",
        {
            "version": 1,
            **SCOPE,
            "source": PAYMENT["source"],
            "entries": [
                {
                    "reference": PAYMENT["reference"],
                    "amount_minor": 100,
                    "status": "booked",
                    "direction": "credit",
                }
            ],
        },
    )
    allocation = {**SCOPE, **PAYMENT, "number": number, "amount_minor": 100}
    allocation["review"] = owner("review", allocation)["review"]
    owner("confirm", allocation)
    before = owner("balance", SCOPE)

    page = lookup(agreement)
    filing = {
        "request_ref": DECLARATION,
        "received_at": moment.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%S"),
        "channel": "email",
    }
    case = submit(page, "intake", **filing)
    assert DECLARATION in case and submit(page, "intake", **filing) == case
    with pytest.raises(urllib.error.HTTPError) as reader:
        lookup(agreement, os.environ["EMOB_DESK_READ_TOKEN"])
    with reader.value as response:
        assert response.code == 403
    review = submit(case, "review", outcome="hold", billing="review_required", evidence_ref=DOSSIER)
    case = submit(review, "confirm")
    assert DOSSIER in case and _provider_decision(PRIVATE_CARD)["status"] == "Accepted"
    mika = Customer("mika")
    assert "Under review" in mika.login()
    assert DOSSIER not in mika.read() and DECLARATION not in mika.read()
    assert "Withdrawal" not in Customer("driver-two").login()
    review = submit(
        case, "review", outcome="approve", billing="review_required", evidence_ref=DOSSIER
    )
    confirm, proposal = _DeskPage(review).form(CASE + "/confirm")
    with pytest.raises(urllib.error.HTTPError) as origin:
        _read_desk(confirm, proposal, origin="https://other.example")
    with origin.value as response:
        assert response.code == 403
    decided = _read_desk(confirm, proposal)
    assert decided == _read_desk(confirm, proposal)
    assert _provider_decision(PRIVATE_CARD)["status"] != "Accepted"
    cp.charge(card=PRIVATE_CARD, minutes=5).assert_refused("Blocked")
    assert _provider_decision("04AB0000000005")["status"] == "Accepted"
    customer_page = mika.read()
    assert "Approved" in customer_page and mika.document(number) == original
    receipt = next(href for href in _DeskPage(customer_page).links if "/r/" in href)
    with urllib.request.urlopen(receipt, timeout=5) as reply:
        assert reply.status == 200

    finance = lookup(agreement, finance_writer)
    evidence = submit(finance, "evidence", token=finance_writer, number=number)
    assert PAYMENT["reference"] in evidence and number in evidence
    credit_review = submit(evidence, "credit-review", token=finance_writer)
    commit, fields = _DeskPage(credit_review).form(CASE + "/credit-confirm")
    with pytest.raises(urllib.error.HTTPError) as denied:
        _read_desk(commit, fields)
    with denied.value as response:
        assert response.code == 403
    posted = _read_desk(commit, fields, token=finance_writer)
    assert _read_desk(commit, fields, token=finance_writer) == posted
    after = owner("balance", SCOPE)
    assert after["allocations"] == before["allocations"]
    assert after["net_due_minor"] == before["net_due_minor"] - int(100 * payable(original))
    assert (after["credit_minor"], after["unapplied_minor"]) == (100, 0)
    credits = [
        d["number"]
        for d in after["documents"]
        if d["kind"] == "credit" and d["predecessor"] == number
    ]
    assert len(credits) == 1
    credit_xml = document(credits[0])
    credit = ElementTree.fromstring(credit_xml)
    assert credit.tag.endswith("}CreditNote")
    assert credit.findtext("./{*}BillingReference/{*}InvoiceDocumentReference/{*}ID") == number
    assert payable(credit_xml) == payable(original)
    receipt_file().write_text(
        json.dumps(
            {
                "confirm": confirm,
                "proposal": proposal,
                "commit": commit,
                "fields": fields,
                "number": number,
                "original": original,
                "balance": after,
            }
        )
    )


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "withdrawal-restored", reason="withdrawal owners replayed"
)
def test_standing_original_invoice_and_cash_reconcile_after_replay():
    saved = json.loads(receipt_file().read_text())
    assert DOSSIER in _read_desk(saved["confirm"], saved["proposal"])
    assert _provider_decision(PRIVATE_CARD)["status"] != "Accepted"
    assert _provider_decision("04AB0000000005")["status"] == "Accepted"
    _read_desk(saved["commit"], saved["fields"], token=os.environ["EMOB_CASE_FINANCE_TOKEN"])
    balance = owner("balance", SCOPE)
    assert balance == saved["balance"]
    assert document(saved["number"]) == saved["original"]
    assert "Approved" in Customer("mika").login()
