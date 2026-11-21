"""Conformance: a service that replaces one of emob's shows it keeps the contract.

Two suites, run against a base URL:

- `receiver(url, secret)` — an ERP, a billing system or any `[[subscribers]]`
  entry taking emob's events in place of `billd`;
- `authorize(url, token)` — an eMSP backend answering `csmsd` in place of
  `empd`.

Every check is a line: `✓` held, `✗` broken with what was seen and why it
matters. The reference daemons pass both.
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.request
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime

from . import webhooks
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


def _post(url: str, body: bytes, headers: dict[str, str], timeout: float = 10.0):
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as answer:
            return answer.status, answer.read(), time.monotonic() - started
    except urllib.error.HTTPError as answer:
        return answer.code, answer.read(), time.monotonic() - started


# ── receiver ──────────────────────────────────────────────────────────────────


def sample_cdr_event(meter: Meter | None = None) -> dict:
    """A `de.emob.cdr.issued` CloudEvent as `csmsd` sends one, marked `emobtest`
    so a receiver never books it — its records really signed by a meter."""
    meter = meter or Meter(seed=1)
    records = meter.charge("22.5", start="2026-12-01T10:00:00+01:00", minutes=45)
    key = f"cdr/DE*EMT/TEST-{uuid.uuid4().hex[:8]}"
    return {
        "specversion": "1.0",
        "id": f"test-{uuid.uuid4()}",
        "source": "urn:emob:csmsd",
        "type": "de.emob.cdr.issued",
        "subject": key,
        "time": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "datacontenttype": "application/json",
        "emobtest": True,
        "data": {
            "cdr": {"key": key.removeprefix("cdr/"), "energy": "22.5"},
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


def receiver(url: str, secret: str, *, budget: float = 10.0) -> Report:
    """Hold a receiver of emob's events to Standard Webhooks and idempotence."""
    report = Report("receiver", url)
    event = sample_cdr_event()
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
        ok = 200 <= status < 300
        report.check(
            name,
            ok == want_2xx,
            f"{status} in {took * 1000:.0f} ms" + ("" if ok == want_2xx else f" — {why}"),
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
    stale = webhooks.sign(secret, body, msg_id=f"stale-{uuid.uuid4()}", timestamp=stale_at)
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
            return json.loads(body)
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
            status, body, _ = ask(uid, token)
            answer = answer_of(body)
            good = status == 200 and answer.get("status") == want
            if want == "Accepted":
                good = good and bool((answer.get("token") or {}).get("contract_id"))
            else:
                good = good and bool(answer.get("reason"))
            report.check(name, good, f"{status} {answer.get('status')!r}")
    except OSError as error:
        report.check("the backend answers", False, str(error))
    return report


SUITES: dict[str, Callable[..., Report]] = {"receiver": receiver, "authorize": authorize}
