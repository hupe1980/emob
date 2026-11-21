import re
import tomllib
from pathlib import Path

import emobtest as et
from emobtest.cli import main


def test_the_wheel_is_the_workspaces_version():
    root = Path(__file__).resolve().parents[2]
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    assert et.__version__ == workspace, "emobtest/Cargo.toml must follow the emob workspace"


def test_verify_and_price_on_the_command_line(capsys):
    meter = et.Meter(seed=3)
    record = meter.charge("4", start="2026-12-01T10:00:00+01:00", minutes=5)[-1]
    assert main(["verify", record, meter.public_key]) == 0
    assert "verified" in capsys.readouterr().out
    assert main(["verify", record, et.Meter(seed=4).public_key]) == 1
    capsys.readouterr()
    assert main(["price", "--energy", "0.59", "--kwh", "30", "--power", "150"]) == 0
    out = capsys.readouterr().out
    assert re.search(r"Preisstand [0-9A-F]{4}", out) and "17.70 EUR" in out
