#!/usr/bin/env python3
"""Benchmark: B-MEMORY-SCALE, role engineering A/B.

Question: how much does pVisor UKSM-inspired physical deduplication save versus
default-parameter kernel KSM, at the same bounded scan window and workload?
Four fresh 256 MiB/1-vCPU VMs, 64 MiB checked payload each, 4 CPU/2 GiB/swap0
group including the daemon; repeated/shared-random/unique-random, COW 25/100%.
One independent preflight, then >=5 randomized rounds. Never write KSM sysfs.
Reuse the audited SDK worker and its external resource/interference observer.
This is a Python phase-accounting experiment, not a hyperfine elapsed-time test.
"""

import argparse
import json
import os
import random
import shlex
import shutil
import statistics
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "benchmark/pvisor"))
from memory_savings import host_identity, run  # noqa: E402
from memory_scale import digest, inventory, numbers, raw_field, validate_report  # noqa: E402

ARMS = ("off", "ksm", "adaptive")
PATTERNS = ("repeated", "random-shared", "random-unique")
TUNABLES = (
    "run",
    "pages_to_scan",
    "sleep_millisecs",
    "smart_scan",
    "advisor_mode",
    "merge_across_nodes",
    "use_zero_pages",
    "max_page_sharing",
)


def save(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def kernel_state():
    root = Path("/sys/kernel/mm/ksm")
    files = {p.name: p.read_text().strip() for p in root.iterdir() if p.is_file()}
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if (proc / "comm").read_text().strip() != "ksmd":
                continue
            # Fields after comm start at field 3; utime/stime are fields 14/15.
            fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
            return dict(
                files=files,
                pid=int(proc.name),
                starttime=fields[19],
                cpu_affinity=sorted(os.sched_getaffinity(int(proc.name))),
                cpu_seconds=(int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
            )
        except (FileNotFoundError, PermissionError):
            continue
    raise ValueError("ksmd CPU counters unavailable")


def require_default_ksm(state):
    expected = dict(
        run="1",
        pages_to_scan="100",
        sleep_millisecs="20",
        smart_scan="1",
        merge_across_nodes="1",
        use_zero_pages="0",
        max_page_sharing="256",
    )
    if any(state["files"].get(k) != v for k, v in expected.items()) or "[none]" not in state[
        "files"
    ].get("advisor_mode", ""):
        raise ValueError(
            "default-parameter KSM required; administrator must configure it before sampling"
        )


def metrics(raw):
    result = {}
    for phase in raw["phases"]:
        a = phase["accounting"]
        result[phase["name"]] = dict(
            pss_mib=sum(p["smaps_totals_bytes"]["Pss"] for p in a["processes"]) / 2**20,
            charged_mib=int(raw_field(a, "memory.current")) / 2**20,
            kernel_mib=numbers(raw_field(a, "memory.stat"))["kernel"] / 2**20,
            ksm_mib=sum(p["smaps_totals_bytes"].get("KSM", 0) for p in a["processes"]) / 2**20,
        )
    window = raw["ksm_scan_window"]
    result["scan_cpu_seconds"] = (
        numbers(raw_field(window["after"], "cpu.stat"))["usage_usec"]
        - numbers(raw_field(window["before"], "cpu.stat"))["usage_usec"]
    ) / 1e6
    result["group_cpu_seconds"] = (
        numbers(raw_field(raw["after"], "cpu.stat"))["usage_usec"]
        - numbers(raw_field(raw["before"], "cpu.stat"))["usage_usec"]
    ) / 1e6
    return result


def paired_interval(differences):
    rng = random.Random(20261018)
    estimates = sorted(
        statistics.median(rng.choices(differences, k=len(differences))) for _ in range(5000)
    )
    return [estimates[124], estimates[4874]]


def analyze(report):
    rows = report["attempts"]
    required = {
        (r, p, a) for r in range(report["samples"]) for p in PATTERNS for a in report["arms"]
    }
    identities = [(x["round"], x["pattern"], x["arm"]) for x in rows]
    if (
        not report["complete"]
        or len(identities) != len(set(identities))
        or set(identities) != required
        or any(x["status"] != "successful" for x in rows)
    ):
        raise ValueError("incomplete/failed/duplicate cohort cannot be summarized")
    summaries, comparisons = [], []
    for pattern in PATTERNS:
        for phase in ("ready", "cow25", "cow100"):
            for arm in report["arms"]:
                selected = [x for x in rows if x["arm"] == arm and x["pattern"] == pattern]
                values = [x["metrics"][phase]["pss_mib"] for x in selected]
                summaries.append(
                    dict(
                        pattern=pattern,
                        phase=phase,
                        arm=arm,
                        n=len(values),
                        median_pss_mib=statistics.median(values),
                        range_pss_mib=[min(values), max(values)],
                        samples_pss_mib=values,
                    )
                )
            if not report["preflight"]:
                for arm in report["arms"]:
                    if arm == "off":
                        continue
                    for reference in ("off", "ksm"):
                        if reference not in report["arms"] or reference == arm:
                            continue
                        pairs = []
                        for r in range(report["samples"]):
                            group = {
                                x["arm"]: x
                                for x in rows
                                if x["pattern"] == pattern and x["round"] == r
                            }
                            pairs.append(
                                group[reference]["metrics"][phase]["pss_mib"]
                                - group[arm]["metrics"][phase]["pss_mib"]
                            )
                        interval = paired_interval(pairs)
                        comparisons.append(
                            dict(
                                pattern=pattern,
                                phase=phase,
                                arm=arm,
                                reference=reference,
                                saved_mib_median=statistics.median(pairs),
                                ci95=interval,
                                detected=not (interval[0] <= 0 <= interval[1]),
                            )
                        )
    return dict(
        summary=summaries,
        comparisons=comparisons,
        caution="Inspect retained samples for separated distributions before interpreting medians; no tail estimates.",
    )


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ("build", "rootfs", "firmware", "output"):
        p.add_argument("--" + name, type=Path, required=True)
    p.add_argument("--preflight", action="store_true")
    p.add_argument(
        "--prepare",
        action="store_true",
        help="write a NEW sibling .input.json for semspec bench, without sampling",
    )
    p.add_argument("--samples", type=int, default=5)
    p.add_argument(
        "--without-ksm",
        action="store_true",
        help="independent off/adaptive subset; no KSM comparison",
    )
    args = p.parse_args()
    if args.samples < 5 and not args.preflight:
        p.error("formal cohort requires at least five rounds")
    args.samples = 1 if args.preflight else args.samples
    args.rootfs, args.firmware, args.output, args.build = (
        getattr(args, n).resolve() for n in ("rootfs", "firmware", "output", "build")
    )
    if len(str(args.output / "44/w").encode()) > 70:
        p.error("use a short output path; worker limit is 70 bytes")
    if args.prepare:
        # Build argv directly: JSON string escaping is not shell escaping.
        command = [sys.executable, str(Path(__file__).resolve())]
        for name in ("build", "rootfs", "firmware", "output", "samples"):
            command.extend(["--" + name, str(getattr(args, name))])
        if args.preflight:
            command.append("--preflight")
        if args.without_ksm:
            command.append("--without-ksm")
        table = json.loads(Path(__file__).with_suffix(".json").read_text())
        table["paired-memory"] = "$ " + shlex.join(command)
        destination = args.output.with_suffix(".input.json")
        with destination.open("x") as stream:
            json.dump(table, stream, indent=2)
            stream.write("\n")
        print(destination)
        return 0
    args.output.mkdir()
    state = kernel_state()
    arms = tuple(a for a in ARMS if not args.without_ksm or a != "ksm")
    if "ksm" in arms and state["files"]["run"] != "1":
        save(
            args.output / "blocked.json",
            dict(reason="administrator must enable KSM before this campaign", kernel=state),
        )
        return 2
    if "ksm" in arms:
        require_default_ksm(state)
    receipt = json.loads((args.build / "build-receipt.json").read_text())
    for name, key in (("probe", "example_sha256"), ("pool-daemon", "binary_sha256")):
        if digest(args.build / name) != receipt[key]:
            raise ValueError("build receipt mismatch: " + name)
    if digest(args.build / "source.tar.gz") != receipt["source_archive_sha256"]:
        raise ValueError("frozen source archive mismatch")
    shutil.copy2(args.build / "build-receipt.json", args.output / "build-receipt.json")
    harness = args.output / "harness"
    harness.mkdir()
    for source in (
        Path(__file__),
        *(
            REPO / "benchmark/pvisor" / n
            for n in ("memory_savings.py", "memory_scale.py", "linux_cold_runtime.py")
        ),
    ):
        shutil.copy2(source, harness / source.name)
    inputs = {n: inventory(getattr(args, n)) for n in ("rootfs", "firmware")}
    save(args.output / "input-manifest.json", inputs)
    report = dict(
        benchmark_id="B-MEMORY-SCALE",
        role="engineering A/B",
        samples=args.samples,
        preflight=args.preflight,
        arms=arms,
        host=host_identity(),
        kernel_before=state,
        scope="pVisor UKSM-inspired physical page pool; upstream UKSM kernel is not installed",
        scan_seconds=60,
        budget=dict(
            vms=4,
            memory_mib=256,
            vcpus=1,
            group_memory_max=2**31,
            group_cpu_quota=4,
            affinity=[0, 1, 2, 3],
            swap_max=0,
        ),
        interference="30s quiet admission; reject any visible foreign VM/build; retain all failures, stop campaign",
        cpu_scope="group CPU includes SDK/VM/daemon; ksmd is global and outside group quota, separately recorded over admission and whole attempt",
        harness_sha256={f.name: digest(f) for f in harness.iterdir()},
        attempts=[],
        complete=False,
    )
    save(args.output / "report.json", report)
    args.quiet_seconds, args.static = 30, False
    for round_id in range(args.samples):
        cells = [(pattern, arm) for pattern in PATTERNS for arm in arms]
        random.Random(20261018 + round_id).shuffle(cells)
        for pattern, arm in cells:
            output = args.output / str(len(report["attempts"]))
            condition = dict(
                vms=4,
                mode="pool" if arm == "adaptive" else "fresh",
                pattern=pattern,
                dedup=arm == "ksm",
                seed=20261006,
                settle_ms=500,
                ksm_wait_seconds=60,
                memory_mib=256,
                cpus=1,
            )
            if arm == "adaptive":
                condition["pool_daemon"] = str(args.build / "pool-daemon")
            binary = args.build / "probe"

            def validate(raw, c):
                validate_report(
                    raw,
                    dict(
                        c,
                        rootfs=str(args.rootfs),
                        firmware=str(args.firmware),
                        output=str(output / "w"),
                        example=str(binary),
                        example_sha256=digest(binary),
                        cgroup=raw["before"]["cgroup"],
                    ),
                )

            before = kernel_state()
            row = run(args, binary, output, condition, round_id, False, validator=validate)
            after = kernel_state()
            row.update(arm=arm, pattern=pattern, kernel_before=before, kernel_after=after)
            stable = all(
                before["files"][n] == after["files"][n] == state["files"][n] for n in TUNABLES
            )
            if not stable or (before["pid"], before["starttime"]) != (
                after["pid"],
                after["starttime"],
            ):
                row.update(status="failed", error="KSM configuration/identity changed")
            if row["status"] == "successful":
                row["metrics"] = metrics(json.loads(Path(row["raw"]).read_text()))
                row["metrics"]["global_ksmd_cpu_seconds"] = (
                    after["cpu_seconds"] - before["cpu_seconds"]
                )
            report["attempts"].append(row)
            save(args.output / "report.json", report)
            print(f"round={round_id} {pattern}/{arm}: {row['status']}", file=sys.stderr, flush=True)
            if row["status"] != "successful":
                print(row.get("error"), file=sys.stderr)
                return 1
    report["inputs_unchanged"] = all(inputs[n] == inventory(getattr(args, n)) for n in inputs)
    report["binaries_unchanged"] = all(
        digest(args.build / name) == receipt[key]
        for name, key in (("probe", "example_sha256"), ("pool-daemon", "binary_sha256"))
    )
    report["complete"] = report["inputs_unchanged"] and report["binaries_unchanged"]
    save(args.output / "report.json", report)
    summary = analyze(report)
    save(args.output / "analysis.json", summary)
    print(
        json.dumps(
            dict(
                conditions=len(report["attempts"]),
                samples_per_condition=args.samples,
                complete=report["complete"],
                evidence=str(args.output),
            )
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
