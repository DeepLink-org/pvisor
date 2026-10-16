"""Publish the measured scope of every user page without inventing missing results."""

import csv
import io
import json
from pathlib import Path

from common import REPO, ROOT, save
from update import replace


def filesystem_publication():
    """Recognize an accepted reference cohort, separate from row-clock bindings."""
    import hashlib

    base = REPO / "docs/src/zh/benchmarks"
    receipt = REPO / "docs/overrides/assets/benchmarks/filesystem-publication-audit.json"
    if not receipt.exists():
        return None
    evidence = json.loads(receipt.read_text())
    if (
        evidence.get("state") != "passed"
        or evidence.get("benchmark_id") != "B-FS-TOOLS"
        or evidence.get("formal_rows") != 240
        or evidence.get("samples_per_backend") != 30
    ):
        raise ValueError("invalid accepted filesystem publication receipt")
    for name in ("filesystem.csv", "filesystem-comparisons.csv", "filesystem-provenance.csv"):
        if hashlib.sha256((base / name).read_bytes()).hexdigest() != evidence["csv_sha256"][name]:
            raise ValueError("filesystem export changed after publication audit: " + name)
    return evidence


def coverage():
    inventory = json.loads((ROOT / "docs.json").read_text())
    entries = list(csv.DictReader((ROOT / "registry.csv").open()))
    filesystem_evidence = filesystem_publication()
    rows = []
    for page in inventory["pages"]:
        if page["locale"] != "zh" or not page["page"].startswith("benchmarks/"):
            continue
        bindings = {
            entry: binding
            for entry, binding in inventory["bindings"].items()
            if binding["page"] == page["page"]
        }
        missing = [
            entry["entry"]
            for entry in entries
            if entry["role"] == "user-facing"
            and entry["benchmark_id"] in page["benchmark_ids"]
            and entry["status"] == "requires-single-condition-command"
        ]
        current = []
        for entry, binding in bindings.items():
            path = (
                REPO
                / "docs/src/zh"
                / (binding.get("table") or "benchmarks/startup-timing.ready.render.json")
            )
            if not path.exists():
                continue
            data = json.loads(path.read_text())
            if binding.get("kind") == "startup":
                import sys

                sys.path.insert(0, str(ROOT / "startup"))
                from publish import LABELS

                name = Path(entry).stem.removesuffix("_timing").replace("_", "-")
                measured = data.get(LABELS[name], {}).get("N") is not None
            else:
                measured = any(row.get("N") is not None for row in data.values())
            if measured:
                current.append(entry)
        full_filesystem = (
            page["page"] == "benchmarks/filesystem.md" and filesystem_evidence is not None
        )
        if full_filesystem:
            current = ["B-FS-TOOLS accepted reference cohort " + filesystem_evidence["batch"]]
        rows.append(
            dict(
                page=page["page"],
                available_bindings=";".join(current),
                missing_metric_adapters=";".join(missing),
                coverage="full" if full_filesystem else "partial" if current else "historical",
            )
        )
    output = io.StringIO()
    writer = csv.DictWriter(output, fieldnames=list(rows[0]))
    writer.writeheader()
    writer.writerows(rows)
    for locale in ("zh", "en"):
        base = REPO / "docs/src" / locale / "benchmarks"
        replace(base / "measurement-coverage.csv", output.getvalue())
        table = {}
        for row in rows:
            page = row["page"].removeprefix("benchmarks/")
            if page == "startup.md":
                description = (
                    "同次启动 Ready / Exit；完整命令另列"
                    if locale == "zh"
                    else "Same-launch Ready / Exit; whole-command time is separate"
                )
            elif page == "replay-fidelity.md":
                description = (
                    "固定 task 0 的完整 CLI 时间；20 任务历史分布另列"
                    if locale == "zh"
                    else "Whole CLI time for fixed task 0; historical 20-task distributions are separate"
                )
            elif page == "filesystem.md":
                description = (
                    (
                        "八后端七工具内部阶段与启动至退出；原生完整命令探针另列"
                        if locale == "zh"
                        else "Eight-backend seven-tool phases and launch through exit; native whole-command probes are separate"
                    )
                    if row["coverage"] == "full"
                    else (
                        "原生 metadata/read 完整命令；七工具跨后端任务未复测"
                        if locale == "zh"
                        else "Native metadata/read whole commands; seven-tool cross-backend task not rerun"
                    )
                )
            else:
                description = (
                    "保留原始指标与来源；暂无匹配的新测量"
                    if locale == "zh"
                    else "Original metrics and sources retained; no matching new measurements"
                )
            label = "总览" if locale == "zh" else "Overview"
            name = (
                label
                if page == "index.md"
                else f"[{page.removesuffix('/index.md').removesuffix('.md')}]({page})"
            )
            table[name] = {
                "数据状态" if locale == "zh" else "Data status": (
                    "完整复测" if locale == "zh" else "Fully rerun"
                )
                if row["coverage"] == "full"
                else ("部分复测" if locale == "zh" else "Partially rerun")
                if row["coverage"] == "partial"
                else ("历史数据" if locale == "zh" else "Historical data"),
                "实测范围" if locale == "zh" else "Measured scope": description,
            }
        save(base / "measurement-coverage.render.json", table)
    return rows


if __name__ == "__main__":
    rows = coverage()
    print(
        f"{len(rows)} user pages: {sum(row['coverage'] == 'partial' for row in rows)} with new matched measurements"
    )
