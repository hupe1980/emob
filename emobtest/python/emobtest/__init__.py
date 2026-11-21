"""emobtest — test and conformance toolkit for EV charging platforms.

Built on emob's own crates, so a test asserts with the code that bills: a meter
that signs OCMF, the receipt's verifier, the Eichrecht chain, the tariff
rating and the Preisstand, the event schemas, Standard Webhooks, the
Mobilithek profile, a charging station and a car that drive a CSMS over OCPP,
an eMSP that answers it — and two conformance suites for a service that replaces
one of emob's.

`AssertionError` means the system under test is wrong; `ValueError` means the
test is.
"""

from . import conformance, webhooks
from ._native import __version__
from .assertions import (
    assert_bills,
    assert_event,
    assert_feed,
    assert_not_billable,
    assert_price,
    assert_tampered,
    assert_verifies,
)
from .core import (
    EVENT_TYPES,
    Check,
    Event,
    Evidence,
    FeedReport,
    Meter,
    Reading,
    Tariff,
    Verdict,
    check_feed,
    emaid,
    event_problems,
    evidence,
    evse_id,
    utc,
    verify,
)
from .emsp import Emsp
from .inbox import Inbox
from .station import Car, Reservation, Session, Station

__all__ = [
    "EVENT_TYPES",
    "Car",
    "Check",
    "Emsp",
    "Event",
    "Evidence",
    "FeedReport",
    "Inbox",
    "Meter",
    "Reading",
    "Reservation",
    "Session",
    "Station",
    "Tariff",
    "Verdict",
    "__version__",
    "assert_bills",
    "assert_event",
    "assert_feed",
    "assert_not_billable",
    "assert_price",
    "assert_tampered",
    "assert_verifies",
    "check_feed",
    "conformance",
    "emaid",
    "event_problems",
    "evidence",
    "evse_id",
    "utc",
    "verify",
    "webhooks",
]
