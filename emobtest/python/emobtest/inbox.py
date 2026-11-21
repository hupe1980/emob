"""An inbox: the system on the other end of a `[[subscribers]]` entry.

It listens on loopback, checks every delivery's Standard Webhooks signature and
schema, answers the way a well-behaved receiver does — `204` taken, `200` for a
duplicate id, `401` for a delivery it cannot trust — and keeps what arrived.

    with Inbox() as inbox:
        # point the platform's subscriber at inbox.url with inbox.secret
        event = inbox.expect("de.emob.cdr.issued", within=10)
"""

from __future__ import annotations

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import webhooks
from .core import Event, event_problems

MAX_BODY = 1 << 20


class Inbox:
    """A signed-event receiver on loopback."""

    def __init__(
        self, secret: str | None = None, *, host: str = "127.0.0.1", port: int = 0
    ) -> None:
        self.secret = secret or webhooks.new_secret()
        self.events: list[Event] = []
        self.refused: list[str] = []
        self._seen: set[str] = set()
        self._arrived = threading.Condition()
        inbox = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("content-length") or 0)
                if length > MAX_BODY:
                    self._answer(413, "too large")
                    return
                body = self.rfile.read(length)
                headers = {key: value for key, value in self.headers.items()}
                try:
                    webhooks.verify(inbox.secret, headers, body)
                except ValueError as why:
                    with inbox._arrived:
                        inbox.refused.append(str(why))
                    self._answer(401, str(why))
                    return
                msg_id = headers.get("webhook-id", "")
                with inbox._arrived:
                    if msg_id in inbox._seen:
                        self._answer(200, '{"duplicate":true}')
                        return
                    inbox._seen.add(msg_id)
                try:
                    parsed = json.loads(body)
                except ValueError:
                    parsed = {}
                event = Event(
                    id=str(parsed.get("id", msg_id)),
                    type=str(parsed.get("type", "")),
                    subject=str(parsed.get("subject", "")),
                    data=parsed.get("data") or {},
                    raw=body,
                    headers=headers,
                    problems=tuple(event_problems(body)) if parsed else ("not JSON",),
                )
                with inbox._arrived:
                    inbox.events.append(event)
                    inbox._arrived.notify_all()
                self._answer(204, "")

            def _answer(self, status: int, text: str) -> None:
                self.send_response(status)
                self.send_header("content-length", str(len(text.encode())))
                self.end_headers()
                self.wfile.write(text.encode())

            def log_message(self, *_: object) -> None:  # quiet by default
                pass

        self._server = ThreadingHTTPServer((host, port), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}/"

    def __enter__(self) -> Inbox:
        self._thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()

    def expect(self, type: str, *, within: float = 10.0, subject: str | None = None) -> Event:
        """The first event of `type` (and `subject`), waiting up to `within` seconds.

        Fails with what did arrive, because "nothing came" and "the wrong thing
        came" are different bugs.
        """
        deadline = time.monotonic() + within
        with self._arrived:
            while True:
                for event in self.events:
                    if event.type == type and (subject is None or event.subject == subject):
                        if event.problems:
                            raise AssertionError(
                                f"a {type} arrived that does not match its schema:\n  "
                                + "\n  ".join(event.problems)
                            )
                        return event
                left = deadline - time.monotonic()
                if left <= 0:
                    got = ", ".join(e.type for e in self.events) or "nothing"
                    refused = f"; refused: {'; '.join(self.refused)}" if self.refused else ""
                    raise AssertionError(f"no {type} within {within} s — received {got}{refused}")
                self._arrived.wait(left)
