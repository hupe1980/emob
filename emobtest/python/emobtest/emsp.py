"""An eMSP backend that answers a CSMS's authorisation — and misbehaves on cue.

It answers `POST /v1/authorize` the way `empd` does, with a bearer token, on
loopback. Each card gets a rule; a card without one is `Invalid`:

    with Emsp() as emsp:
        emsp.on("04A1B2C3D4E5F6").accepts(contract="DE-MSP-C00000001-W")
        emsp.on("04DEADBEEF0001").blocks("reported stolen")
        emsp.on("04510A0000000A").stalls(3)  # past the CSMS's budget
        emsp.on("04FA11ED000000").fails(503)
        # point the CSMS's eMSP at emsp.url with emsp.token, then charge
        assert emsp.asked == ["04A1B2C3D4E5F6"]

What a CSMS must do with a stall or a failure is the test: `csmsd` refuses the
driver, because "no answer" is never "probably fine".
"""

from __future__ import annotations

import json
import secrets
import threading
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


@dataclass
class _Rule:
    status: str = "Invalid"
    reason: str = "the token is not known"
    contract: str | None = None
    stall: float = 0.0
    http: int = 200


class Card:
    """The rule for one card — `Emsp.on(uid)`'s answer."""

    def __init__(self, emsp: Emsp, uid: str) -> None:
        self._emsp = emsp
        self._uid = uid

    def _set(self, **rule: object) -> Emsp:
        with self._emsp._lock:
            self._emsp._rules[self._uid] = _Rule(**rule)  # type: ignore[arg-type]
        return self._emsp

    def accepts(self, contract: str = "DE-EMT-C00000001-0") -> Emsp:
        return self._set(status="Accepted", reason="the contract is in force", contract=contract)

    def blocks(self, reason: str = "the token is blocked") -> Emsp:
        return self._set(status="Blocked", reason=reason)

    def expired(self, reason: str = "the contract has ended") -> Emsp:
        return self._set(status="Expired", reason=reason)

    def stalls(self, seconds: float) -> Emsp:
        """Answer `Accepted` — but only after `seconds`."""
        return self._set(
            status="Accepted",
            reason="the contract is in force",
            contract="DE-EMT-C00000001-0",
            stall=seconds,
        )

    def fails(self, http: int = 503) -> Emsp:
        """Answer with an HTTP error instead of a decision."""
        return self._set(http=http)


class Emsp:
    """An eMSP's authorisation endpoint on loopback."""

    def __init__(
        self,
        token: str | None = None,
        *,
        issuer: str = "DE*EMT",
        host: str = "127.0.0.1",
        port: int = 0,
    ) -> None:
        self.token = token or secrets.token_urlsafe(24)
        self.issuer = issuer
        self.asked: list[str] = []
        self._rules: dict[str, _Rule] = {}
        self._lock = threading.Lock()
        emsp = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                if self.path.rstrip("/") != "/v1/authorize":
                    self._answer(404, {})
                    return
                if self.headers.get("authorization") != f"Bearer {emsp.token}":
                    self._answer(401, {})
                    return
                try:
                    uid = str(
                        json.loads(self.rfile.read(int(self.headers["content-length"])))["uid"]
                    )
                except (KeyError, TypeError, ValueError):
                    self._answer(400, {})
                    return
                with emsp._lock:
                    emsp.asked.append(uid)
                    rule = emsp._rules.get(uid, _Rule())
                time.sleep(rule.stall)
                if rule.http != 200:
                    self._answer(rule.http, {})
                    return
                token = None
                if rule.status == "Accepted":
                    token = {
                        "issuer": emsp.issuer,
                        "uid": uid,
                        "token_type": "Rfid",
                        "contract_id": rule.contract,
                    }
                self._answer(
                    200,
                    {
                        "status": rule.status,
                        "reason": rule.reason,
                        "token": token,
                        "reference": None,
                    },
                )

            def _answer(self, status: int, body: dict) -> None:
                text = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(text)))
                self.end_headers()
                self.wfile.write(text)

            def log_message(self, *_: object) -> None:
                pass

        self._server = ThreadingHTTPServer((host, port), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def on(self, uid: str) -> Card:
        """The rule for `uid`: `.accepts()`, `.blocks()`, `.expired()`, `.stalls()`, `.fails()`."""
        return Card(self, uid)

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def __enter__(self) -> Emsp:
        self._thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
