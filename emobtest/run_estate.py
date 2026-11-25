"""Run the external client against a private instance of the example estate."""

from __future__ import annotations

import os
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import ExitStack
from pathlib import Path

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
    "ocpp": 9000,
}


def start(name, work, env):
    with (work / f"{name}.log").open("a") as output:
        return subprocess.Popen(
            [ROOT / "target/debug" / name], env=env, stdout=output, stderr=output
        )


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
        estate.write_text(config.replace("../target/dev-tls/", str(ROOT / "target/dev-tls") + "/"))
        env = dict(
            os.environ,
            EMOB_ESTATE=str(estate),
            EMOB_TEST_AGREEMENT=str(work / "agreement-reference"),
            EMOB_JOURNAL_DIR=scratch,
            SITED_FAILSAFE_KW="11",
            EMOB_CSMSD=f"http://{hosts['csmsd']}",
            EMOB_DESKD=f"http://{hosts['deskd']}",
            CSMSD_BIND=hosts["ocpp"],
        )
        children: dict[str, subprocess.Popen] = {}
        try:
            for name in PORTS:
                if name == "ocpp":
                    continue
                reserved[name].close()
                if name == "csmsd":
                    reserved["ocpp"].close()
                env[f"{name.upper()}_HTTP_BIND"] = hosts[name]
                children[name] = start(name, work, env)
            wait_ready(children, hosts)
            suite = "tests/test_against_emob.py"
            replay = [
                suite + "::test_named_card_actions_survive_provider_restart",
                suite + "::test_native_agreement_survives_restart_and_offer_withdrawal",
            ]

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

            status = run_tests("private", suite, *(f"--deselect={test}" for test in replay))
            if status == 0:
                children["empd"].terminate()
                stop(children["empd"])
                children["empd"] = start("empd", work, env)
                wait_ready({"empd": children["empd"]}, hosts)
                status = run_tests("replayed", *replay)
        finally:
            for child in children.values():
                child.terminate()
            for child in children.values():
                stop(child)
        # The example's printed number is submitted over HTTP during the
        # operator investigation. Audit the two services after flushing logs.
        for name in ("deskd", "empd", "csmsd"):
            log = (work / f"{name}.log").read_bytes()
            sensitive = [b"MOB-1001", b"MOB-1003", b"04AB0000000004", b"STOCK-1004"]
            receipt = Path(env["EMOB_TEST_AGREEMENT"])
            if receipt.exists():
                sensitive.append(receipt.read_bytes())
            if any(label in log for label in sensitive):
                raise RuntimeError(f"{name} wrote a private lookup value to its log")
        return status


if __name__ == "__main__":
    sys.exit(run())
