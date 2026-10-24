"""B-FS-TOOLS: complete native Python commands, 3 warmups and 30 samples.

Motivation: quantify native metadata/read command costs on a prepared warm tree.
Design: 2048 files / 32 directories, one shared 64 MiB payload, perf_counter_ns
around process launch and normal exit. Interpreter, pipes and subject validation
are included; fixture preparation, observer startup and later audits are excluded.
These probes are distinct from seven-tool internal timings and cross-backend tasks.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import FILESYSTEM_SCOPE as SCOPE
from common import ROOT, RUNTIME, digest, save
from workload import MODES, validate_fixture

RUNS = 30
WARMUP = 3
TIMEOUT = 30
FIXTURE_SCOPE = "native Python whole-command probes; 2048 files in 32 directories; 64 MiB read with SHA256; warm cache; distinct from seven-tool internal timings"
FIELDS = RUNTIME.FIELDS
ROWS = {"native-" + mode: mode for mode in MODES}


def command(batch, mode):
    return [sys.executable, str(ROOT / "filesystem/workload.py"), mode, str(batch / "fixture")]


summarize = RUNTIME.summarize


def validate_result(record, mode):
    if record["exit_code"] != 0 or json.loads(record["stdout"]) != dict(
        workload=mode, correctness="passed"
    ):
        raise ValueError("filesystem subject failed or returned incorrect output")


def run_trial(batch, mode, phase, index):
    return RUNTIME.command_trial(
        batch / "trials" / mode / f"{phase}-{index:03d}.json",
        mode,
        phase,
        index,
        dict(argv=command(batch, mode), cwd=str(batch), timeout=TIMEOUT),
        validate=lambda record: validate_result(record, mode),
    )


def measure(batch, mode):
    manifest = json.loads((batch / "manifest.json").read_text())
    if (
        manifest["status"] != "prepared"
        or manifest["metric_scope"] != SCOPE
        or "native-" + mode not in manifest["rows"]
    ):
        raise ValueError("expected a freshly prepared filesystem row")
    if list((batch / "trials" / mode).glob("*.json")) != [
        batch / "trials" / mode / "preflight-000.json"
    ]:
        raise ValueError("do not reuse a measured filesystem row")
    for path, expected in manifest["artifacts"].items():
        if digest(path) != expected:
            raise ValueError("prepared artifact changed: " + path)
    validate_fixture(batch / "fixture")
    samples = RUNTIME.repeat(
        WARMUP, RUNS, lambda phase, index: run_trial(batch, mode, phase, index)
    )
    validate_fixture(batch / "fixture")
    result = summarize(samples)
    save(
        batch / f"{mode}.samples.json",
        dict(engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=result),
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("mode", choices=MODES)
    args = parser.parse_args()
    batch = args.batch.resolve()
    try:
        print(json.dumps(measure(batch, args.mode)))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        manifest = json.loads((batch / "manifest.json").read_text())
        manifest.update(status="failed", error=str(error))
        save(batch / "manifest.json", manifest)
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
