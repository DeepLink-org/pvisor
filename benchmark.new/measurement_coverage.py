"""Publish the measured scope of every user page without inventing missing results."""

import csv
import json
from pathlib import Path

from common import REPO, ROOT, save
from public_assets import csv_path


def filesystem_publication():
    """Recognize an accepted reference cohort, separate from row-clock bindings."""
    import hashlib

    for prefix, modes in (("filesystem-layered-references", 9), ("filesystem-layered", 5)):
        layered = REPO / "docs/overrides/assets/benchmarks" / prefix / "provenance.json"
        if not layered.exists():
            continue
        evidence = json.loads(layered.read_text())
        counts = evidence.get("counts", {})
        if (
            evidence.get("benchmark_id") != "B-FS-TOOLS"
            or evidence.get("role") != "user-facing"
            or evidence.get("state") not in ("passed", "partial")
            or counts.get("cases") != 19
            or counts.get("backends") != modes
            or counts.get("expected_environments") != 19 * modes * 3
        ):
            raise ValueError("invalid layered filesystem publication receipt")
        if modes == 9 and [mode.get("id") for mode in evidence.get("modes", [])] != [
            "native",
            "host",
            "staged",
            "vm",
            "vm-writeback",
            "docker",
            "fc-system",
            "qemu",
            "qemu-microvm",
        ]:
            raise ValueError("reference filesystem publication mode inventory differs")
        for relative, expected in evidence["outputs"].items():
            if hashlib.sha256((REPO / relative).read_bytes()).hexdigest() != expected:
                raise ValueError("layered filesystem export changed: " + relative)
        return {**evidence, "kind": "layered"}
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
        if (
            hashlib.sha256(csv_path(REPO, name).read_bytes()).hexdigest()
            != evidence["csv_sha256"][name]
        ):
            raise ValueError("filesystem export changed after publication audit: " + name)
    return evidence


def lazy_stage_publication():
    """Recognize an audited same-executor cohort without replacing VM results."""
    import hashlib

    receipt = REPO / "docs/overrides/assets/benchmarks/lazy-stage.provenance.json"
    if not receipt.exists():
        return None
    evidence = json.loads(receipt.read_text())
    counts = evidence.get("counts", {})
    expected = {
        str(csv_path(REPO, f"lazy-stage-{name}.csv").relative_to(REPO))
        for name in ("summary", "comparisons")
    }
    if (
        evidence.get("benchmark_id") != "B-LAZY-STARTUP"
        or evidence.get("role") != "user-facing"
        or evidence.get("state") != "passed"
        or evidence.get("scope") != "same-host-staged-lower-readonly"
        or counts.get("formal_rows") != 120
        or counts.get("samples_per_cell") != 30
        or counts.get("warmup_rows") != 12
        or set(evidence.get("outputs", {})) != expected
    ):
        raise ValueError("invalid same-executor lazy image publication receipt")
    for relative, checksum in evidence["outputs"].items():
        if hashlib.sha256((REPO / relative).read_bytes()).hexdigest() != checksum:
            raise ValueError("lazy stage export changed: " + relative)
    return evidence


def coverage():
    inventory = json.loads((ROOT / "docs.json").read_text())
    entries = list(csv.DictReader((ROOT / "registry.csv").open()))
    filesystem_evidence = filesystem_publication()
    lazy_stage_evidence = lazy_stage_publication()
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
        filesystem_page = (
            page["page"] == "benchmarks/filesystem.md" and filesystem_evidence is not None
        )
        full_filesystem = filesystem_page and filesystem_evidence.get("state") == "passed"
        if filesystem_page:
            current = ["B-FS-TOOLS accepted reference cohort " + filesystem_evidence["batch"]]
        if page["page"] == "benchmarks/lazy-image-startup.md" and lazy_stage_evidence is not None:
            current.append("startup/stage_lazy_compare.json " + lazy_stage_evidence["batch"])
        rows.append(
            dict(
                page=page["page"],
                available_bindings=";".join(current),
                missing_metric_adapters=";".join(missing),
                coverage="full" if full_filesystem else "partial" if current else "historical",
            )
        )
    for locale in ("zh", "en"):
        base = REPO / "docs/src" / locale / "benchmarks"
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
                if filesystem_evidence and filesystem_evidence.get("kind") == "layered":
                    description = (
                        (
                            "九种配置（含Docker、Firecracker、QEMU q35/microvm）；12项机制、4项资源边界、3项完整任务；Ready与退出分开，N3新环境、内部三轮"
                            if locale == "zh"
                            else "Nine modes including Docker, Firecracker, QEMU q35/microvm; 12 mechanisms, four resource anchors, three tasks; Ready excludes exit; N3 fresh environments with three resident rounds"
                        )
                        if filesystem_evidence["counts"]["backends"] == 9
                        else (
                            "五种配置；12项机制、4项资源边界、3项完整任务；Ready与退出分开，N3新环境、内部三轮"
                            if locale == "zh"
                            else "Five modes; 12 mechanisms, four resource anchors, three tasks; Ready excludes exit; N3 fresh environments with three resident rounds"
                        )
                    )
                else:
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
            elif page == "lazy-image-startup.md" and lazy_stage_evidence is not None:
                description = (
                    "同一 host staged 执行器的四文件惰性加载；冷/热各 N30，含取数和挂载；VM 应用启动保留独立历史数据"
                    if locale == "zh"
                    else "Same host staged executor, four-file lazy loading; N30 cold/warm including fetch and mount; VM application startup retains separate historical data"
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
