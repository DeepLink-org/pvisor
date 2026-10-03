#!/usr/bin/env python3
"""Export auditable samples and figures from a completed reference experiment."""

import argparse
import csv
import json
import shutil
import tarfile
from pathlib import Path

from bench import percentile

BACKENDS = [
    "native",
    "pvisor-host",
    "pvisor-staged",
    "pvisor-vm",
    "docker",
    "firecracker",
    "qemu",
    "qemu-microvm",
]
LABELS = {
    "native": "Native",
    "pvisor-host": "pVisor host",
    "pvisor-staged": "pVisor staged",
    "pvisor-vm": "pVisor VM",
    "docker": "Docker rootless",
    "firecracker": "Firecracker PCI",
    "qemu": "QEMU q35",
    "qemu-microvm": "QEMU microvm",
}


def stats(rows, field):
    values = [row[field] for row in rows]
    return {
        "n": len(values),
        **{f"p{q}": percentile(values, q) for q in (50, 95, 99)},
        "min": min(values),
        "max": max(values),
    }


def summarize(report):
    result = {}
    for mode in report["arguments"]["modes"].split(","):
        result[mode] = {}
        for backend in BACKENDS:
            rows = [
                row for row in report["rows"] if row["mode"] == mode and row["backend"] == backend
            ]
            if not rows:
                result[mode][backend] = {
                    "n": 0,
                    "capability": report["capabilities"].get(mode + "/" + backend),
                }
                continue
            entry = {
                "n": len(rows),
                **{
                    name: stats(rows, name)
                    for name in (
                        "ready_ms",
                        "result_ms",
                        "completion_ms",
                        "prepare_ms",
                        "peak_tree_rss_kib",
                    )
                },
            }
            if mode == "filesystem":
                entry["filesystem"] = {
                    task: stats(
                        [{"ms": row["result"]["filesystem"][task]["worker_ms"]} for row in rows],
                        "ms",
                    )
                    for task in rows[0]["result"]["filesystem"]
                }
            if mode == "tools":
                entry["phases"] = {
                    task: stats([{"ms": row["result"]["phases_ms"][task]} for row in rows], "ms")
                    for task in rows[0]["result"]["phases_ms"]
                }
            result[mode][backend] = entry
    return result


def figures(summary, out):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.lines import Line2D

    plt.rcParams.update(
        {
            "font.size": 10,
            "axes.spines.top": False,
            "axes.spines.right": False,
            "svg.fonttype": "none",
            "savefig.bbox": "tight",
        }
    )
    legend = [
        Line2D([0], [0], linewidth=7, color="#2375b8", label="P50"),
        Line2D([0], [0], marker="|", markersize=12, color="#222222", linestyle="none", label="P95"),
    ]

    def panel(ax, mode, metric, title, scale, unit):
        data = summary[mode]
        for y, backend in enumerate(BACKENDS):
            entry = data[backend]
            if not entry["n"]:
                ax.text(
                    0.02,
                    y,
                    "FAILED PREFLIGHT (no latency sample)",
                    transform=ax.get_yaxis_transform(),
                    color="#ae2525",
                    va="center",
                    fontsize=9,
                )
                continue
            p50 = entry[metric]["p50"] / scale
            p95 = entry[metric]["p95"] / scale
            color = "#2375b8" if backend.startswith("pvisor") else "#738597"
            ax.barh(y, p50, color=color, height=0.62)
            ax.plot([p50, p95], [y, y], color="#222222", linewidth=1.2)
            ax.plot(p95, y, marker="|", color="#222222", markersize=9)
        ax.set_yticks(range(len(BACKENDS)), [LABELS[b] for b in BACKENDS])
        ax.invert_yaxis()
        ax.set_xlim(left=0)
        ax.set_xlabel(unit)
        ax.set_title(title, loc="left", weight="bold")
        ax.grid(axis="x", alpha=0.15)
        ax.set_axisbelow(True)

    fig, ax = plt.subplots(figsize=(8, 4.1))
    panel(ax, "ready", "ready_ms", "First command output · prepared environment", 1, "Milliseconds")
    ax.legend(handles=legend, loc="lower right")
    fig.savefig(out / "reference-startup.svg")
    fig.savefig(out / "reference-startup.png", dpi=150)
    plt.close(fig)
    fig, axes = plt.subplots(2, 2, figsize=(13, 8.8))
    for ax, mode, title, metric in [
        (axes[0, 0], "env", "Seven-tool self-check", "ready_ms"),
        (axes[0, 1], "tools", "Repair + Python / Rust / Node tests", "result_ms"),
        (axes[1, 0], "claude", "Claude Code · controlled tool loop", "result_ms"),
        (axes[1, 1], "codex", "Codex · controlled tool loop", "result_ms"),
    ]:
        panel(ax, mode, metric, title, 1000, "Seconds")
    fig.legend(handles=legend, loc="lower center", ncol=2)
    fig.suptitle(
        "Same environment and two physical host cores · 30 samples per available case",
        weight="bold",
    )
    fig.tight_layout(rect=[0, 0.035, 1, 0.97])
    fig.savefig(out / "reference-workflows.svg")
    fig.savefig(out / "reference-workflows.png", dpi=150)
    plt.close(fig)


def export_evidence(source, out):
    with tarfile.open(out / "evidence.tar.gz", "w:gz") as archive:
        for path in sorted((source / "trials").rglob("*")):
            if not path.is_file() or path.name not in (
                "stdout.log",
                "stderr.log",
                "command.json",
                "failure.txt",
                "_model-requests.json",
                "_cli-output.json",
            ):
                continue
            if path.name == "_model-requests.json":
                import io

                requests = json.loads(path.read_text())
                minimized = []
                for request in requests:
                    body = request["body"]
                    proof = {k: body[k] for k in ("model", "stream") if k in body}
                    proof["messages"] = [
                        {
                            "role": message["role"],
                            "content": [
                                block
                                for block in message["content"]
                                if block.get("type") == "tool_result"
                            ],
                        }
                        for message in body.get("messages", [])
                        if isinstance(message.get("content"), list)
                        and any(block.get("type") == "tool_result" for block in message["content"])
                    ]
                    proof["input"] = [
                        item
                        for item in body.get("input", [])
                        if isinstance(item, dict)
                        and item.get("type") in ("function_call", "function_call_output")
                    ]
                    minimized.append({"path": request["path"], "body": proof})
                payload = json.dumps(minimized, indent=2).encode()
                info = tarfile.TarInfo(str(path.relative_to(source)))
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            else:
                archive.add(path, arcname=str(path.relative_to(source)))
        for path in sorted((source / "harness").rglob("*")):
            if path.is_file() and path.suffix in (".py", ".rs", ".sh", ".config"):
                archive.add(path, arcname=str(path.relative_to(source)))
        for path in sorted((source / "trials").rglob("run-bundle.json")):
            value = json.loads(path.read_text())
            run = value["run"]
            proof = {
                "state": run["state"],
                "exit_code": run.get("exit_code"),
                "executor": run["executor"],
                "safety": value.get("safety"),
                "metrics": {k: v for k, v in run.get("metrics", {}).items() if k.startswith("resource.vm_")},
            }
            # Runtime proof is sufficient here; do not publish inherited host environment.
            import io

            payload = json.dumps(proof, indent=2).encode()
            info = tarfile.TarInfo(str(path.relative_to(source).with_name("runtime-proof.json")))
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--report", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--assets", type=Path, required=True)
    args = p.parse_args()
    report = json.loads(args.report.read_text())
    for row in report["rows"]:
        if row["correctness"] != "passed":
            raise ValueError("failed workloads cannot enter the latency distribution")
        if row["backend"] == "docker" and "exact container shim" not in row["memory_scope"]:
            row["memory_scope"] = (
                "CLI + private dockerd descendants; detached container task RSS excluded (audited)"
            )
    report["resource_audit"] = (
        "The initial daemon-tree sampler omitted detached containerd shims. Docker RSS from that batch is partial, not comparable to full workload trees. The separate resource batch explicitly follows the exact container ID/shim."
    )
    if "summary" not in report:
        raise ValueError("report is incomplete: summary is absent")
    args.output.mkdir(parents=True, exist_ok=False)
    summary = summarize(report)
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    shutil.copy2(args.report, args.output / "report.original.json")
    for name in ("assets.json", "tool-identities.json", "kernel.config"):
        if (args.assets / name).is_file():
            shutil.copy2(args.assets / name, args.output / name)
    fields = [
        "mode",
        "backend",
        "trial",
        "correctness",
        "ready_ms",
        "result_ms",
        "completion_ms",
        "prepare_ms",
        "peak_tree_rss_kib",
        "memory_scope",
    ]
    with (args.output / "samples.csv").open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields)
        writer.writeheader()
        writer.writerows({k: row[k] for k in fields} for row in report["rows"])
    (args.output / "compatibility.json").write_text(
        json.dumps(report["capabilities"], indent=2) + "\n"
    )
    export_evidence(args.report.parent, args.output)
    figures(summary, args.output)
    print("Exported", len(report["rows"]), "successful samples to", args.output)


if __name__ == "__main__":
    main()
