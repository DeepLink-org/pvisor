#!/usr/bin/env python3
"""Publish distinct full-Ubuntu benchmark batches without pooling repetitions."""

import argparse
import csv
import json
from collections import defaultdict
from pathlib import Path

from bench import percentile
from publication import write_derived_summary
from evidence_tsv import copy_evidence, migrate, resolve
from evidence_tsv import load as load_evidence
from render_reference_baselines import export_evidence

LABELS = {
    "native": "Native (Fedora)",
    "pvisor-staged": "pVisor staged",
    "pvisor-vm-hostroot": "pVisor VM / host tools",
    "firecracker-ubuntu": "Firecracker / Ubuntu prepared",
    "firecracker-ubuntu-firstboot": "Firecracker / Ubuntu first boot",
    "qemu-ubuntu": "QEMU q35 / Ubuntu prepared",
    "qemu-microvm-ubuntu": "QEMU microvm / Ubuntu prepared",
}
COLORS = {
    "native": "#8b98a7",
    "pvisor-staged": "#4079b0",
    "pvisor-vm-hostroot": "#148c74",
    "firecracker-ubuntu": "#da8e2a",
    "firecracker-ubuntu-firstboot": "#ac5e23",
    "qemu-ubuntu": "#8663aa",
    "qemu-microvm-ubuntu": "#c56794",
}


def distribution(values):
    return {
        "n": len(values),
        "p50": percentile(values, 50),
        "p95": percentile(values, 95),
        "p99": percentile(values, 99),
        "min": min(values),
        "max": max(values),
    }


def validate_complete(report):
    """Reject partial batches and failed rows before publishing a distribution."""
    modes = report["arguments"]["modes"].split(",")
    backends = report["arguments"]["backends"].split(",")
    planned = int(report["arguments"]["samples"])
    for row in report["rows"]:
        if row.get("correctness") != "passed":
            raise ValueError("Failed workloads cannot enter performance distributions")
    for mode in modes:
        for backend in backends:
            if backend.endswith("firstboot") and mode != "ready":
                continue
            key = f"{mode}/{backend}"
            capability = report["capabilities"].get(key)
            if capability is None:
                raise ValueError(f"Incomplete batch: missing capability {key}")
            trials = [
                r["trial"] for r in report["rows"] if r["mode"] == mode and r["backend"] == backend
            ]
            if capability["state"] == "failed-preflight":
                if trials:
                    raise ValueError(f"Failed preflight has performance samples: {key}")
                continue
            failed = [f["trial"] for f in capability.get("failures", []) if f["trial"] >= 0]
            all_trials = trials + failed
            if len(all_trials) != planned or set(all_trials) != set(range(planned)):
                raise ValueError(f"Incomplete or duplicated trials: {key}")


def summarize(report):
    grouped = defaultdict(list)
    for row in report["rows"]:
        grouped[(row["mode"], row["backend"])].append(row)
    result = {}
    for (mode, backend), rows in grouped.items():
        summary = {
            name: distribution([r[name] for r in rows if r.get(name) is not None])
            for name in (
                "ready_ms",
                "result_ms",
                "completion_ms",
                "prepare_ms",
                "os_ready_ms",
                "peak_tree_rss_kib",
            )
            if any(r.get(name) is not None for r in rows)
        }
        workers = [r["result"]["worker_ms"] for r in rows if "worker_ms" in r["result"]]
        if workers:
            summary["worker_ms"] = distribution(workers)
        for name in ("phases_ms", "filesystem"):
            phases = defaultdict(list)
            for row in rows:
                for key, value in row["result"].get(name, {}).items():
                    phases[key].append(value if name == "phases_ms" else value["worker_ms"])
            if phases:
                summary[name] = {k: distribution(v) for k, v in phases.items()}
        result[f"{mode}/{backend}"] = summary
    return result


def write_derived_summary(public_output / "summary.csv", summaries)
    plot(summaries, public_output):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10})
    fig, ax = plt.subplots(figsize=(10, 5.1))
    names = [b for b in LABELS if f"ready/{b}" in summaries]
    for index, backend in enumerate(names):
        value = summaries[f"ready/{backend}"]["ready_ms"]
        ax.barh(index, value["p50"], color=COLORS[backend], height=0.52)
        ax.plot([value["p50"], value["p95"]], [index, index], color="#243344", linewidth=1.7)
        ax.plot(value["p95"], index, "|", color="#243344")
        ax.text(
            value["p95"] * 1.1,
            index,
            f"{value['p50']:.1f} / {value['p95']:.1f} ms; N={value['n']}",
            va="center",
            fontsize=9,
        )
    ax.set_yticks(range(len(names)), [LABELS[b] for b in names])
    ax.invert_yaxis()
    ax.set_xscale("log")
    ax.set_xlabel("Launch to ready (ms, logarithmic scale); bars P50, ticks P95")
    ax.set_xlim(0.7, max(summaries[f"ready/{b}"]["ready_ms"]["p95"] for b in names) * 40)
    ax.set_title(
        "Image-free pVisor and complete Ubuntu on Firecracker/QEMU\nLinux / KVM; fresh VM; 2 vCPU, 2 GiB; separate measurement batches"
    )
    ax.grid(axis="x", alpha=0.16)
    ax.set_axisbelow(True)
    fig.tight_layout()
    fig.savefig(out / "ubuntu-startup.svg", bbox_inches="tight")
    fig.savefig(out / "ubuntu-startup.png", dpi=150, bbox_inches="tight")
    plt.close(fig)
    fig, axes = plt.subplots(1, 3, figsize=(13.1, 5.8), sharey=True)
    backends = [
        b
        for b in LABELS
        if b != "firecracker-ubuntu-firstboot"
        and any(f"{mode}/{b}" in summaries for mode in ("tools", "claude", "codex"))
    ]
    for ax, mode, title in zip(
        axes,
        ("tools", "claude", "codex"),
        ("Repair and tests", "Claude tool loop", "Codex tool loop"),
        strict=True,
    ):
        for index, backend in enumerate(backends):
            key = f"{mode}/{backend}"
            if key not in summaries:
                ax.text(0.05, index, "PRECHECK FAILED; N=0", va="center", fontsize=8)
                continue
            value = summaries[key]["result_ms"]
            ax.barh(index, value["p50"] / 1000, color=COLORS[backend], height=0.52)
            ax.plot(
                [value["p50"] / 1000, value["p95"] / 1000],
                [index, index],
                color="#243344",
                linewidth=1.7,
            )
            ax.plot(value["p95"] / 1000, index, "|", color="#243344")
            ax.text(
                value["p95"] / 1000 + 0.12,
                index,
                f"{value['p50'] / 1000:.2f} / {value['p95'] / 1000:.2f}",
                va="center",
                fontsize=8,
            )
        ax.set_title(title)
        ax.set_xlabel("Launch to graded result (seconds)")
        ax.grid(axis="x", alpha=0.16)
        ax.set_axisbelow(True)
        high = max(
            (
                summaries[f"{mode}/{b}"]["result_ms"]["p95"] / 1000
                for b in backends
                if f"{mode}/{b}" in summaries
            ),
            default=1,
        )
        ax.set_xlim(0, high * 1.35 + 0.8)
    axes[0].set_yticks(range(len(backends)), [LABELS[b] for b in backends])
    axes[0].invert_yaxis()
    fig.suptitle(
        "Complete offline Agent task; every trial starts a new environment\nUp to 10 successful samples per case; bars P50, ticks P95; fixture responses, no real inference"
    )
    fig.tight_layout()
    fig.savefig(out / "ubuntu-workflows.svg")
    fig.savefig(out / "ubuntu-workflows.png", dpi=150)
    plt.close(fig)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--report", type=Path, action="append", required=True)
    p.add_argument("--assets", type=Path, required=True)
    p.add_argument(
        "--replace-cohort",
        action="append",
        default=[],
        help="Explicitly select a later batch for this mode/backend; never pool",
    )
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    public_output = args.output
    public_output.mkdir(parents=True, exist_ok=True)
    out = public_output / ".data"
    out.mkdir(exist_ok=True)
    summaries, metadata, samples, selected = {}, {}, [], {}
    for report_path in args.report:
        report = load_evidence(report_path)
        validate_complete(report)
        batch = report_path.parent.name
        target = out / batch
        target.mkdir(exist_ok=True)
        copy_evidence(report_path, target / "report.json")
        summary = summarize(report)
        overlap = summaries.keys() & summary.keys()
        if overlap - set(args.replace_cohort):
            raise ValueError(f"Keep overlapping cohorts separate: {overlap}")
        summaries.update(summary)
        selected.update({key: batch for key in summary})
        metadata[batch] = {
            "report": batch + "/report.tsv",
            "rows": len(report["rows"]),
            "capabilities": report["capabilities"],
        }
        (target / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        export_evidence(report_path.parent, target)
        for row in report["rows"]:
            samples.append(
                {
                    "batch": batch,
                    **{
                        k: row.get(k)
                        for k in (
                            "backend",
                            "mode",
                            "trial",
                            "correctness",
                            "ready_ms",
                            "result_ms",
                            "completion_ms",
                            "prepare_ms",
                            "os_ready_ms",
                            "peak_tree_rss_kib",
                        )
                    },
                    "worker_ms": row["result"].get("worker_ms"),
                }
            )
    for sample in samples:
        key = f"{sample['mode']}/{sample['backend']}"
        sample["selected_for_summary"] = sample["batch"] == selected[key]
    with (out / "samples.csv").open("w") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(samples[0]))
        writer.writeheader()
        writer.writerows(samples)
    (out / "summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
    (out / "manifest.json").write_text(
        json.dumps(
            {
                "schema": "pvisor-full-ubuntu-reference-archive/v1",
                "batches": metadata,
                "cohorts": "No pooling; per-case counts are recorded in summary distributions",
                "selected_cohorts": selected,
                "explicit_replacements": args.replace_cohort,
                "selected_samples": sum(s["selected_for_summary"] for s in samples),
                "archived_samples": len(samples),
            },
            indent=2,
        )
        + "\n"
    )
    for name in (
        "assets.json",
        "ubuntu-generic.config",
        "partition-layout.json",
        "provision-command-v2.json",
        "provision-command.json",
        "ubuntu-packages.txt",
        "provision-v5.log",
        "provision.log",
    ):
        source = args.assets / name
        if resolve(source).exists():
            copy_evidence(source, out / name)
    write_derived_summary(public_output / "summary.csv", summaries)
    plot(summaries, public_output)
    migrate(out, replace=True)
    print(out)


if __name__ == "__main__":
    main()
