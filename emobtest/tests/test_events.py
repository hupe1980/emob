"""Events: the schemas, Standard Webhooks, the inbox, and the two conformance
suites — the receiver suite run against emobtest's own inbox."""

import json
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import emobtest as et
from emobtest import cli, conformance, webhooks


def test_the_sample_event_is_what_the_schema_says_and_a_broken_one_is_named():
    event = conformance.sample_cdr_event()
    et.assert_event(event, type="de.emob.cdr.issued")
    broken = dict(event, data={"station": 1})
    with pytest.raises(AssertionError, match="schema"):
        et.assert_event(broken)
    assert "de.emob.invoice.issued" in et.EVENT_TYPES
    production = dict(event)
    del production["emobtest"]
    assert et.event_problems(production), "a dry-run record cannot satisfy the production schema"


def test_a_signature_holds_only_for_its_secret_body_and_time():
    secret, body = webhooks.new_secret(), b'{"a":1}'
    headers = webhooks.sign(secret, body)
    webhooks.verify(secret, headers, body)
    with pytest.raises(ValueError, match="does not match"):
        webhooks.verify(secret, headers, b'{"a":2}')
    with pytest.raises(ValueError, match="does not match"):
        webhooks.verify(secret, dict(headers, **{"webhook-signature": "é"}), body)
    with pytest.raises(ValueError, match="off"):
        webhooks.verify(secret, headers, body, now=int(headers["webhook-timestamp"]) + 3600)


def test_upstream_standard_webhooks_vector_and_empty_key():
    # Fixed vector from the Standard Webhooks Rust SDK's test_sign.
    key = "whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD"
    msg_id = "msg_27UH4WbU6Z5A5EzD8u03UvzRbpk"
    timestamp = 1649367553
    body = b'{"email":"test@example.com","username":"test_user"}'
    headers = webhooks.sign(key, body, msg_id=msg_id, timestamp=timestamp)
    assert headers["webhook-signature"] == "v1,tZ1I4/hDygAJgO5TYxiSd6Sd0kDW6hPenDe+bTa3Kkw="
    webhooks.verify(key, headers, body, now=timestamp)
    with pytest.raises(ValueError, match="empty"):
        webhooks.sign("whsec_", body)


def _deliver(inbox, event, secret=None):
    body = json.dumps(event).encode()
    headers = webhooks.sign(secret or inbox.secret, body, msg_id=event["id"])
    request = urllib.request.Request(inbox.url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(request) as answer:
            return answer.status
    except urllib.error.HTTPError as answer:
        return answer.code


def test_the_inbox_takes_a_signed_event_once_and_refuses_a_forgery(inbox):
    event = conformance.sample_cdr_event()
    assert _deliver(inbox, event) == 204
    assert _deliver(inbox, event) == 200, "a duplicate is acknowledged, not booked twice"
    assert _deliver(inbox, event, secret=webhooks.new_secret()) == 401
    got = inbox.expect("de.emob.cdr.issued", within=1)
    assert got.is_test and got.data["signed"]
    assert len(inbox.events) == 1
    with pytest.raises(AssertionError, match=r"no de\.emob\.invoice\.issued within"):
        inbox.expect("de.emob.invoice.issued", within=0.2)


@pytest.mark.parametrize("event_type", et.EVENT_TYPES)
def test_emobtests_own_inbox_conforms_as_a_receiver(inbox, event_type, capsys, monkeypatch):
    monkeypatch.setenv("TEST_RECEIVER_SECRET", inbox.secret)
    result = cli.main(
        [
            "conformance",
            "receiver",
            inbox.url,
            "--secret-env",
            "TEST_RECEIVER_SECRET",
            "--event-type",
            event_type,
        ]
    )
    assert result == 0
    assert "5/5 held" in capsys.readouterr().out
    assert inbox.expect(event_type).is_test


def test_receiver_probe_requires_an_authentication_refusal(monkeypatch):
    statuses = iter([202, 202, 500, 500, 500])
    monkeypatch.setattr(conformance, "_post", lambda *_args: (next(statuses), b"", 0))
    report = conformance.receiver("https://example.invalid/events", webhooks.new_secret())
    assert [result.passed for result in report.results] == [True, True, False, False, False]


def test_accepted_answer_must_be_readable_by_the_charging_station(monkeypatch):
    accepted = {"status": "Accepted", "reason": "valid", "token": {"contract_id": "DE-EMT-C1"}}
    invalid = {"status": "Invalid", "reason": "unknown"}
    replies = iter([(401, {}), (401, {}), (200, invalid), (200, accepted)])

    def post(*_args):
        status, answer = next(replies)
        return status, json.dumps(answer).encode(), 0

    monkeypatch.setattr(conformance, "_post", post)
    report = conformance.authorize("https://example.invalid", "example", known="KNOWN")
    assert not report.passed and not report.results[-1].passed


def test_inbox_retains_only_valid_events_and_scopes_ids_to_the_source(inbox):
    cdr = conformance.sample_cdr_event()
    assert _deliver(inbox, dict(cdr, specversion="0.1")) == 422
    assert _deliver(inbox, cdr) == 204
    invoice = dict(conformance.sample_invoice_event(), id=cdr["id"], subject="invoice/" + cdr["id"])
    assert _deliver(inbox, invoice) == 204
    assert len(inbox.events) == 2


class _Emsp(BaseHTTPRequestHandler):
    """A minimal eMSP backend that keeps the contract."""

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        if self.headers.get("authorization") != "Bearer secret-token-1234":
            self.send_response(401)
            self.end_headers()
            return
        uid = body["uid"]
        answer = {"status": "Invalid", "reason": "no such token"}
        if uid == "KNOWN":
            answer = {
                "status": "Accepted",
                "reason": "ok",
                "token": {
                    "issuer": "DE*EMT",
                    "uid": uid,
                    "token_type": "Rfid",
                    "contract_id": "DE-EMT-C1",
                },
            }
        if uid == "LOST":
            answer = {"status": "Blocked", "reason": "reported lost"}
        data = json.dumps(answer).encode()
        self.send_response(200)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


def test_the_authorize_suite_holds_a_backend_to_the_contract():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Emsp)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f"http://127.0.0.1:{server.server_address[1]}"
        report = conformance.authorize(url, "secret-token-1234", known="KNOWN", blocked="LOST")
        report.assert_passed()
        bad = conformance.authorize(url, "another-token")
        assert not bad.passed, "a backend that refuses csmsd's token does not conform"
    finally:
        server.shutdown()
        server.server_close()
