"""Subscriber event recovery verifies the request and response independently."""

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from emobtest import recover_event, webhooks
from emobtest.cli import main
from emobtest.recovery import MAX_EVENT_BYTES, ROUTE


@pytest.fixture
def issuer():
    secret = webhooks.new_secret()
    event = {
        "specversion": "1.0",
        "id": "R-2026-001",
        "type": "de.emob.invoice.issued",
        "source": "urn:emob:billd",
        "subject": "invoice/R-2026-001",
        "datacontenttype": "application/xml",
        "data": "<Invoice>private</Invoice>",
    }
    state = {
        "body": json.dumps(event, indent=2).encode(),
        "event": event,
        "requests": [],
        "secret": secret,
    }

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            raw = self.rfile.read(int(self.headers["content-length"]))
            state["requests"].append((self.path, dict(self.headers.items()), raw))
            body = state["body"]
            headers = webhooks.sign(
                state.get("signer", secret),
                body,
                msg_id=state.get("id", event["id"]),
                timestamp=state.get("timestamp"),
            )
            headers.update(state.get("headers", {}))
            self.send_response(state.get("status", 200))
            self.send_header("Content-Length", str(state.get("size", len(body))))
            for key, value in headers.items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    state["url"] = f"http://127.0.0.1:{server.server_port}"
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01})
    thread.start()
    try:
        yield state
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_recovery_returns_only_the_signed_original_bytes(issuer, capsysbinary, monkeypatch):
    e = issuer["event"]
    monkeypatch.setenv("TEST_SUBSCRIBER_KEY", issuer["secret"])
    args = [
        "recover",
        issuer["url"],
        "--subscriber",
        "erp",
        "--event-type",
        e["type"],
        "--id",
        e["id"],
        "--secret-env",
        "TEST_SUBSCRIBER_KEY",
    ]
    assert main(args) == 0
    output = capsysbinary.readouterr()
    assert output.out == issuer["body"] and not output.err
    assert (
        recover_event(issuer["url"] + ROUTE, "erp", e["type"], e["id"], secret=issuer["secret"])
        == issuer["body"]
    )
    nonces = set()
    for path, headers, raw in issuer["requests"]:
        assert path == ROUTE
        headers = {key.lower(): value for key, value in headers.items()}
        assert headers["content-type"] == "application/json"
        webhooks.verify(issuer["secret"], headers, raw)
        assert json.loads(raw) == {
            "purpose": "recover",
            "subscriber": "erp",
            "type": e["type"],
            "id": e["id"],
        }
        nonces.add(headers["webhook-id"])
    assert len(nonces) == 2 and e["id"] not in nonces


@pytest.mark.parametrize(
    "invalid",
    [
        "signer",
        "stale",
        "id",
        "envelope-id",
        "source",
        "type",
        "schema",
        "redirect",
        "oversized",
        "truncated",
        "unsigned",
        "non-ascii-signature",
        "duplicate-field",
    ],
)
def test_recovery_failure_writes_neither_event_nor_secret(
    issuer, invalid, capsysbinary, monkeypatch
):
    e = issuer["event"]
    if invalid == "signer":
        issuer["signer"] = webhooks.new_secret()
    elif invalid == "stale":
        issuer["timestamp"] = int(time.time()) - 500
    elif invalid == "id":
        issuer["id"] = "different"
    elif invalid in {"envelope-id", "source", "type", "schema"}:
        field = {"envelope-id": "id", "source": "source", "type": "type", "schema": "data"}[invalid]
        issuer["body"] = json.dumps({**e, field: 42 if invalid == "schema" else "other"}).encode()
    elif invalid == "redirect":
        issuer.update(status=307, headers={"Location": issuer["url"] + "/elsewhere"})
    elif invalid == "duplicate-field":
        issuer["body"] = issuer["body"].replace(b'"id":', b'"id":"other","id":')
    elif invalid == "non-ascii-signature":
        issuer["headers"] = {"webhook-signature": "v1,é"}
    elif invalid == "unsigned":
        issuer["headers"] = {"webhook-signature": ""}
    else:
        issuer["size"] = MAX_EVENT_BYTES + 1 if invalid == "oversized" else len(issuer["body"]) + 5
    monkeypatch.setenv("TEST_SUBSCRIBER_KEY", issuer["secret"])
    assert (
        main(
            [
                "recover",
                issuer["url"],
                "--subscriber",
                "erp",
                "--id",
                e["id"],
                "--event-type",
                e["type"],
                "--secret-env",
                "TEST_SUBSCRIBER_KEY",
            ]
        )
        == 1
    )
    output = capsysbinary.readouterr()
    assert not output.out and b"recovery failed" in output.err
    assert b"private" not in output.err and issuer["secret"].encode() not in output.err
    assert len(issuer["requests"]) == 1


@pytest.mark.parametrize(
    "url",
    [
        "http://example.org",
        "ftp://127.0.0.1",
        "https://user:pass@example.org",
        "https://example.org?q=1",
    ],
)
def test_recovery_only_posts_directly_to_the_configured_issuer(url):
    with pytest.raises(ValueError, match="issuer"):
        recover_event(url, "erp", "de.emob.invoice.issued", "R-1", secret=webhooks.new_secret())
