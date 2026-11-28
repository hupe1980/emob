"""Reconcile a line on a shared, partially paid invoice through the real owners."""

import json
import os
import urllib.error
from decimal import Decimal
from pathlib import Path
from xml.etree import ElementTree

import pytest

from test_against_emob import _DeskPage, _read_desk
from test_customer_portal import Customer, save, state
from test_receivables import SCOPE, case, owner


def desk(path, data=None, **kwargs):
    return _read_desk(path, data, token=os.environ["EMOB_CASE_FINANCE_TOKEN"], **kwargs)


def record():
    return Path(os.environ["EMOB_TEST_INVOICE"]).with_suffix(".line-credit")


def forbidden(call):
    with pytest.raises(urllib.error.HTTPError) as error:
        call()
    with error.value as response:
        assert response.code == 403


def xml_value(root, path):
    return root.findtext(path)


def payer_balance(customer, number):
    return customer.read("/customer/balance?lang=en", {"number": number})


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "cash", reason="partially paid fleet")
def test_review_one_fleet_line_and_reconcile_its_credit():
    invoice = state()
    number, total = case()
    agreement, other = invoice["agreements"]
    original = ElementTree.fromstring(invoice["document"])
    before = owner("balance", SCOPE)
    assert (before["net_due_minor"], len(before["allocations"])) == (total - 100, 1)
    save("finance", "account", SCOPE["account"])
    payer = Customer("finance")
    payer.login()
    page = _DeskPage(desk("/line-credits?lang=en"))
    lookup, fields = page.form("/line-credits/lookup")
    found = desk(lookup, {**fields, "number": number, "agreement": agreement})
    assert other not in found
    lines = _DeskPage(found)
    source_ids = lines.options["line_id"]
    originals = {line.findtext("./{*}ID"): line for line in original.findall("./{*}InvoiceLine")}
    source = next(
        i for i in source_ids if Decimal(originals[i].findtext("./{*}LineExtensionAmount")) > 1
    )
    review, fields = lines.form("/line-credits/review")
    inputs = {**fields, "line_id": source, "reason": "service_dispute", "amount": "1.00"}
    for key in ("EMOB_OPERATOR_TOKEN", "EMOB_FINANCE_TOKEN", "EMOB_PRIVACY_TOKEN"):
        forbidden(lambda key=key: _read_desk(review, inputs, token=os.environ[key]))
    preview = desk(review, inputs)
    confirm, command = _DeskPage(preview).form("/line-credits/confirm")
    forbidden(lambda: desk(confirm, command, origin="https://other.example"))
    receipt = desk(confirm, command)
    assert "Line credit issued" in receipt and desk(confirm, command) == receipt
    _, credit_doc = _DeskPage(receipt).form("/retail-invoices/document")
    credit_number = credit_doc["number"]
    assert credit_number != number
    credit = payer.document(credit_number)
    xml = ElementTree.fromstring(credit)
    assert xml.tag.endswith("}CreditNote")
    assert xml_value(xml, "./{*}BillingReference/{*}InvoiceDocumentReference/{*}ID") == number
    (corrected,) = xml.findall("./{*}CreditNoteLine")
    assert corrected.findtext("./{*}ID") == source
    assert Decimal(corrected.findtext("./{*}LineExtensionAmount")) == 1
    assert Decimal(xml_value(xml, "./{*}TaxTotal/{*}TaxAmount")) == Decimal("0.19")
    assert Decimal(xml_value(xml, "./{*}LegalMonetaryTotal/{*}PayableAmount")) == Decimal("1.19")
    assert payer.document(number) == invoice["document"]
    # Read the payer balance before staff refresh the accounting feed.
    view = payer_balance(payer, number)
    due = Decimal(total - 219).scaleb(-2)
    assert f"<dt>Amount due</dt><dd>{due:.2f} EUR</dd>" in view
    assert "<dt>Payments applied</dt><dd>1.00 EUR</dd>" in view
    for sensitive in ("LOCAL-BANK", "EXTERNAL-FINANCE", agreement, other):
        assert sensitive not in view
    driver = Customer("driver-two")
    overview = driver.login()
    assert other in overview and agreement not in overview and credit_number not in overview
    driver.document(credit_number, 404)
    driver.read("/customer/balance?lang=en", {"number": number}, 404)
    save("finance", "account", SCOPE["account"], False)
    payer.read("/customer/balance?lang=en", {"number": number}, 404)
    payer.document(credit_number, 404)
    after = owner("balance", SCOPE)
    assert after["net_due_minor"] == total - 219
    assert after["allocations"] == before["allocations"]
    assert after["payments"] == before["payments"]
    row = next(row for row in after["documents"] if row["number"] == credit_number)
    assert (row["kind"], row["predecessor"], row["amount_minor"]) == ("line_credit", number, 119)
    record().write_text(
        json.dumps(
            {
                "confirm": confirm,
                "command": command,
                "receipt": receipt,
                "credit": credit,
                "number": credit_number,
            }
        )
    )


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "cash-restored", reason="owners replayed")
def test_line_credit_receipt_xml_and_balance_survive_replay():
    saved = json.loads(record().read_text())
    assert desk(saved["confirm"], saved["command"]) == saved["receipt"]
    save("finance", "account", SCOPE["account"])
    payer = Customer("finance")
    assert "Line credit" in payer.login()
    assert payer.document(saved["number"]) == saved["credit"]
    number, _ = case()
    assert payer.document(number) == state()["document"]
    assert "<dt>Amount due</dt><dd>0.00 EUR</dd>" in payer_balance(payer, number)
    after = owner("balance", SCOPE)
    assert after["net_due_minor"] == 0 and after["unapplied_minor"] == 1119
    assert sum(row["number"] == saved["number"] for row in after["documents"]) == 1
