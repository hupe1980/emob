"""Recover one original CloudEvent from its issuer and authenticate its exact bytes."""

from __future__ import annotations

import http.client
import ipaddress
import json
from urllib.parse import urlsplit

from . import webhooks
from .core import EVENT_TYPES, event_problems

ROUTE = "/v1/subscriber-events/recover"
MAX_EVENT_BYTES = 8 * 1024 * 1024


def _target(url: str) -> tuple[http.client.HTTPConnection, str]:
    """Contact exactly this issuer, with TLS away from loopback."""
    parts = urlsplit(url)
    if (
        not parts.hostname
        or parts.username is not None
        or parts.password is not None
        or parts.query
        or parts.fragment
        or any(ord(c) <= 32 or ord(c) == 127 for c in url)
    ):
        raise ValueError("supply an issuer URL without credentials, query or fragment")
    host = parts.hostname
    path = parts.path.rstrip("/")
    if not path.endswith(ROUTE):
        path += ROUTE
    if parts.scheme == "https":
        return http.client.HTTPSConnection(host, parts.port, timeout=10), path
    if parts.scheme == "http":
        if host == "localhost":
            host = "127.0.0.1"
        try:
            loopback = ipaddress.ip_address(host).is_loopback
        except ValueError:
            loopback = False
        if loopback:
            return http.client.HTTPConnection(host, parts.port, timeout=10), path
    raise ValueError("use HTTPS for the issuer or a loopback address for local HTTP")


def _unique_fields(pairs: list[tuple]) -> dict:
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError("event contains a repeated JSON field")
    return fields


def recover_event(
    url: str, subscriber: str, event_type: str, event_id: str, *, secret: str
) -> bytes:
    """Return the original signed envelope after checking its identity, freshness and schema."""
    if not subscriber or not event_id or event_type not in EVENT_TYPES:
        raise ValueError("supply the subscriber name, a supported event type and its original ID")
    proof = json.dumps(
        {"purpose": "recover", "subscriber": subscriber, "type": event_type, "id": event_id},
        separators=(",", ":"),
    ).encode()
    if len(proof) > 4096:
        raise ValueError("the recovery request exceeds 4096 bytes")
    headers = webhooks.sign(secret, proof)
    headers["content-type"] = "application/json"
    headers["accept"] = "application/cloudevents+json"
    connection, path = _target(url)
    try:
        connection.request("POST", path, proof, headers)
        with connection.getresponse() as response:
            if response.status != 200:
                raise ValueError(f"issuer returned HTTP {response.status}")
            if response.headers.get_content_type() != "application/cloudevents+json":
                raise ValueError("issuer did not return a structured CloudEvent")
            lengths = response.headers.get_all("Content-Length", [])
            if lengths and (
                len(lengths) != 1
                or not lengths[0].isascii()
                or not lengths[0].isdigit()
                or int(lengths[0]) > MAX_EVENT_BYTES
                or response.headers.get("Transfer-Encoding")
            ):
                raise ValueError("issuer returned an invalid or excessive event length")
            body = response.read(MAX_EVENT_BYTES + 1)
            if len(body) > MAX_EVENT_BYTES or (lengths and len(body) != int(lengths[0])):
                raise ValueError("issuer returned a truncated or oversized event")
            signed = {}
            for name in ("webhook-id", "webhook-timestamp", "webhook-signature"):
                values = response.headers.get_all(name, [])
                if len(values) != 1:
                    raise ValueError(f"issuer must return exactly one {name} header")
                signed[name] = values[0]
    except (OSError, http.client.HTTPException) as error:
        raise ValueError("could not read the complete event from its issuer") from error
    finally:
        connection.close()
    try:
        webhooks.verify(secret, signed, body)
    except ValueError as error:
        raise ValueError("issuer returned an invalid or stale event signature") from error
    if signed["webhook-id"] != event_id:
        raise ValueError("signed delivery ID differs from the requested event")
    try:
        event = json.loads(body, object_pairs_hook=_unique_fields)
        if (
            not isinstance(event, dict)
            or event.get("type") != event_type
            or event.get("id") != event_id
        ):
            raise ValueError("envelope identity differs from the requested event")
        # Each type's published schema fixes its issuing source.
        if event_problems(body):
            raise ValueError("event does not match its issuer's published schema")
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise ValueError("issuer returned an unreadable event") from error
    return body
