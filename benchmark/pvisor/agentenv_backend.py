#!/usr/bin/env python3
"""AgentENV seven-tool capability preflight (not a performance publication).

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: establish that AgentENV can execute the common seven-tool payload.
Conclusion sought: complete per-call guest results with API time kept separate.
Design: fresh sandbox, frozen reference inputs, two passes, output verification,
bounded calls, owned-sandbox cleanup. No cold-cache or equal-budget ranking.
"""

import argparse
import csv
import importlib.metadata
import json
import math
import os
import platform
import re
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

from agentenv_guest import PREFIX, TOOLS, digest


def validate_endpoint(url):
    parsed = urlsplit(url)
    if (
        parsed.scheme not in ("http", "https")
        or not parsed.hostname
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
    ):
        raise ValueError("expected an HTTP(S) server URL without credentials/query")
    if parsed.scheme == "http" and parsed.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise ValueError("plain HTTP is restricted to loopback")
    return url.rstrip("/")


def validate_result(stdout, expected_digest, calls):
    records = [line[len(PREFIX) :] for line in stdout.splitlines() if line.startswith(PREFIX)]
    if len(records) != 1:
        raise ValueError("expected exactly one complete guest result")
    result = json.loads(records[0])
    if (
        result.get("schema") != 1
        or result.get("correctness") != "passed"
        or result.get("inputs_verified") is not True
    ):
        raise ValueError("guest integrity failed")
    if result.get("input_manifest_sha256") != expected_digest:
        raise ValueError("template input manifest mismatch")
    expected = [(tool, call) for call in range(1, calls + 1) for tool in TOOLS]
    if [(row.get("tool"), row.get("call")) for row in result["calls"]] != expected:
        raise ValueError("missing, duplicated or reordered tool calls")
    for row in result["calls"]:
        values = [row.get("operation_ms"), row.get("child_ms")]
        if any(type(value) not in (int, float) or not math.isfinite(value) for value in values):
            raise ValueError("invalid call duration")
        if not 0 <= values[0] <= values[1] or row.get("correctness") != "passed":
            raise ValueError("failed call or inconsistent timing")
    return result


def save(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def run_trial(factory, template, output, manifest_digest, calls=2, timeout=120, expected_cpus=2):
    output.mkdir(parents=True, exist_ok=False)
    token = uuid.uuid4().hex
    row = dict(
        schema=1,
        benchmark_id="B-FS-DIAG",
        role="diagnostic",
        backend="agentenv",
        state="running",
        run_token=token,
        template=template,
        host_kernel=platform.release(),
        host_load=list(os.getloadavg()),
        formal_comparison_eligible=False,
        limitations=[
            "input verification warms files",
            "template may restore existing memory/cache state",
            "whole host service/ublk CPU and memory budget not yet verified",
            "destroy acknowledgement is not proof of complete host process/device reap",
            "no snapshot/durability or historical speedup claim",
        ],
    )
    sandbox = None
    try:
        start = time.perf_counter_ns()
        # SDK retries/transport time belong to API time, never guest operation time.
        sandbox = factory(
            template=template,
            timeout=timeout + 180,
            metadata={"pvisor_benchmark": token},
            request_timeout=30,
        )
        row.update(
            sandbox_id=sandbox.sandbox_id, create_api_ms=(time.perf_counter_ns() - start) / 1e6
        )
        save(output / "report.json", row)
        command = f"/usr/bin/python3 -B /bench/pvisor-agentenv/guest.py --calls {calls}"
        start = time.perf_counter_ns()
        result = sandbox.commands.run(
            command,
            user="root",
            timeout=timeout,
            envs={
                "PYTHONDONTWRITEBYTECODE": "1",
                "PYTHONPYCACHEPREFIX": "/__pvisor_reference_no_pyc__",
                "PYTHONOPTIMIZE": "0",
            },
        )
        row["command_api_ms"] = (time.perf_counter_ns() - start) / 1e6
        (output / "stdout.log").write_text(result.stdout)
        (output / "stderr.log").write_text(result.stderr)
        row["exit_code"] = result.exit_code
        if result.exit_code != 0:
            raise ValueError("guest command failed; see retained logs")
        row["result"] = validate_result(result.stdout, manifest_digest, calls)
        if row["result"].get("guest_cpus") != expected_cpus:
            raise ValueError("template guest CPU count differs from requested preflight")
        row["state"] = "passed"
    except BaseException as error:
        row["state"] = "failed"
        # Avoid persisting SDK exception bodies that can contain request tokens.
        row["error_type"] = type(error).__name__
        if isinstance(error, ValueError):
            row["error"] = str(error)
        if hasattr(error, "stdout"):
            (output / "stdout.log").write_text(str(error.stdout))
        if hasattr(error, "stderr"):
            (output / "stderr.log").write_text(str(error.stderr))
        if sandbox is None:
            row["creation_state_unknown"] = True
            row["reconcile_metadata_token"] = token
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
    finally:
        if sandbox is not None:
            try:
                start = time.perf_counter_ns()
                acknowledged = sandbox.kill(request_timeout=30)
                row["destroy_api_ms"] = (time.perf_counter_ns() - start) / 1e6
                if acknowledged is False:
                    raise ValueError("destroy was not acknowledged")
                row["destroy_acknowledged"] = True
            except Exception as error:
                row.update(
                    state="failed",
                    destroy_acknowledged=False,
                    cleanup_error_type=type(error).__name__,
                )
        save(output / "report.json", row)
    if row["state"] == "passed":
        with (output / "per-call.csv").open("w", newline="") as stream:
            writer = csv.DictWriter(
                stream, fieldnames=("tool", "call", "operation_ms", "child_ms", "correctness")
            )
            writer.writeheader()
            writer.writerows(
                {k: item[k] for k in writer.fieldnames} for item in row["result"]["calls"]
            )
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", default="http://127.0.0.1:18080")
    parser.add_argument("--template", required=True)
    parser.add_argument(
        "--prepared", type=Path, required=True, help="directory produced by prepare_agentenv.py"
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--calls", type=int, choices=(1, 2), default=2)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--expected-cpus", type=int, default=2)
    parser.add_argument(
        "--api-key-file", type=Path, help="read private local file; never prints the key"
    )
    args = parser.parse_args()
    endpoint = validate_endpoint(args.endpoint)
    if not 1 <= args.timeout <= 600:
        parser.error("timeout must be between 1 and 600 seconds")
    if not re.fullmatch(r"[A-Za-z0-9_.:-]+", args.template):
        parser.error("invalid template identifier")
    if args.output.exists():
        parser.error("output must be new")
    receipt = json.loads((args.prepared / "preparation.json").read_text())
    for name, expected in receipt["files"].items():
        path = Path(name)
        if path.is_absolute() or ".." in path.parts or digest(args.prepared / path) != expected:
            raise ValueError("prepared artifact changed")
    if digest(args.prepared / "input-manifest.json") != receipt["input_manifest_sha256"]:
        raise ValueError("manifest receipt mismatch")
    key = (
        args.api_key_file.read_text().strip()
        if args.api_key_file
        else os.environ.get("E2B_API_KEY")
    )
    if not key:
        parser.error("provide --api-key-file or E2B_API_KEY")
    os.environ.update(E2B_API_URL=endpoint, E2B_SANDBOX_URL=endpoint, E2B_API_KEY=key)
    try:
        from e2b import Sandbox
    except ImportError:
        parser.error("install the E2B Python SDK into an isolated venv first")
    version = importlib.metadata.version("e2b")
    row = run_trial(
        Sandbox.create,
        args.template,
        args.output,
        receipt["input_manifest_sha256"],
        args.calls,
        args.timeout,
        args.expected_cpus,
    )
    row.update(
        e2b_version=version,
        endpoint=endpoint,
        preparation_sha256=digest(args.prepared / "preparation.json"),
        harness_sha256={
            name: digest(Path(__file__).with_name(name))
            for name in ("agentenv_backend.py", "agentenv_guest.py", "prepare_agentenv.py")
        },
    )
    save(args.output / "report.json", row)
    print(json.dumps(dict(state=row["state"], report=str(args.output / "report.json"))))
    raise SystemExit(0 if row["state"] == "passed" else 1)


if __name__ == "__main__":
    main()
