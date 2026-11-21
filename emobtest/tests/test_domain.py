"""The domain, from emob's crates: a meter signs, the verifier and the
Eichrecht chain agree, the tariff rates as the price page does."""

from decimal import Decimal

import pytest

import emobtest as et

START = "2026-12-01T10:00:00+01:00"


def test_a_meter_signs_what_the_verifier_and_the_chain_accept(meter):
    records = meter.charge("30", start=START, minutes=12)
    assert len(records) >= 2, "a begin and an end at least"
    check = et.assert_verifies(records[-1], meter.public_key)
    assert check.readings[0].unit == "kWh" or check.readings[0].unit == "Wh"
    session = et.assert_bills(records, meter.public_key, kwh="30")
    assert session.kwh == Decimal("30")


def test_one_changed_digit_is_caught_by_both(meter):
    records = meter.charge("10", start=START, minutes=5)
    tampered = records[-1].replace('"RV":', '"RV":1', 1)
    et.assert_tampered(tampered, meter.public_key)
    refused = et.assert_not_billable([*records[:-1], tampered], meter.public_key)
    assert refused.reasons, "and it says why"
    with pytest.raises(AssertionError, match="may not be billed"):
        et.assert_bills([*records[:-1], tampered], meter.public_key)


def test_another_meters_key_does_not_verify():
    first, other = et.Meter(seed=1), et.Meter(seed=2)
    record = first.charge("5", start=START, minutes=3)[-1]
    assert et.verify(record, other.public_key).verdict is et.Verdict.NOT_VERIFIED


def test_one_seed_signs_the_same_bytes():
    a = et.Meter(seed=42).charge("7.5", start=START, minutes=10)
    b = et.Meter(seed=42).charge("7.5", start=START, minutes=10)
    assert a == b


def test_the_price_is_the_billing_functions_and_floats_are_refused():
    tariff = et.Tariff(energy="0.59", session_fee="0.50")
    assert tariff.total(kwh=30, power_kw=150, at=START) == Decimal("18.20")
    assert tariff.lines(START) == ["0.59 EUR / kWh", "0.50 EUR / session"]
    assert len(tariff.preisstand) == 4 and tariff.fingerprint.startswith(tariff.preisstand.lower())
    with pytest.raises(ValueError, match="float"):
        et.Tariff(energy=0.59).total(kwh=30)
    et.assert_price("18.20", tariff, kwh=30, power_kw=150, at=START)
    with pytest.raises(AssertionError, match="Preisstand"):
        et.assert_price("18.21", tariff, kwh=30, power_kw=150, at=START)


def test_a_new_version_has_a_new_preisstand():
    assert et.Tariff(energy="0.59").preisstand != et.Tariff(energy="0.60").preisstand


def test_identifiers_are_checked_not_guessed():
    assert et.evse_id("DE*ABC*E00001") == "DE*ABC*E00001"
    assert et.emaid("NL-TNM-000122045") == "NLTNM000122045U"
    with pytest.raises(ValueError):
        et.emaid("NL-TNM-000122045-X")
    with pytest.raises(ValueError):
        et.evse_id("not an evse")


def test_two_values_for_one_instant_do_not_bill_here_as_in_csmsd(meter):
    """Found by driving csmsd: the kit called billable what csmsd refused."""
    at = "2026-12-01T10:30:00+01:00"
    records = [
        meter.sign("100.000", at="2026-12-01T10:00:00+01:00", marker="B", page=1),
        meter.sign("104.622", at=at, marker="C", page=2),
        meter.sign("104.625", at=at, marker="E", page=3),
    ]
    found = et.assert_not_billable(records, meter.public_key)
    assert any("one value at that instant" in reason for reason in found.reasons), found.reasons
    agreeing = [*records[:2], meter.sign("104.622", at=at, marker="E", page=3)]
    et.assert_bills(agreeing, meter.public_key, kwh="4.622")
