"""Events: the schemas, Standard Webhooks, the inbox, and the two conformance
suites — the receiver suite run against emobtest's own inbox."""

import json
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import emobtest as et
from emobtest import conformance, webhooks


def test_the_sample_event_is_what_the_schema_says_and_a_broken_one_is_named():
    event = conformance.sample_cdr_event()
    et.assert_event(event, type="de.emob.cdr.issued")
    broken = dict(event, data={"station": 1})
    with pytest.raises(AssertionError, match="schema"):
        et.assert_event(broken)
    assert "de.emob.invoice.issued" in et.EVENT_TYPES


def test_a_signature_holds_only_for_its_secret_body_and_time():
    secret, body = webhooks.new_secret(), b'{"a":1}'
    headers = webhooks.sign(secret, body)
    webhooks.verify(secret, headers, body)
    with pytest.raises(ValueError, match="does not match"):
        webhooks.verify(secret, headers, b'{"a":2}')
    with pytest.raises(ValueError, match="off"):
        webhooks.verify(secret, headers, body, now=int(headers["webhook-timestamp"]) + 3600)


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


def test_emobtests_own_inbox_conforms_as_a_receiver(inbox):
    report = conformance.receiver(inbox.url, inbox.secret)
    report.assert_passed()
    assert "5/5 held" in str(report)


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
                "token": {"uid": uid, "contract_id": "DE-EMT-C1"},
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
