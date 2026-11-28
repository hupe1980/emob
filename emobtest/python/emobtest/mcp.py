"""Probe the local meter verifier over MCP 2026-07-28 stdio."""

from __future__ import annotations

import asyncio
import json
import os
import signal
from collections.abc import Sequence

from .conformance import Report
from .core import Meter

VERSION = "2026-07-28"
TOOL = "verify_meter_record"
META = {
    "io.modelcontextprotocol/protocolVersion": VERSION,
    "io.modelcontextprotocol/clientInfo": {"name": "emobtest", "version": "1"},
    "io.modelcontextprotocol/clientCapabilities": {},
}


async def probe(command: Sequence[str]) -> Report:
    """Run the trusted local command, with a deadline on each request and on shutdown."""
    if not command or isinstance(command, str):
        raise ValueError("supply the server executable and its arguments as a sequence")
    report = Report("mcp", " ".join(command))
    process = await asyncio.create_subprocess_exec(
        *command,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        limit=512 * 1024,
        start_new_session=os.name == "posix",
    )
    assert process.stdin is not None and process.stdout is not None
    writer, reader = process.stdin, process.stdout
    sequence = 0

    async def request(method: str, **kwargs) -> dict:
        nonlocal sequence
        sequence += 1
        message = {
            "jsonrpc": "2.0",
            "id": sequence,
            "method": method,
            "params": {
                "_meta": META,
                **kwargs,
            },
        }
        async with asyncio.timeout(5):
            writer.write(json.dumps(message).encode() + b"\n")
            await writer.drain()
            reply = json.loads(await reader.readline())
        if (
            not isinstance(reply, dict)
            or reply.get("id") != sequence
            or reply.get("jsonrpc") != "2.0"
        ):
            raise ValueError(f"{method} returned an invalid JSON-RPC response")
        return reply

    try:
        discovery = (await request("server/discover")).get("result", {})
        report.check(
            "discovery advertises tools for the requested revision",
            VERSION in discovery.get("supportedVersions", [])
            and "tools" in discovery.get("capabilities", {}),
        )
        catalog = (await request("tools/list")).get("result", {}).get("tools", [])
        tool = next((entry for entry in catalog if entry.get("name") == TOOL), {})
        schema = tool.get("inputSchema", {})
        report.check(
            "the local verifier advertises its read-only contract",
            tool.get("annotations", {}).get("readOnlyHint") is True
            and set(schema.get("required", [])) == {"record", "public_key"}
            and schema.get("additionalProperties") is False,
        )
        meter = Meter(seed=41)
        record = meter.sign("22.5", at="2026-01-01T12:00:00Z")
        arguments = {"record": record, "public_key": meter.public_key}
        for name, args, expected in [
            ("the signature verifies", arguments, "verified"),
            (
                "another meter's key differs",
                {**arguments, "public_key": Meter(42).public_key},
                "mismatch",
            ),
            (
                "an unreadable record is inconclusive",
                {**arguments, "record": "unreadable"},
                "inconclusive",
            ),
        ]:
            result = (await request("tools/call", name=TOOL, arguments=args)).get("result", {})
            answer = result.get("structuredContent", {})
            observed = answer.get("status")
            passed = observed == expected and not result.get("isError")
            report.check(
                name, passed, observed if passed else f"expected {expected}, got {observed!r}"
            )
        for name, args in [
            ("undeclared inputs are refused", {**arguments, "extra": True}),
            ("an oversized record is refused", {**arguments, "record": "x" * 65_537}),
        ]:
            reply = await request("tools/call", name=TOOL, arguments=args)
            report.check(
                name,
                reply.get("error", {}).get("code") == -32602
                or reply.get("result", {}).get("isError") is True,
            )
    except (OSError, ValueError, TypeError, AttributeError) as error:
        report.check("the server answers each request within five seconds", False, str(error))
    finally:
        writer.close()

        async def finish() -> int:
            # Drain in chunks while the peer exits. An unread, full pipe can
            # keep asyncio's process.wait() pending even after a child exits.
            while await reader.read(64 * 1024):
                pass
            return await process.wait()

        try:
            code = await asyncio.wait_for(finish(), 3)
            report.check("the server exits cleanly at end of input", code == 0, f"exit {code}")
        except TimeoutError:
            try:
                if os.name == "posix":
                    os.killpg(process.pid, signal.SIGKILL)
                elif process.returncode is None:
                    process.kill()
            except ProcessLookupError:
                pass
            try:
                await asyncio.wait_for(finish(), 1)
            except TimeoutError:
                pass
            report.check("the server exits cleanly at end of input", False, "shutdown timed out")
    return report
