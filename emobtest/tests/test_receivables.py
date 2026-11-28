"""Reconcile a real issued retail invoice through the finance desk and owner."""

import base64
import json
import os
import urllib.error
import urllib.request
from datetime import datetime
from decimal import Decimal
from pathlib import Path
from xml.etree import ElementTree

import pytest

from test_against_emob import _DeskPage, _read_desk
from test_customer_portal import Customer, save, state

SCOPE = {"account": "PAYER-FLEET", "currency": "EUR"}
SOURCE = "LOCAL-BANK"
REFERENCE = "EXTERNAL-FINANCE-0001"


def finance(path, fields=None):
    return _read_desk(path, fields, token=os.environ["EMOB_FINANCE_TOKEN"])


def owner(action, data, *, credential="EMOB_FINANCE_TOKEN"):
    request = urllib.request.Request(
        os.environ["EMOB_ACCOUNTINGD"] + "/v1/accountingd/" + action,
        data=json.dumps(data).encode(),
        headers={
            "Authorization": "Bearer " + os.environ[credential],
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        assert "no-store" in response.headers["Cache-Control"]
        return json.load(response)


def upload(statement):
    desk = os.environ["EMOB_DESKD"]
    boundary = "emob-standalone-statement"
    payload = (
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="file"; filename="fixture.json"\r\n'
        "Content-Type: application/json\r\n\r\n" + json.dumps(statement) + f"\r\n--{boundary}--\r\n"
    )
    credential = base64.b64encode(("desk:" + os.environ["EMOB_FINANCE_TOKEN"]).encode()).decode()
    request = urllib.request.Request(
        desk + "/receivables/upload?lang=en",
        data=payload.encode(),
        headers={
            "Authorization": "Basic " + credential,
            "Origin": desk,
            "Sec-Fetch-Site": "same-origin",
            "Content-Type": "multipart/form-data; boundary=" + boundary,
        },
    )
    with urllib.request.urlopen(request, timeout=10) as reply:
        assert "no-store" in reply.headers["Cache-Control"]
        return reply.read().decode()


def case():
    result = json.loads(Path(os.environ["EMOB_TEST_INVOICE"]).read_text())
    xml = ElementTree.fromstring(result["document"])
    total = int(100 * Decimal(xml.findtext("./{*}LegalMonetaryTotal/{*}PayableAmount")))
    return result["number"], total


def confirmation_path():
    return Path(os.environ["EMOB_TEST_INVOICE"]).with_suffix(".allocation")


def statement(amount):
    return {
        "version": 1,
        "source": SOURCE,
        **SCOPE,
        "entries": [
            {
                "reference": REFERENCE,
                "status": "booked",
                "direction": "credit",
                "amount_minor": amount,
            }
        ],
    }


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "cash", reason="issued retail invoice")
def test_finance_can_reconcile_the_issued_ubl():
    """receivables-allocation/SR-351, SR-352, SR-353 and SR-354."""
    number, total = case()
    entry = "/receivables?lang=en"
    for key in ("EMOB_DESK_READ_TOKEN", "EMOB_OPERATOR_TOKEN"):
        with pytest.raises(urllib.error.HTTPError) as refused:
            _read_desk(entry, token=os.environ[key])
        with refused.value as reply:
            assert reply.code == 403
    home = _DeskPage(finance(entry))
    load, scope = home.form("/receivables/balance")
    submit, fields = home.form("/receivables/import")
    with pytest.raises(urllib.error.HTTPError) as missing:
        finance(load, {**scope, "account": "PAYER-TYPO", "currency": "EUR"})
    with missing.value as reply:
        assert reply.code == 404
    batch = statement(total + 1000)
    assert "Statement imported" in upload(batch)
    assert "Statement imported" in finance(submit, {**fields, "statement": json.dumps(batch)})
    assert owner("import", batch)["duplicates"] == 1
    assert (
        owner("import", {**batch, "account": "PAYER-PRIVATE"})["held"][0]["reason"]
        == "reference_conflict"
    )
    assert owner("balance", {"account": "PAYER-PRIVATE", "currency": "EUR"})["unapplied_minor"] == 0
    balance = _DeskPage(finance(load, {**scope, **SCOPE}))
    review, form = balance.form("/receivables/review")
    choice = {**form, "reference": REFERENCE, "source": SOURCE, "number": number}
    first = finance(review, {**choice, "amount": "1.00"})
    later = finance(review, {**choice, "amount": "2.00"})
    confirm, decision = _DeskPage(first).form("/receivables/confirm")
    with pytest.raises(urllib.error.HTTPError) as foreign:
        _read_desk(
            confirm,
            decision,
            token=os.environ["EMOB_FINANCE_TOKEN"],
            origin="https://other.example",
        )
    with foreign.value as reply:
        assert reply.code == 403
    assert "Allocation posted" in finance(confirm, decision)
    _, stale = _DeskPage(later).form("/receivables/confirm")
    with pytest.raises(urllib.error.HTTPError) as changed:
        finance(confirm, stale)
    with changed.value as reply:
        assert reply.code == 409
    now = owner("balance", SCOPE)
    assert (now["net_due_minor"], now["unapplied_minor"]) == (total - 100, total + 900)


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "cash", reason="adjusted fleet invoice")
def test_settle_the_remaining_fleet_debt():
    number, total = case()
    scope = owner("balance", SCOPE)
    remaining = scope["net_due_minor"]
    assert remaining == total - 219
    choice = {**SCOPE, "source": SOURCE, "reference": REFERENCE, "number": number}
    finish = finance(
        "/receivables/review?lang=en", {**choice, "amount": str(Decimal(remaining).scaleb(-2))}
    )
    path, full = _DeskPage(finish).form("/receivables/confirm")
    assert "Allocation posted" in finance(path, full)
    final = owner("balance", SCOPE)
    assert (final["net_due_minor"], final["unapplied_minor"]) == (0, 1119)
    assert len(final["allocations"]) == 2 and all(
        "finance clerk" in a["actor"] for a in final["allocations"]
    )
    confirmation_path().write_text(json.dumps([path, full, final]))


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "cash-offline", reason="issuer stopped")
def test_unavailable_issuer_cannot_authorize_a_new_allocation():
    path, full, _ = json.loads(confirmation_path().read_text())
    assert "Allocation posted" in finance(path, full)
    with pytest.raises(urllib.error.HTTPError) as unavailable:
        owner(
            "review",
            {
                **SCOPE,
                "reference": REFERENCE,
                "source": SOURCE,
                "number": case()[0],
                "amount_minor": 1,
            },
        )
    with unavailable.value as reply:
        assert reply.code == 503


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "cash-restored", reason="cash ledger replayed"
)
def test_allocations_and_unapplied_cash_survive_replay():
    """receivables-allocation/SR-355."""
    path, full, before = json.loads(confirmation_path().read_text())
    assert "Allocation posted" in finance(path, full)
    assert owner("balance", SCOPE) == before
    assert owner("import", statement(case()[1] + 1000))["duplicates"] == 1


def offset_record():
    return Path(os.environ["EMOB_TEST_INVOICE"]).with_suffix(".offset")


def assert_payer_offset_views(receipt):
    payer = Customer("finance")
    payer.login()
    source, target = receipt["command"]["source_number"], receipt["command"]["target_number"]
    for number, hidden, label, balance_label, left in [
        (source, target, "Credit used on other invoices", "Credit balance", "0.19"),
        (target, source, "Credit from other invoices", "Amount due", "4.95"),
    ]:
        assert payer.document(number) == receipt["originals"][number]
        page = payer.read("/customer/balance?lang=en", {"number": number})
        assert f"<dt>{label}</dt><dd>1.00 EUR</dd>" in page
        assert f"<dt>{balance_label}</dt><dd>{left} EUR</dd>" in page
        for private in (hidden, receipt["command"]["review"], REFERENCE, "case finance reviewer"):
            assert private not in page
    german = payer.read("/customer/balance?lang=de", {"number": source})
    assert "<dt>Guthaben</dt><dd>0,19 EUR</dd>" in german
    return payer


def credit_paid_monthly_fee(source, writer):
    """Credit the other driver’s retained €5 net monthly line by €1 net."""
    first = state()
    agreement = first["agreements"][1]
    fields = {"number": source, "agreement": agreement}
    lines = _DeskPage(_read_desk("/line-credits/lookup?lang=en", fields, token=writer))
    xml = ElementTree.fromstring(first["document"])
    candidates = lines.options["line_id"]
    line_id = next(
        line.findtext("./{*}ID")
        for line in xml.findall("./{*}InvoiceLine")
        if line.findtext("./{*}ID") in candidates
        and Decimal(line.findtext("./{*}LineExtensionAmount")) == Decimal("5.00")
    )
    review, choice = lines.form("/line-credits/review")
    preview = _read_desk(
        review,
        {**choice, "line_id": line_id, "reason": "service_dispute", "amount": "1.00"},
        token=writer,
    )
    _read_desk(*_DeskPage(preview).form("/line-credits/confirm"), token=writer)


def issue_next_fleet_month(writer):
    period = datetime.fromisoformat(os.environ["EMOB_TEST_WITHDRAWAL_USE"]).strftime("%Y-%m")
    next_invoice = _read_desk(
        "/retail-invoices/review?lang=en",
        {"account": SCOPE["account"], "period": period},
        token=writer,
    )
    issued = _read_desk(*_DeskPage(next_invoice).form("/retail-invoices/close"), token=writer)
    download, fields = _DeskPage(issued).form("/retail-invoices/document")
    target = fields["number"]
    target_xml = _read_desk(download, fields, token=writer)
    payable = ElementTree.fromstring(target_xml).findtext(
        "./{*}LegalMonetaryTotal/{*}PayableAmount"
    )
    assert Decimal(payable) == Decimal("5.95")
    return target, target_xml


@pytest.mark.skipif(os.environ.get("EMOB_TEST_ESTATE") != "withdrawal", reason="two closed months")
def test_apply_credit_to_a_second_fleet_invoice():
    """customer-credit-offset/SR-030 and customer-credit-offset/SR-031."""
    writer = os.environ["EMOB_CASE_FINANCE_TOKEN"]
    source, _ = case()
    credit_paid_monthly_fee(source, writer)
    target, target_xml = issue_next_fleet_month(writer)
    assert source != target
    before = owner("balance", SCOPE)
    assert before["credit_minor"] == 119 and before["net_due_minor"] == 476

    request = {**SCOPE, "source_number": source, "target_number": target, "amount_minor": 100}
    for credential in ("EMOB_FINANCE_TOKEN", "EMOB_OPERATOR_TOKEN"):
        with pytest.raises(urllib.error.HTTPError) as denied:
            owner("offsets/review", request, credential=credential)
        with denied.value as reply:
            assert reply.code == 403
    review_token = owner("offsets/review", request, credential="EMOB_CASE_FINANCE_TOKEN")["review"]
    command = {**request, "review": review_token}
    second_review = owner("offsets/review", request, credential="EMOB_SECOND_FINANCE_TOKEN")[
        "review"
    ]
    assert second_review != review_token
    competing = {**request, "review": second_review}
    with pytest.raises(urllib.error.HTTPError) as unauthorized_confirm:
        owner("offsets/confirm", command)
    with unauthorized_confirm.value as reply:
        assert reply.code == 403
    reader = finance("/receivables/balance?lang=en", SCOPE)
    assert "/receivables/offsets/review" not in reader
    balance = _DeskPage(_read_desk("/receivables/balance?lang=en", SCOPE, token=writer))
    review, selection = balance.form("/receivables/offsets/review")
    preview = _read_desk(
        review,
        {**selection, "source_number": source, "target_number": target, "amount": "1.00"},
        token=writer,
    )
    confirm, form = _DeskPage(preview).form("/receivables/offsets/confirm")
    assert form["review"] == review_token
    receipt_page = _read_desk(confirm, form, token=writer)
    assert "Credit applied" in receipt_page and "case finance reviewer" in receipt_page
    accepted = owner("offsets/confirm", command, credential="EMOB_CASE_FINANCE_TOKEN")
    assert [
        accepted[key]
        for key in (
            "source_before_minor",
            "source_after_minor",
            "target_before_minor",
            "target_after_minor",
        )
    ] == [-119, -19, 595, 495]
    with pytest.raises(urllib.error.HTTPError) as changed:
        owner("offsets/confirm", competing, credential="EMOB_SECOND_FINANCE_TOKEN")
    with changed.value as reply:
        assert reply.code == 409
    with pytest.raises(urllib.error.HTTPError) as outdated:
        _read_desk(
            confirm,
            {**form, "review": second_review},
            token=os.environ["EMOB_SECOND_FINANCE_TOKEN"],
        )
    with outdated.value as reply:
        conflict = reply.read().decode()
        assert reply.code == 409 and "0.19" in conflict and "4.95" in conflict
        assert 'role="alert"' in conflict
        assert _DeskPage(conflict).form("/receivables/offsets/review")
        assert "/receivables/offsets/confirm?" not in conflict
    after = owner("balance", SCOPE)
    for key in ("net_due_minor", "unapplied_minor", "payments", "allocations"):
        assert before[key] == after[key]
    assert after["credit_minor"] == 19 and len(after["offsets"]) == 1
    saved = {
        "command": command,
        "accepted": accepted,
        "competing": competing,
        "after": after,
        "originals": {source: state()["document"], target: target_xml},
    }
    payer = assert_payer_offset_views(saved)
    unrelated = Customer("mika")
    unrelated.login()
    for number in (source, target):
        unrelated.read("/customer/balance?lang=en", {"number": number}, 404)
    save("finance", "account", SCOPE["account"], False)
    for number in (source, target):
        payer.read("/customer/balance?lang=en", {"number": number}, 404)
    offset_record().write_text(json.dumps(saved))


@pytest.mark.skipif(
    os.environ.get("EMOB_TEST_ESTATE") != "withdrawal-restored",
    reason="both invoice owners replayed",
)
def test_offset_survives_billing_and_accounting_restart():
    """customer-credit-offset/SR-030 and customer-credit-offset/SR-031."""
    saved = json.loads(offset_record().read_text())
    assert (
        owner("offsets/confirm", saved["command"], credential="EMOB_CASE_FINANCE_TOKEN")
        == saved["accepted"]
    )
    with pytest.raises(urllib.error.HTTPError) as stale:
        owner("offsets/confirm", saved["competing"], credential="EMOB_SECOND_FINANCE_TOKEN")
    with stale.value as reply:
        assert reply.code == 409
    assert owner("balance", SCOPE) == saved["after"]
    blocked = Customer("finance")
    blocked.login()
    blocked.read("/customer/balance?lang=en", {"number": saved["command"]["source_number"]}, 404)
    save("finance", "account", SCOPE["account"])
    assert_payer_offset_views(saved)
