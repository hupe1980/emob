"""Run the external client against a private instance of the example estate."""

from __future__ import annotations

import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import ExitStack
from datetime import UTC, datetime, timedelta
from pathlib import Path

from emobtest import Inbox, recover_event

ROOT = Path(__file__).resolve().parents[1]
PORTS = {
    "csmsd": 9001,
    "agentd": 9580,
    "tarifd": 9581,
    "poid": 9582,
    "roamd": 9583,
    "billd": 9584,
    "empd": 9585,
    "sited": 9586,
    "deskd": 9587,
    "portald": 9589,
    "accountingd": 9591,
    "ocpp": 9000,
}


def start(name, work, env, business_time=None):
    binary = ROOT / "target/debug"
    command = [binary / name]
    if business_time is not None:
        command = [binary / "examples/retail_fixture_clock", name, business_time.isoformat()]
    with (work / f"{name}.log").open("a") as output:
        return subprocess.Popen(command, env=env, stdout=output, stderr=output)


def stop(child):
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def wait_ready(children, hosts):
    pending = set(children)
    deadline = time.monotonic() + 40
    while pending:
        for name, child in children.items():
            if child.poll() is not None:
                raise RuntimeError(f"{name} exited during startup ({child.returncode})")
            if name not in pending:
                continue
            try:
                with urllib.request.urlopen(f"http://{hosts[name]}/health/ready", timeout=1):
                    pending.remove(name)
            except (OSError, urllib.error.URLError):
                pass
        if pending and time.monotonic() >= deadline:
            raise RuntimeError(f"Timed out waiting for {', '.join(sorted(pending))}")
        if pending:
            time.sleep(0.1)


def run() -> int:
    with tempfile.TemporaryDirectory() as scratch, ExitStack() as sockets:
        work = Path(scratch)
        journals = work / "journals"
        journals.mkdir()
        hosts = {}
        reserved = {}
        for name in PORTS:
            sock = sockets.enter_context(socket.socket())
            sock.bind(("127.0.0.1", 0))
            hosts[name] = f"127.0.0.1:{sock.getsockname()[1]}"
            reserved[name] = sock

        template = (ROOT / "deploy/estate.example.toml").read_text()
        addresses = {str(port): hosts[name] for name, port in PORTS.items()}
        config = re.sub(r"127\.0\.0\.1:(\d+)", lambda m: addresses[m[1]], template)
        estate = work / "estate.toml"
        # Exercise a nonzero recurring charge without changing the walkthrough's tariff.
        config = config.replace('monthly = "0"', 'monthly = "5.95"')
        config += '\n[[card_stock]]\ncode = "STOCK-PRIVATE"\nuid = "04AB0000000007"\n'
        config += """
[[operators]]
name = "privacy officer"
token_env = "EMOB_PRIVACY_TOKEN"
privacy = true

[[operators]]
name = "finance clerk"
token_env = "EMOB_FINANCE_TOKEN"
finance = true

[[operators]]
name = "case finance reviewer"
token_env = "EMOB_CASE_FINANCE_TOKEN"
finance = true
write = true

[[operators]]
name = "second finance reviewer"
token_env = "EMOB_SECOND_FINANCE_TOKEN"
finance = true
write = true
"""
        customers = {}
        for subject in ("mika", "nico", "driver-one", "driver-two", "finance"):
            key = "PORTAL_TEST_" + subject.upper().replace("-", "_")
            customers[key] = secrets.token_hex(32)
            config += f'\n[[portal_users]]\nsubject = "{subject}"\nsecret_env = "{key}"\n'
        inbox = sockets.enter_context(Inbox())
        cdr_type = "de.emob.cdr.issued"
        subscriber = "recovery-probe"
        config += f'''
[[subscribers]]
name = "{subscriber}"
url = "{inbox.url}"
secret_env = "EMOB_TEST_SUBSCRIBER_SECRET"
types = ["{cdr_type}"]
'''
        estate.write_text(config.replace("../target/dev-tls/", str(ROOT / "target/dev-tls") + "/"))
        env = dict(
            os.environ,
            **customers,
            EMOB_ESTATE=str(estate),
            EMOB_TEST_SUBSCRIBER_SECRET=inbox.secret,
            EMOB_TEST_AGREEMENT=str(work / "agreement-reference"),
            EMOB_TEST_INVOICE=str(work / "retail-result.json"),
            EMOB_JOURNAL_DIR=str(journals),
            EMPD_CUSTODY_DIR=str(work / "custody"),
            EMOB_PRIVACY_TOKEN=secrets.token_hex(32),
            SITED_FAILSAFE_KW="11",
            EMOB_CSMSD=f"http://{hosts['csmsd']}",
            EMOB_FINANCE_TOKEN=secrets.token_hex(32),
            EMOB_CASE_FINANCE_TOKEN=secrets.token_hex(32),
            EMOB_SECOND_FINANCE_TOKEN=secrets.token_hex(32),
            EMOB_ACCOUNTINGD=f"http://{hosts['accountingd']}",
            EMOB_PORTALD=f"http://{hosts['portald']}",
            EMOB_EMPD=f"http://{hosts['empd']}",
            EMOB_BILLD=f"http://{hosts['billd']}",
            EMOB_ROAMD=f"http://{hosts['roamd']}",
            EMOB_TARIFD=f"http://{hosts['tarifd']}",
            EMOB_DESKD=f"http://{hosts['deskd']}",
            CSMSD_BIND=hosts["ocpp"],
        )
        children: dict[str, subprocess.Popen] = {}
        month_start = datetime.now(UTC).replace(day=1, hour=0, minute=0, second=0, microsecond=0)
        # Use the previous complete month even when the suite runs across midnight on the first.
        month_start = (month_start - timedelta(days=1)).replace(day=1)
        env["EMOB_TEST_USAGE_START"] = month_start.replace(day=15, hour=10).isoformat()
        enrolled_at = month_start - timedelta(days=10)
        closed_at = (month_start + timedelta(days=32)).replace(day=1) + timedelta(days=1)
        try:
            for name in PORTS:
                if name == "ocpp":
                    continue
                reserved[name].close()
                if name == "csmsd":
                    reserved["ocpp"].close()
                env[f"{name.upper()}_HTTP_BIND"] = hosts[name]
                children[name] = start(name, work, env, enrolled_at if name == "empd" else None)
            wait_ready(children, hosts)
            suite = "tests/test_against_emob.py"
            replay = [
                suite + "::test_named_card_actions_survive_provider_restart",
                suite + "::test_native_agreement_survives_restart_and_offer_withdrawal",
            ]

            closing = suite + "::test_close_the_fleet_billing_month"
            reopening = suite + "::test_reopen_the_issued_fleet_invoice"

            lifecycle = suite + "::test_replace_and_end_one_fleet_agreement"
            retired = suite + "::test_retired_fleet_credentials_stay_retired"

            def restart(name, business_time=None):
                children[name].terminate()
                stop(children[name])
                children[name] = start(name, work, env, business_time)
                wait_ready({name: children[name]}, hosts)

            def run_tests(phase, *targets):
                return subprocess.call(
                    [
                        sys.executable,
                        "-m",
                        "pytest",
                        "-q",
                        *targets,
                        "--emob-ocpp",
                        f"wss://{hosts['ocpp']}/ocpp",
                        "--emob-ca",
                        str(ROOT / "target/dev-tls/ca.pem"),
                    ],
                    cwd=ROOT / "emobtest",
                    env={**env, "EMOB_TEST_ESTATE": phase},
                )

            status = run_tests(
                "private",
                suite,
                *(
                    f"--deselect={test}"
                    for test in [*replay, closing, reopening, lifecycle, retired]
                ),
            )
            if status == 0:
                # The external inbox accepted the push from a real OCPP settlement.
                # Hold recovery to precisely those bytes on both sides of a cold start.
                original = inbox.expect(cdr_type, within=15)

                def verify_recovery():
                    recovered = recover_event(
                        env["EMOB_CSMSD"], subscriber, cdr_type, original.id, secret=inbox.secret
                    )
                    if recovered != original.raw:
                        raise RuntimeError(
                            "Recovered CDR differs from the signed original delivery"
                        )

                verify_recovery()
                restart("csmsd")
                verify_recovery()
                print(
                    "CDR recovery matched the signed subscriber delivery before and after restart."
                )
                restart("empd")
                status = run_tests("replayed", *replay)
            if status == 0:
                restart("billd", closed_at)
                status = run_tests("closed", closing)
            if status == 0:
                restart("billd", closed_at)
                status = run_tests("issued", reopening)
            if status == 0:
                # End the first fleet agreement on a month boundary. The other
                # agreement then has a complete next month for the offset exercise.
                restart("empd", closed_at.replace(day=1))
                status = run_tests("lifecycle", lifecycle)
            if status == 0:
                restart("empd", closed_at)
                status = run_tests("retired", retired)
            portal = "tests/test_customer_portal.py"
            if status == 0:
                status = run_tests("portal", portal + "::test_customer_access_and_revocation")
            if status == 0:
                children["billd"].terminate()
                stop(children["billd"])
                status = run_tests("offline", portal + "::test_unavailable_billing")
            if status == 0:
                restart("billd", closed_at)
                children["empd"].terminate()
                stop(children["empd"])
                status = run_tests("authority-offline", portal + "::test_unavailable_authority")
            if status == 0:
                restart("empd", closed_at)
                status = run_tests("restored", portal + "::test_grants_and_archive_after_replay")
            cash = "tests/test_receivables.py"
            adjustment = "tests/test_line_credit.py"
            if status == 0:
                status = run_tests(
                    "cash",
                    cash + "::test_finance_can_reconcile_the_issued_ubl",
                    adjustment + "::test_review_one_fleet_line_and_reconcile_its_credit",
                    cash + "::test_settle_the_remaining_fleet_debt",
                )
            if status == 0:
                children["billd"].terminate()
                stop(children["billd"])
                status = run_tests(
                    "cash-offline",
                    cash + "::test_unavailable_issuer_cannot_authorize_a_new_allocation",
                )
            if status == 0:
                restart("billd", closed_at)
                restart("accountingd")
                status = run_tests(
                    "cash-restored",
                    cash + "::test_allocations_and_unapplied_cash_survive_replay",
                    adjustment + "::test_line_credit_receipt_xml_and_balance_survive_replay",
                )
            withdrawal = "tests/test_withdrawal.py"
            if status == 0:
                use_at = closed_at.replace(day=15, hour=10)
                case_at = (use_at + timedelta(days=32)).replace(day=2)
                env["EMOB_TEST_WITHDRAWAL_USE"] = use_at.isoformat()
                restart("empd", case_at)
                restart("billd", case_at)
                status = run_tests(
                    "withdrawal",
                    withdrawal
                    + "::test_a_paid_driver_withdraws_and_finance_issues_one_reviewed_credit",
                    cash + "::test_apply_credit_to_a_second_fleet_invoice",
                )
            if status == 0:
                restart("empd", case_at)
                restart("billd", case_at)
                restart("accountingd")
                status = run_tests(
                    "withdrawal-restored",
                    withdrawal + "::test_standing_original_invoice_and_cash_reconcile_after_replay",
                    cash + "::test_offset_survives_billing_and_accounting_restart",
                )
            erasure = "tests/test_erasure.py"
            if status == 0:
                status = run_tests(
                    "erasure-cut",
                    erasure + "::test_capture_an_active_grant_and_partially_paid_invoice",
                )
            owners = ("empd", "billd", "accountingd")
            backup = work / "before-erasure"
            if status == 0:
                backup.mkdir()
                for name in owners:
                    children[name].terminate()
                    stop(children[name])
                for name in owners:
                    for suffix in (".ndjson", ".ndjson.head"):
                        shutil.copyfile(journals / (name + suffix), backup / (name + suffix))
                for name in owners:
                    restart(name, case_at if name != "accountingd" else None)
                status = run_tests(
                    "erasure", erasure + "::test_review_erase_and_reopen_the_durable_receipt"
                )
            if status == 0:
                for name in owners:
                    children[name].terminate()
                    stop(children[name])
                for file in backup.iterdir():
                    shutil.copyfile(file, journals / file.name)
                # A bound journal and its ordinary service credentials alone must fail cold start.
                absent = work / "missing-custody"
                provider_log = work / "empd.log"
                offset = provider_log.stat().st_size
                probe = start("empd", work, {**env, "EMPD_CUSTODY_DIR": str(absent)}, case_at)
                try:
                    if probe.wait(timeout=8) == 0:
                        raise RuntimeError("Provider accepted a cut without its current custody")
                    with provider_log.open("rb") as log:
                        log.seek(offset)
                        if b"current empd custody directory is missing" not in log.read():
                            raise RuntimeError("Provider failed before checking current custody")
                finally:
                    if probe.poll() is None:
                        probe.terminate()
                        stop(probe)
                for name in owners:
                    restart(name, case_at if name != "accountingd" else None)
                status = run_tests(
                    "erasure-restored",
                    erasure + "::test_an_old_operational_cut_cannot_reinstate_erased_links",
                )
        finally:
            for child in children.values():
                child.terminate()
            for child in children.values():
                stop(child)
        # Check flushed service logs for private identifiers and credentials used by the clients.
        for name in ("deskd", "empd", "csmsd", "billd", "portald", "accountingd"):
            log = (work / f"{name}.log").read_bytes()
            sensitive = [b"MOB-1001", b"MOB-1003", b"04AB0000000004", b"STOCK-1004"]
            sensitive.extend(value.encode() for value in customers.values())
            sensitive.append(inbox.secret.encode())
            sensitive.extend([env["EMOB_FINANCE_TOKEN"].encode(), b"EXTERNAL-FINANCE-0001"])
            sensitive.extend(
                [
                    env["EMOB_CASE_FINANCE_TOKEN"].encode(),
                    env["EMOB_SECOND_FINANCE_TOKEN"].encode(),
                    b"POSTROOM-036-431",
                    b"FILE-036-721",
                    b"PAY-036-451",
                ]
            )
            sensitive.extend([env["EMOB_PRIVACY_TOKEN"].encode(), b"CASH-037-CUT"])
            receipt = Path(env["EMOB_TEST_AGREEMENT"])
            session = receipt.with_suffix(".session")
            if session.exists():
                sensitive.append(json.loads(session.read_text())["value"].encode())
            if receipt.exists():
                sensitive.append(receipt.read_bytes())
            if any(label in log for label in sensitive):
                raise RuntimeError(f"{name} wrote a private lookup value to its log")
        return status


if __name__ == "__main__":
    sys.exit(run())
