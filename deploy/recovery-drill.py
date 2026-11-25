#!/usr/bin/env python3
"""Run one synthetic recovery cut in an offline, read-only Linux container."""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import uuid
from pathlib import Path


def local_dir(value, allow_missing=False):
    path = Path(value).absolute()
    if path != path.resolve(strict=False) or (
        not path.is_dir() and not (allow_missing and not path.exists())
    ):
        raise ValueError("unsafe_input_path")
    if any(char in str(path) for char in (",", '"', "\n")):
        raise ValueError("unsafe_input_path")
    return path


def docker_environment():
    # Inspect the client's local configuration before any engine operation.
    # An SSH or TCP context would otherwise operate on a different machine.
    env = os.environ.copy()
    endpoint = env.get("DOCKER_HOST")
    if env.get("DOCKER_CONTEXT") or not endpoint:
        context = subprocess.run(
            [
                "docker",
                "context",
                "inspect",
                "--format",
                '{{(index .Endpoints "docker").Host}}',
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        )
        endpoint = context.stdout.strip()
    if not endpoint.startswith("unix:///"):
        raise ValueError("local_engine_required")
    env.pop("DOCKER_CONTEXT", None)
    env["DOCKER_HOST"] = endpoint
    return env


def run(args):
    # Reserve a unique report before starting work. Loss of the host during
    # the drill leaves explicit incomplete evidence, never a previous pass.
    parent = local_dir(Path(args.report).parent)
    dest = parent / Path(args.report).name
    inputs = [
        local_dir(value, allow_missing=True)
        for value in (args.copy, args.evidence, args.keys)
    ]

    def within(path, root):
        if path == root or root in path.parents:
            return True
        return root.exists() and any(
            p.exists() and p.samefile(root) for p in (path, *path.parents)
        )

    scratch = local_dir(args.scratch) if getattr(args, "scratch", None) else None
    if scratch is not None and (
        any(scratch.iterdir())
        or within(parent, scratch)
        or any(within(scratch, value) or within(value, scratch) for value in inputs)
    ):
        raise ValueError("scratch_not_fresh")
    for index, path in enumerate(inputs):
        for other in inputs[index + 1 :]:
            if within(path, other) or within(other, path):
                raise ValueError("custody_paths_overlap")
        if within(parent, path):
            raise ValueError("report_inside_source")

    attempt = uuid.uuid4().hex
    name = "emob-recovery-" + attempt
    start = time.monotonic()
    incomplete = {
        "version": 1,
        "verdict": "incomplete",
        "attempt_id": attempt,
        "reasons": ["attempt_interrupted"],
        "containment": "unconfirmed",
        "after_cut": "unknown",
        "limits": {
            "input_bytes": 2 * (1 << 30),
            "journal_bytes": 1 << 30,
            "wall_seconds": 900,
        },
    }

    with os.fdopen(
        os.open(dest, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "w"
    ) as f:
        json.dump(incomplete, f)
        f.flush()
        os.fsync(f.fileno())

    result = incomplete.copy()
    code = 2
    launched = False
    docker_env = None
    try:
        if any(not p.is_dir() for p in inputs):
            raise FileNotFoundError("input_unavailable")
        # Resolve a previously built or approved image to its immutable ID.
        # No registry access, Docker socket, host network or secret environment
        # is available to the process reading the backup.
        docker_env = docker_environment()
        inspection = subprocess.run(
            ["docker", "image", "inspect", "--format", "{{.Id}}", "--", args.image],
            capture_output=True,
            timeout=15,
            check=True,
            text=True,
            env=docker_env,
        )
        image = inspection.stdout.strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise ValueError("image_unavailable")
        command = [
            "docker",
            "run",
            "--pull=never",
            "--rm",
            "--name",
            name,
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit=32",
            "--memory=4g",
            "--memory-swap=4g",
            "--log-driver=none",
            "--user",
            f"{os.getuid()}:{os.getgid()}",
            "--tmpfs",
            "/var/lib/emob:ro,noexec,nosuid,nodev,size=4k",
            "--entrypoint",
            "/usr/local/bin/daemon",
        ]
        if scratch is None:
            command.extend(
                ["--tmpfs", "/scratch:rw,noexec,nosuid,nodev,size=64m,mode=1777"]
            )
        else:
            command.extend(
                [
                    "--mount",
                    f"type=bind,src={scratch},dst=/scratch,bind-recursive=disabled",
                ]
            )
        for path, target in zip(inputs, ("/cut", "/evidence", "/keys")):
            command.extend(
                [
                    "--mount",
                    f"type=bind,src={path},dst={target},readonly,bind-recursive=disabled",
                ]
            )
        command.extend([image, "recovery-drill", "--isolated-worker"])
        launched = True
        worker = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=max(1, 900 - (time.monotonic() - start)),
            check=False,
            env=docker_env,
        )
        if worker.returncode not in (0, 1, 2) or len(worker.stdout) > 65536:
            raise ValueError("worker_incomplete")
        payload = json.loads(worker.stdout)
        if (
            not isinstance(payload, dict)
            or payload.get("version") != 1
            or payload.get("verdict")
            != ("pass", "fail", "incomplete")[worker.returncode]
        ):
            raise ValueError("invalid_worker_result")
        result = payload
        code = worker.returncode
    except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt):
        result["reasons"] = [
            "input_unavailable"
            if any(not p.is_dir() for p in inputs)
            else "worker_unavailable_or_interrupted"
        ]
    finally:
        if launched:
            try:
                subprocess.run(
                    ["docker", "rm", "-f", "-v", name],
                    check=False,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=15,
                    env=docker_env,
                )
            except (OSError, subprocess.SubprocessError):
                code = 2
                result = dict(incomplete, reasons=["worker_cleanup_unconfirmed"])
    result["limits"] = incomplete["limits"]
    result["elapsed_ms"] = int((time.monotonic() - start) * 1000)
    replacement = parent / ("." + attempt + ".json")
    with os.fdopen(
        os.open(replacement, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "w"
    ) as f:
        json.dump(result, f, indent=2)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())
    os.replace(replacement, dest)
    return code


def terminate(_signal, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":

    class Parser(argparse.ArgumentParser):
        def error(self, message):
            self.print_usage(sys.stderr)
            self.exit(64, message + "\n")

    parser = Parser(description=__doc__)
    parser.add_argument("--image", default="emob-recovery:pilot")
    parser.add_argument(
        "--scratch", help="fresh private scratch directory (default: a new tmpfs)"
    )
    for field in ("copy", "evidence", "keys", "report"):
        parser.add_argument("--" + field, required=True)
    signal.signal(signal.SIGTERM, terminate)
    try:
        sys.exit(run(parser.parse_args()))
    except (ValueError, OSError, KeyboardInterrupt):
        print(
            "Recovery inputs must have separate real directories and a fresh report path.",
            file=sys.stderr,
        )
        sys.exit(1)
