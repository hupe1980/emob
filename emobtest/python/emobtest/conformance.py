"""Conformance: a service that replaces one of emob's shows it keeps the contract.

HTTP suites, run against the selected service:

- `receiver(url, secret)` — an ERP, a billing system or any `[[subscribers]]`
  entry taking emob's events in place of `billd`;
- `authorize(url, token)` — an eMSP backend answering `csmsd` in place of
  `empd`;
- `tariff(url, tariff_id)` — current and versioned public seller documents.

Every check is a line: `✓` held, `✗` broken with what was seen and why it
matters. HTTP probes use the configured endpoint directly.
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime

from . import _native, webhooks
from .core import Meter, event_problems

#: How long `csmsd` waits for an authorisation by default
#: (`[services] authorize_timeout_ms`): past it, the driver is refused.
AUTHORIZE_BUDGET_SECONDS = 2.0


@dataclass(frozen=True)
class Result:
    name: str
    passed: bool
    detail: str = ""

    def line(self) -> str:
        mark = "✓" if self.passed else "✗"
        return f"  {mark} {self.name}" + (f" — {self.detail}" if self.detail else "")


@dataclass
class Report:
    suite: str
    target: str
    results: list[Result] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return bool(self.results) and all(r.passed for r in self.results)

    def check(self, name: str, passed: bool, detail: str = "") -> None:
        self.results.append(Result(name, passed, detail))

    def __str__(self) -> str:
        held = sum(r.passed for r in self.results)
        head = f"{self.suite} · {self.target}"
        lines = [head, *(r.line() for r in self.results)]
        verdict = "conforms" if self.passed else "does not conform"
        lines.append(f"{'✓' if self.passed else '✗'} {held}/{len(self.results)} held — {verdict}")
        return "\n".join(lines)

    def assert_passed(self) -> None:
        if not self.passed:
            raise AssertionError(str(self))


class _DirectEndpoint(urllib.request.HTTPRedirectHandler):
    """A probe holds the configured endpoint to its contract and credentials."""

    def redirect_request(self, *_args):
        return None


def _post(url: str, body: bytes, headers: dict[str, str], timeout: float = 10.0):
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    return _fetch(request, timeout)


def _fetch(request: urllib.request.Request, timeout: float):
    started = time.monotonic()
    try:
        with urllib.request.build_opener(_DirectEndpoint).open(request, timeout=timeout) as answer:
            data = answer.read((1 << 20) + 1)
            if len(data) > 1 << 20:
                raise OSError("response exceeds 1 MiB")
            return answer.status, data, time.monotonic() - started
    except urllib.error.HTTPError as answer:
        with answer:
            return answer.code, b"", time.monotonic() - started


# ── receiver ──────────────────────────────────────────────────────────────────


def sample_cdr_event(meter: Meter | None = None) -> dict:
    """A `de.emob.cdr.issued` CloudEvent as `csmsd` sends one, marked `emobtest`
    so a receiver never books it — its records really signed by a meter."""
    meter = meter or Meter(seed=1)
    records = meter.charge("22.5", start="2026-12-01T10:00:00+01:00", minutes=45)
    key = f"test-{uuid.uuid4()}"
    return {
        "specversion": "1.0",
        "id": key,
        "source": "urn:emob:csmsd",
        "type": "de.emob.cdr.issued",
        "subject": f"cdr/{key}",
        "time": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "datacontenttype": "application/json",
        "emobtest": True,
        "data": {
            "cdr": {"key": key, "energy": "22.5"},
            "token": {"uid": "EMOBTEST", "contract_id": "DE-EMT-C00000001-0"},
            "signed": records,
            "station": "EMOBTEST-1",
            "point": {
                "evse": meter.evse,
                "connector": "iec62196_t2_combo",
                "rated_power_kw": "150",
                "location": {"id": "EMOBTEST", "name": "emobtest", "time_zone": "Europe/Berlin"},
            },
        },
    }


def sample_invoice_event() -> dict:
    """Signed probe payload for an invoice receiver. Never submit it for payment."""
    key = f"test-{uuid.uuid4()}"
    return {
        "specversion": "1.0",
        "id": key,
        "source": "urn:emob:billd",
        "type": "de.emob.invoice.issued",
        "subject": f"invoice/{key}",
        "datacontenttype": "application/xml",
        "emobtest": True,
        "data": "<Invoice><!-- emobtest sample --></Invoice>",
    }


def receiver(
    url: str, secret: str, *, budget: float = 10.0, event_type: str = "de.emob.cdr.issued"
) -> Report:
    """Hold a receiver of emob's events to Standard Webhooks and idempotence."""
    report = Report("receiver", url)
    if event_type == "de.emob.cdr.issued":
        event = sample_cdr_event()
    elif event_type == "de.emob.invoice.issued":
        event = sample_invoice_event()
    else:
        raise ValueError(f"unknown receiver event type: {event_type}")
    problems = event_problems(event)
    if problems:  # the kit's own sample is a test bug, not the receiver's
        raise ValueError("emobtest's sample does not match the schema: " + "; ".join(problems))
    body = json.dumps(event).encode()
    signed = webhooks.sign(secret, body, msg_id=event["id"])

    def attempt(name: str, headers: dict[str, str], want_2xx: bool, why: str) -> None:
        try:
            status, _, took = _post(url, body, headers, budget)
        except OSError as error:
            report.check(name, False, f"no answer: {error}")
            return
        ok = 200 <= status < 300 if want_2xx else status == 401
        report.check(
            name,
            ok,
            f"{status} in {took * 1000:.0f} ms" + ("" if ok else f" — {why}"),
        )

    attempt(
        "a signed delivery is taken", signed, True, "the platform will deliver it again until it is"
    )
    attempt(
        "the same delivery again is taken, not an error",
        signed,
        True,
        "redelivery under one id is how at-least-once works; it must not fail or double-book",
    )
    forged = dict(
        signed,
        **{
            "webhook-signature": webhooks.signature(
                webhooks.new_secret(), event["id"], int(signed["webhook-timestamp"]), body
            )
        },
    )
    attempt(
        "a delivery signed with another secret is refused",
        forged,
        False,
        "anyone could post a record that bills",
    )
    stale_at = int(time.time()) - 15 * 60
    stale = webhooks.sign(secret, body, msg_id=event["id"], timestamp=stale_at)
    attempt(
        "a delivery fifteen minutes old is refused",
        stale,
        False,
        "a captured delivery could be replayed",
    )
    unsigned = {"content-type": "application/cloudevents+json"}
    attempt(
        "an unsigned delivery is refused", unsigned, False, "anyone could post a record that bills"
    )
    return report


# ── authorize ─────────────────────────────────────────────────────────────────

STATUSES = {"Accepted", "Blocked", "Expired", "Invalid"}


def authorize(
    url: str,
    token: str,
    *,
    known: str | None = None,
    blocked: str | None = None,
) -> Report:
    """Hold an eMSP backend to the answer `csmsd` asks for at a charge point.

    `known` and `blocked` are uids the backend holds as accepted and blocked;
    without them those two checks are skipped, not passed.
    """
    endpoint = url.rstrip("/")
    if not endpoint.endswith("/v1/authorize"):
        endpoint += "/v1/authorize"
    report = Report("authorize", endpoint)

    def ask(uid: str, bearer: str | None):
        headers = {"content-type": "application/json"}
        if bearer is not None:
            headers["authorization"] = f"Bearer {bearer}"
        return _post(endpoint, json.dumps({"uid": uid}).encode(), headers, 10.0)

    def answer_of(body: bytes) -> dict:
        try:
            text = body.decode("utf-8")
            return json.loads(text) if _native.valid_authorize_answer(text) else {}
        except ValueError:
            return {}

    try:
        status, _, _ = ask("EMOBTEST00", None)
        report.check("no token is 401", status == 401, f"{status}")
        status, _, _ = ask("EMOBTEST00", token + "x")
        report.check("a wrong token is 401", status == 401, f"{status}")
        status, body, took = ask(f"EMOBTEST{uuid.uuid4().hex[:8].upper()}", token)
        answer = answer_of(body)
        report.check(
            "an unknown uid is answered Invalid, with a reason",
            status == 200 and answer.get("status") == "Invalid" and bool(answer.get("reason")),
            f"{status} {answer.get('status')!r}",
        )
        report.check(
            f"the answer comes within {AUTHORIZE_BUDGET_SECONDS:.0f} s",
            took <= AUTHORIZE_BUDGET_SECONDS,
            f"{took * 1000:.0f} ms"
            + ("" if took <= AUTHORIZE_BUDGET_SECONDS else " — past it, csmsd refuses the driver"),
        )
        for uid, want, name in [
            (known, "Accepted", "a known uid is Accepted, naming its contract"),
            (blocked, "Blocked", "a blocked uid is Blocked, with a reason"),
        ]:
            if uid is None:
                continue
            status, body, took = ask(uid, token)
            answer = answer_of(body)
            good = (
                status == 200 and took <= AUTHORIZE_BUDGET_SECONDS and answer.get("status") == want
            )
            if want == "Accepted":
                good = good and answer["token"]["uid"] == uid
            else:
                good = good and bool(answer.get("reason"))
            report.check(name, good, f"{status} {answer.get('status')!r} in {took * 1000:.0f} ms")
    except OSError as error:
        report.check("the backend answers", False, str(error))
    return report


def tariff(url: str, tariff_id: str) -> Report:
    """Check a seller's current and immutable version lookup against one tariff."""
    report = Report("tariff", url)
    base = url.rstrip("/") + "/v1/driver-tariffs/" + urllib.parse.quote(tariff_id, safe="")
    try:
        status, body, _ = _fetch(urllib.request.Request(base), 10.0)
        text = body.decode("utf-8")
        current = json.loads(text) if status == 200 and _native.valid_driver_tariff(text) else {}
        valid = current.get("id") == tariff_id
        report.check("current tariff matches driver-tariff.v1 and its id", valid, f"HTTP {status}")
        if valid:
            pinned = base + "/" + current["version"]
            status, body, _ = _fetch(urllib.request.Request(pinned), 10.0)
            report.check(
                "its version returns the same document",
                status == 200 and json.loads(body) == current,
                f"HTTP {status}",
            )
        missing = base + "/" + uuid.uuid4().hex + uuid.uuid4().hex
        status, _, _ = _fetch(urllib.request.Request(missing), 10.0)
        report.check("an unknown version is 404", status == 404, f"HTTP {status}")
    except (OSError, ValueError) as error:
        report.check("the seller receives a readable tariff document", False, str(error))
    return report


SUITES: dict[str, Callable[..., Report]] = {
    "receiver": receiver,
    "authorize": authorize,
    "tariff": tariff,
}
