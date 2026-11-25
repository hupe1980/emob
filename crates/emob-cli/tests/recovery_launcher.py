"""Exercise the host's SR-221/SR-222/SR-223/SR-225 containment and timeout paths."""

import argparse
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[3] / "deploy" / "recovery-drill.py"
SPEC = importlib.util.spec_from_file_location("recovery_drill", SCRIPT)
launcher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(launcher)


class LauncherTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.args = argparse.Namespace(image="fixture", report=self.root / "run.json")
        for key in ("copy", "evidence", "keys"):
            path = self.root / key
            path.mkdir()
            setattr(self.args, key, path)

    def test_budget_kills_only_its_own_isolated_run_and_keeps_secrets_private(self):
        calls = []

        def docker(command, **kwargs):
            if command[1:3] == ["context", "inspect"]:
                return subprocess.CompletedProcess(
                    command, 0, "unix:///local/docker.sock"
                )
            calls.append(command)
            if command[1:3] == ["image", "inspect"]:
                return subprocess.CompletedProcess(command, 0, "sha256:" + "a" * 64)
            if command[1] == "run":
                self.assertLessEqual(kwargs["timeout"], 900)
                self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
                self.assertIn("--network=none", command)
                self.assertIn("--read-only", command)
                self.assertIn("--cap-drop=ALL", command)
                self.assertFalse(any("docker.sock" in arg for arg in command))
                self.assertEqual(
                    sum("readonly,bind-recursive=disabled" in arg for arg in command), 3
                )
                raise subprocess.TimeoutExpired(command, 900, output=b"private-canary")
            return subprocess.CompletedProcess(command, 0)

        with patch.object(launcher.subprocess, "run", side_effect=docker):
            self.assertEqual(launcher.run(self.args), 2)
        name = calls[1][calls[1].index("--name") + 1]
        self.assertEqual(calls[-1], ["docker", "rm", "-f", "-v", name])
        report = self.args.report.read_text()
        self.assertEqual(json.loads(report)["verdict"], "incomplete")
        self.assertNotIn("private-canary", report)
        with self.assertRaises(FileExistsError):
            launcher.run(self.args)
        self.assertEqual(report, self.args.report.read_text())

    def test_alias_and_source_output_refused_before_any_write(self):
        with patch.object(launcher.subprocess, "run") as docker:
            self.args.report = self.args.copy / "overwrite.json"
            with self.assertRaises(ValueError):
                launcher.run(self.args)
            self.assertFalse(self.args.report.exists())
            self.args.report = self.root / "report.json"
            self.args.evidence = self.args.copy
            with self.assertRaises(ValueError):
                launcher.run(self.args)
            self.assertFalse(self.args.report.exists())
            docker.assert_not_called()

    def test_remote_docker_context_is_refused_before_engine_access(self):
        with (
            patch.dict(
                launcher.os.environ,
                {"DOCKER_HOST": "tcp://remote.invalid:2376", "DOCKER_CONTEXT": ""},
            ),
            patch.object(launcher.subprocess, "run") as docker,
        ):
            self.assertEqual(launcher.run(self.args), 2)
            docker.assert_not_called()
        self.assertEqual(
            json.loads(self.args.report.read_text())["containment"], "unconfirmed"
        )

    def test_isolation_unavailable_refuses_without_starting_a_worker(self):
        error = subprocess.CalledProcessError(
            1, ["docker", "image", "inspect"], stderr=b"private-marker"
        )
        with patch.object(launcher.subprocess, "run", side_effect=error) as docker:
            self.assertEqual(launcher.run(self.args), 2)
            self.assertEqual(docker.call_count, 1)
        public = self.args.report.read_text()
        self.assertNotIn("private-marker", public)
        self.assertEqual(json.loads(public)["containment"], "unconfirmed")

    def test_nonempty_scratch_and_symlinked_custody_refused(self):
        self.args.scratch = self.root / "scratch"
        self.args.scratch.mkdir()
        marker = self.args.scratch / "previous"
        marker.touch()
        with patch.object(launcher.subprocess, "run") as docker:
            with self.assertRaises(ValueError):
                launcher.run(self.args)
            marker.unlink()
            alias = self.root / "alias"
            alias.symlink_to(self.args.keys)
            self.args.keys = alias
            with self.assertRaises(ValueError):
                launcher.run(self.args)
            docker.assert_not_called()
        self.assertFalse(self.args.report.exists())

    def test_missing_custody_directory_is_incomplete(self):
        self.args.keys.rmdir()
        with patch.object(launcher.subprocess, "run") as docker:
            self.assertEqual(launcher.run(self.args), 2)
            docker.assert_not_called()
        result = json.loads(self.args.report.read_text())
        self.assertEqual(result["reasons"], ["input_unavailable"])


if __name__ == "__main__":
    unittest.main()
