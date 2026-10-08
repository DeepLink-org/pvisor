"""Summarize instrumented migration logs; never treat them as performance gates.

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: attribute time to profile spans after an uninstrumented A/B.
Conclusion sought: span and counter breakdown explaining an observed gap.
Design: reads profile-enabled batches only; their timings are not published.
"""

import argparse
import csv
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


def collect_profiles(lines):
    """Keep one cumulative record per process and instance, with coverage."""
    profiles = {}
    for line in lines:
        if not line.startswith("pvisor-fs-profile "):
            continue
        profile = json.loads(line.removeprefix("pvisor-fs-profile "))
        if "pid" not in profile:
            raise ValueError("profile lacks process identity; cannot safely combine instances")
        key = (profile["pid"], profile["component"], profile["instance"])
        previous = profiles.get(key)
        if previous:
            for label, measurement in previous["measurements"].items():
                current = profile["measurements"].get(label, {})
                if any(current.get(field, 0) < measurement.get(field, 0)
                       for field in ("calls", "total_ns", "units")):
                    raise ValueError(f"nonmonotonic cumulative profile {key}/{label}")
        profiles[key] = profile
    return list(profiles.values())


def summarize_trial(row, directory):
    stages, persistence = {}, {}
    lines = (directory / "stderr.log").read_text().splitlines()
    profiles = collect_profiles(lines)
    for line in lines:
        if line.startswith("pvisor-startup ") and (match := STARTUP.search(line)):
            stage, timestamp = match.groups()
            stages.setdefault(stage, int(timestamp))
        elif line.startswith("pvisor-persistence ") and (match := PERSISTENCE.search(line)):
            obj, phase, duration, outcome = match.groups()
            values = persistence.setdefault(f"{obj}.{phase}", [])
            values.append(dict(duration_us=int(duration), outcome=outcome))
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
        filesystem=profiles,
        profile_coverage=dict(instances=len(profiles),
                              final_instances=sum(p.get("final_record", False) for p in profiles),
                              partial_instances=sum(not p.get("final_record", False) for p in profiles),
                              meaning="final records cover completed instance spans; partial records are lower bounds, never complete-run counts"),
    )


def write_counter_csv(rows, path):
    fields = ["backend", "mode", "trial", "pid", "component", "instance", "schema",
              "coverage", "label", "calls", "inclusive_total_ms", "mean_inclusive_us",
              "max_inclusive_ms", "units", "latency_buckets"]
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            for profile in row["filesystem"]:
                for label, value in profile["measurements"].items():
                    calls = value.get("calls", 0)
                    writer.writerow(dict(backend=row.get("backend", row.get("variant", "")),
                        mode=row.get("mode", "filesystem"), trial=row.get("trial", row.get("round", "")),
                        **{field: profile[field] for field in ("pid", "component", "instance", "schema")},
                        coverage="final" if profile.get("final_record") else "partial-lower-bound",
                        label=label, calls=calls, inclusive_total_ms=value.get("total_ns", 0) / 1e6,
                        mean_inclusive_us=value.get("total_ns", 0) / calls / 1000 if calls else "",
                        max_inclusive_ms=value["max_ns"] / 1e6 if "max_ns" in value else "",
                        units=value.get("units", 0), latency_buckets=json.dumps(value.get("latency_buckets"))))


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
