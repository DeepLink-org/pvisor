"""Summarize instrumented migration logs; never treat them as performance gates."""

import argparse
import json
from pathlib import Path
import re

PHASES = {
    "admission_storage_ms": ("process.entry", "vm.prepare_begin"),
    "vm_prepare_ms": ("vm.prepare_begin", "vm.spawn_begin"),
    "spawn_build_ms": ("vm.spawn_begin", "runner.vmm_built"),
    "guest_and_finish_ms": ("runner.vmm_built", "cli.run_finished"),
    "guest_until_wait_done_ms": ("runner.vmm_built", "vm.wait_done"),
    "transport_drain_ms": ("vm.wait_done", "vm.transport_drained"),
    "executor_finish_ms": ("vm.transport_drained", "vm.output_ready"),
    "terminal_publish_ms": ("vm.output_ready", "cli.run_finished"),
}
STARTUP = re.compile(r"\bstage=(\S+) monotonic_us=(\d+)\b")
PERSISTENCE = re.compile(r"\bobject=(\S+) phase=(\S+) duration_us=(\d+) outcome=(\S+)")


def summarize_trial(row, directory):
    stages, persistence, profiles = {}, {}, {}
    for line in (directory / "stderr.log").read_text().splitlines():
        if line.startswith("pvisor-startup ") and (match := STARTUP.search(line)):
            stage, timestamp = match.groups()
            stages.setdefault(stage, int(timestamp))
        elif line.startswith("pvisor-persistence ") and (match := PERSISTENCE.search(line)):
            obj, phase, duration, outcome = match.groups()
            values = persistence.setdefault(f"{obj}.{phase}", [])
            values.append(dict(duration_us=int(duration), outcome=outcome))
        elif line.startswith("pvisor-fs-profile "):
            profile = json.loads(line.removeprefix("pvisor-fs-profile "))
            # Records are cumulative. Retain each instance's latest checkpoint,
            # never sum successive checkpoints or nested inclusive spans.
            profiles[(profile["component"], profile["instance"])] = profile
    phases, missing = {}, []
    for phase, (start, end) in PHASES.items():
        if start not in stages or end not in stages:
            missing.append(phase)
            continue
        if stages[end] < stages[start]:
            raise ValueError(f"nonmonotonic phase {phase} in {directory}")
        phases[phase] = (stages[end] - stages[start]) / 1000
    return row | dict(
        phases=phases,
        missing_phases=missing,
        persistence=persistence,
        filesystem=list(profiles.values()),
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = json.loads(args.report.read_text())
    protocol = report["protocol"]
    if not protocol["diagnostic_timing"] or not protocol["filesystem_profile"]:
        parser.error("requires a diagnostic-timing and filesystem-profile run")
    rows = [summarize_trial(row, args.report.parent / "trials" / Path(row["work"]).name)
            for row in report["samples"]]
    args.output.write_text(json.dumps(dict(
        scope="Instrumented diagnostics; partial cumulative filesystem checkpoints; inclusive spans",
        source=str(args.report),
        samples=rows,
    ), indent=2) + "\n")
    print(json.dumps([
        {k: row[k] for k in ("batch", "round", "variant", "completion_ms", "phases", "missing_phases")}
        for row in sorted(rows, key=lambda row: row["completion_ms"], reverse=True)[:8]
    ], indent=2))


if __name__ == "__main__":
    main()
