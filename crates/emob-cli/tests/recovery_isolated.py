"""Qualify a prebuilt local recovery image against a private synthetic cut."""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

LAUNCHER = Path(__file__).resolve().parents[3] / "deploy" / "recovery-drill.py"


def snapshot(inputs):
    return {
        (directory.name, path.name): hashlib.sha256(path.read_bytes()).hexdigest()
        for directory in inputs
        for path in directory.iterdir()
    }


def business_result(report):
    return {
        key: value
        for key, value in report.items()
        if key not in ("elapsed_ms", "attempt_id")
    }


def qualify(image, report_file, root):
    subprocess.run(
        [
            "docker",
            "run",
            "--pull=never",
            "--rm",
            "--network=none",
            "--read-only",
            "--user",
            f"{os.getuid()}:{os.getgid()}",
            "--mount",
            f"type=bind,src={root},dst=/fixture",
            image,
            "recovery-fixture",
            "/fixture/generated",
        ],
        check=True,
    )
    cold, evidence, keys = (
        root / "generated" / name for name in ("copy", "evidence", "keys")
    )
    initial = snapshot((cold, evidence, keys))

    def run(report, expected_code):
        code = subprocess.run(
            [
                sys.executable,
                str(LAUNCHER),
                "--image",
                image,
                "--copy",
                str(cold),
                "--evidence",
                str(evidence),
                "--keys",
                str(keys),
                "--report",
                str(report),
            ],
            check=False,
        ).returncode
        if not report.is_file():
            raise RuntimeError(
                f"Recovery launcher exited {code} without a report. "
                "Use a fresh report path under a real directory (pwd -P)."
            )
        result = json.loads(report.read_text())
        assert code == expected_code, (code, result.get("reasons"))
        return result

    first = run(report_file, 0)
    second = run(root / "second.json", 0)
    assert first["attempt_id"] != second["attempt_id"]
    assert business_result(first) == business_result(second)
    assert first["containment"] == "isolated" and first["retry"] == "one_effect"
    assert first["owed_before"]["billd"] == 1 and first["owed_after"]["billd"] == 0
    assert (
        first["owed_before"]["invoice_subscriber"]
        == first["owed_after"]["invoice_subscriber"]
        == 1
    )
    assert set(first["services"]) == {"csmsd", "billd", "roamd", "empd", "poid", "accountingd"}
    assert all(
        r["witness"] == "exact" and r["replay"] == "folded"
        for r in first["services"].values()
    )
    assert len(first["business"]) == 7 and set(first["business"].values()) == {"match"}
    assert snapshot((cold, evidence, keys)) == initial
    keyfile = keys / "data.json"
    original_keys = keyfile.read_bytes()
    keyring = json.loads(original_keys)
    canaries = [
        *keyring.values(),
        "04A1B2C3D4E5F6",
        "DE-MSP-C00000001-W",
        "pilot-archive",
        "PILOT-PAYMENT",
    ]
    for report in (report_file, root / "second.json"):
        assert all(value not in report.read_text() for value in canaries)
    # Deliberate faults use this exercise's synthetic, disposable sources.
    keyring["empd"] = keyring["billd"]
    keyfile.write_text(json.dumps(keyring))
    bad = run(root / "wrong-key.json", 1)
    assert "wrong_data_key" in bad["reasons"]
    assert all(row["replay"] == "unchecked" for row in bad["services"].values())
    keyfile.write_bytes(original_keys)
    sidecar = cold / "poid.ndjson.head"
    original_head = sidecar.read_bytes()
    sidecar.unlink()
    missing = run(root / "missing-head.json", 2)
    assert "input_unavailable" in missing["reasons"]
    assert all(row["replay"] == "unchecked" for row in missing["services"].values())
    sidecar.write_bytes(original_head)
    assert snapshot((cold, evidence, keys)) == initial
    print(
        f"Two isolated passes, two expected refusals. First run {first['elapsed_ms']} ms, "
        f"{sum(p.stat().st_size for p in cold.glob('*.ndjson'))} journal bytes."
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="emob-recovery-") as temp:
        qualify(args.image, args.report, Path(temp).resolve())
