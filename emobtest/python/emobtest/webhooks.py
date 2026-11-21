"""Standard Webhooks, as emob signs every event it delivers.

`webhook-id`, `webhook-timestamp` and `webhook-signature` (`v1,` + Base64 of
HMAC-SHA256 over `{id}.{timestamp}.{body}`), under a secret spelled
`whsec_<Base64>`. A timestamp more than five minutes from now is refused, which
is what stops a captured delivery from being replayed.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
import time
import uuid

TOLERANCE_SECONDS = 5 * 60


def new_secret() -> str:
    """A fresh `whsec_…` secret, 32 random bytes."""
    return "whsec_" + base64.b64encode(secrets.token_bytes(32)).decode()


def _key(secret: str) -> bytes:
    if not secret.startswith("whsec_"):
        raise ValueError("a Standard Webhooks secret starts with whsec_")
    try:
        return base64.b64decode(secret.removeprefix("whsec_"), validate=True)
    except ValueError as error:
        raise ValueError(f"the secret is not Base64 after whsec_: {error}") from None


def signature(secret: str, msg_id: str, timestamp: int, body: bytes) -> str:
    """The `webhook-signature` value for one delivery."""
    signed = f"{msg_id}.{timestamp}.".encode() + body
    digest = hmac.new(_key(secret), signed, hashlib.sha256).digest()
    return "v1," + base64.b64encode(digest).decode()


def sign(
    secret: str, body: bytes, *, msg_id: str | None = None, timestamp: int | None = None
) -> dict[str, str]:
    """The three headers a signed delivery carries."""
    msg_id = msg_id or f"msg_{uuid.uuid4().hex}"
    timestamp = int(time.time()) if timestamp is None else timestamp
    return {
        "webhook-id": msg_id,
        "webhook-timestamp": str(timestamp),
        "webhook-signature": signature(secret, msg_id, timestamp, body),
        "content-type": "application/cloudevents+json",
    }


def verify(secret: str, headers: dict[str, str], body: bytes, *, now: int | None = None) -> None:
    """Raise `ValueError` naming why a delivery is not genuine; return when it is."""
    lower = {key.lower(): value for key, value in headers.items()}
    msg_id = lower.get("webhook-id")
    stamp = lower.get("webhook-timestamp")
    given = lower.get("webhook-signature")
    if not (msg_id and stamp and given):
        raise ValueError(
            "unsigned: webhook-id, webhook-timestamp and webhook-signature are required"
        )
    try:
        timestamp = int(stamp)
    except ValueError:
        raise ValueError(f"webhook-timestamp {stamp!r} is not seconds since the epoch") from None
    now = int(time.time()) if now is None else now
    if abs(now - timestamp) > TOLERANCE_SECONDS:
        raise ValueError(f"webhook-timestamp is {abs(now - timestamp)} s off: a replay, or a clock")
    expected = signature(secret, msg_id, timestamp, body)
    if not any(hmac.compare_digest(expected, part) for part in given.split()):
        raise ValueError("webhook-signature does not match: another secret, or an altered body")
