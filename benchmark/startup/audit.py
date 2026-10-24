"""Audit the current paired event protocol, samples, output and prepared inputs."""

import json
import shlex
import sys
from pathlib import Path

from hooks import validate_output
from subjects import LABELS, NAMES
from timing import COMMON, FIELDS, ROOT, RUNS, TIMEOUT, WARMUP, summarize_pairs


def audit_startup(directory, manifest, results):
    sys.path.insert(0, str(ROOT.parent / "benchmark/support"))
    from firecracker_kernels import verify_kernel_receipt
    from reference_inputs import verify_reference_inputs

    root = Path(directory)
    if COMMON.digest(root / "protocol.json") != manifest["artifacts"][str(root / "protocol.json")]:
        raise ValueError("startup protocol checksum mismatch")
    protocol = json.loads((root / "protocol.json").read_text())
    if manifest["runs"] < 30 or protocol["backends"] != list(results):
        raise ValueError("startup audit requires >=30 samples per condition")
    after = verify_reference_inputs(Path(protocol["assets"])) if protocol["assets"] else None
    if after != protocol["input_before"]:
        raise ValueError("prepared inputs changed")
    if (
        protocol["kernel"]
        and verify_kernel_receipt(Path(protocol["kernel_receipt"]), "fc-system")
        != protocol["kernel"]
    ):
        raise ValueError("stock kernel changed")
    history = list((root / "trials").glob("*/condition.json"))
    for backend in protocol["backends"]:
        trials = [
            json.loads(p.read_text())
            for p in history
            if json.loads(p.read_text())["backend"] == backend
        ]
        if len(trials) != 1 + manifest["warmup"] + manifest["runs"] or any(
            trial != dict(backend=backend, validated=True) for trial in trials
        ):
            raise ValueError("missing preflight/warmup/measured validation evidence: " + backend)
    COMMON.save(
        root / "audit.json",
        dict(status="passed", inputs_before=protocol["input_before"], inputs_after=after),
    )


def audit_timing(batch, manifest, evidence, resolved, template):
    """Recompute the row from retained nanoseconds and recheck every trial."""

    if len(manifest["conditions"]) != 1 or manifest["conditions"][0] not in NAMES:
        raise ValueError("expected one known startup backend")
    backend = manifest["conditions"][0]
    label = LABELS[backend]
    entry = "startup/" + backend.replace("-", "_") + "_timing.json"
    if (
        manifest["entry"] not in (entry, "startup/timing.json")
        or manifest["metric_scope"] != COMMON.READY_EXIT_SCOPE
        or manifest["benchmark_id"] != "B-STARTUP"
        or manifest.get("measurement_engine") != "python-perf_counter_ns"
        or manifest["runs"] != RUNS
        or manifest["warmup"] != WARMUP
        or manifest["rows"] != {label: backend}
    ):
        raise ValueError("unexpected Ready measurement protocol")
    fields = FIELDS
    expression = template.get(label)
    if not isinstance(expression, str) or not expression.startswith("$ "):
        raise ValueError("expected one Ready script row")
    script = backend.replace("-", "_") + "_timing.py"
    expected_argv = ["python3", str(ROOT / "startup" / script), str(batch)]
    if len(template) != 1 or shlex.split(expression[2:]) != expected_argv:
        raise ValueError("Ready script command differs from the fixed entry")
    commands = evidence["commands"]
    if (
        len(commands) != 1
        or commands[0].get("kind") != "script-row"
        or commands[0]["command"] != expression[2:]
        or evidence["cells"]
    ):
        raise ValueError("timing evidence must be one script row")
    if evidence.get("rows") != [
        {
            "row": label,
            "command": expression[2:],
            "expression": expression[2:],
            "columns": list(fields),
        }
    ]:
        raise ValueError("Ready row mapping differs")
    protocol = json.loads((batch / "protocol.json").read_text())
    if (
        protocol.get("scope") != manifest["metric_scope"]
        or protocol.get("measurement_engine") != "python-perf_counter_ns"
        or protocol.get("ready") != "exact REFERENCE_READY line received"
        or protocol.get("guest_cpus") != 2
        or protocol.get("guest_memory_mib") != 128
    ):
        raise ValueError("unexpected Ready protocol scope or VM configuration")
    samples = []
    history = sorted((batch / "trials").glob("*/timing.json"))
    phases = (
        [("preflight", 0)]
        + [("warmup", i) for i in range(WARMUP)]
        + [("sample", i) for i in range(RUNS)]
    )
    if len(history) != len(phases):
        raise ValueError("missing Ready trial evidence")
    previous_end = 0
    for path, (phase, index) in zip(history, phases):
        trial = json.loads(path.read_text())
        if (
            (trial.get("phase"), trial.get("index")) != (phase, index)
            or trial["command"] != protocol["subject"]
            or trial["exit_code"] != 0
        ):
            raise ValueError("Ready trial command, order or exit differs")
        start, ready, end = (trial[k] for k in ("start_ns", "ready_ns", "end_ns"))
        if (
            any(type(n) is not int for n in (start, ready, end))
            or not previous_end < start < ready <= end
        ):
            raise ValueError("invalid Ready monotonic timestamps")
        if trial["ready_duration_ns"] != ready - start or end - start > TIMEOUT * 1_000_000_000:
            raise ValueError("invalid Ready duration/timeout")
        if trial.get("exit_duration_ns") != end - start:
            raise ValueError("invalid Exit duration")
        previous_end = end
        root = path.parent
        validate_output(
            (root / "stdout").read_text(),
            (root / "stderr").read_text(),
            guest=backend in ("fc-system", "qemu", "qemu-microvm"),
        )
        if backend in ("pvisor-vm", "pvisor-staged"):
            bundle = json.loads((root / "stage/run-bundle.json").read_text())
            if (
                bundle["run"]["state"] != "completed"
                or bundle["run"]["exit_code"] != 0
                or bundle["run"]["executor"]["isolation"]
                != ("virtual_machine" if backend == "pvisor-vm" else protocol["staged_isolation"])
                or bundle["safety"]["filesystem_changes_staged"] is not True
                or any(p.is_file() for p in (root / "stage/upper").rglob("*"))
            ):
                raise ValueError("Ready VM output/Run Bundle did not pass")
        if phase == "sample":
            samples.append({"ready_ns": ready - start, "exit_ns": end - start})
    raw = json.loads((batch / "timing.samples.json").read_text())
    result = summarize_pairs(samples)
    if raw != {
        "engine": "python-perf_counter_ns",
        "unit": "nanosecond",
        "samples": samples,
        "result": result,
    }:
        raise ValueError("Ready raw samples or summary changed")
    if (
        resolved != {label: result}
        or commands[0]["raw"] != result
        or commands[0]["value"] != result
    ):
        raise ValueError("Ready resolved row differs from archived samples")
    results = {backend: {"command": shlex.join(protocol["subject"])}}
    audit_startup(batch, manifest, results)
    return results
