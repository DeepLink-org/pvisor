#!/usr/bin/env python3
"""Prepare B-PROCESS's JSON suite and validate an untimed completed Run.

Benchmark: B-PROCESS (benchmark/README.md#b-process), role engineering A/B.
Motivation: catch minimal Run and Bundle read regressions.
Conclusion sought: direct-command native/run/status/review levels for CI.
Design: frozen CLI; independent storage; completed Bundle validation; 2/10 or 10/50.
"""

import argparse
import importlib.util
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("benchmark_common", ROOT / "common.py")
COMMON = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(COMMON)


def prepare_template(template, binary, output=None):
    binary = Path(binary).resolve(strict=True)
    batch = COMMON.create_batch(template, output)
    try:
        (batch / "workspace").mkdir()
        env = os.environ.copy() | dict(
            PVISOR_RUN_HOME=str(batch / "runs"), PVISOR_STARTUP_TIMING="0", PVISOR_FS_PROFILE="0"
        )
        command = [
            str(binary),
            "run",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--stdio",
            "capture",
            "--",
            "/usr/bin/true",
        ]
        result = subprocess.run(
            command, cwd=batch / "workspace", env=env, capture_output=True, text=True
        )
        (batch / "preflight.stdout").write_text(result.stdout)
        (batch / "preflight.stderr").write_text(result.stderr)
        if result.returncode:
            raise ValueError("process preflight failed; see " + str(batch / "preflight.stderr"))
        bundles = list((batch / "runs").glob("run-*/run-bundle.json"))
        if len(bundles) != 1:
            raise ValueError("preflight must create exactly one completed Run Bundle")
        checker = Path(__file__).with_name("check.py")
        subprocess.run(["python3", str(checker), "bundle", str(bundles[0])], check=True)
        (batch / "runs/.checked.json").write_text(json.dumps([str(bundles[0])]))
        COMMON.materialize_commands(
            template,
            batch,
            dict(BINARY=binary, JOB=bundles[0].parent),
            [binary, checker],
            batch / "workspace",
            [Path(__file__), checker, Path(__file__).with_name("compare.py")],
        )
        return batch
    except BaseException as error:
        COMMON.save(batch / "failure.json", dict(error=str(error)))
        raise


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("template", type=Path)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--output", type=Path)
    args = p.parse_args()
    try:
        print(prepare_template(args.template, args.binary, args.output))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        p.exit(1, f"{error}\n")
