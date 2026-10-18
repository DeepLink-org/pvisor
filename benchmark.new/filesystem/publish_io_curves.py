#!/usr/bin/env python3
"""Benchmark: B-FS-ENG/B-FS-DIAG derived I/O curve publication.

Validate completed, separately collected cohorts and copy derived SVG
bytes without rounding. Raw operation rows, private manifests and host paths are
never published. CSV and provenance are archived in the primary --shape batch.
--check is read-only. Registry/article ownership remains with caller.
"""

import argparse
import csv
import hashlib
import io
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from curves_analysis import describe, seed_for
from public_assets import referenced_svg_paths
from publication_archive import archive_targets, preflight_archive, write_archive

REPO = Path(__file__).resolve().parents[2]
PRIVATE_PATH = re.compile(rb"/(?:home|tmp|sys|proc|run|root)(?:/|\\)")


def require(value, message):
    if not value:
        raise ValueError(message)


def load(path):
    return json.loads(Path(path).read_text())


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def digest(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def public_bytes(path):
    data = Path(path).read_bytes()
    require(not PRIVATE_PATH.search(data), f"private host path in derived output: {path.name}")
    return data


def csv_rows(path):
    data = public_bytes(path)
    rows = list(csv.DictReader(io.StringIO(data.decode())))
    require(rows, f"empty derived table: {path}")
    return rows


def validate_projection(path, expected):
    rows = csv_rows(path)
    require(len(rows) == len(expected), f"derived row count differs: {path.name}")
    for row, original in zip(rows, expected):
        for key, value in original.items():
            formatted = (
                json.dumps(value, sort_keys=True)
                if isinstance(value, (dict, list))
                else ""
                if value is None
                else str(value)
            )
            require(row.get(key) == formatted, f"analysis/CSV differs: {path.name}/{key}")
        require(
            all(key in original or not value for key, value in row.items()),
            f"CSV has unaccounted value: {path.name}",
        )


def ref(path, root):
    path, root = Path(path), Path(root)
    return dict(file=str(path.relative_to(root)), sha256=sha(path))


def read_cohort(batch, diagnostic=False):
    """The read runner has a global, explicit post-verification receipt."""
    batch = batch.resolve()
    protocol, status = load(batch / "protocol.json"), load(batch / "status.json")
    records = load(batch / "records.json")
    post = load(batch / "host-postverification.json")
    require(
        protocol.get("profiling") is diagnostic
        and status.get("valid") is True
        and status.get("fixture_verified") is True
        and status.get("failed") == 0
        and status.get("excluded") == 0
        and status.get("trials") == len(records)
        and post.get("verified") is True
        and post.get("full_sha256") is True
        and all(
            r.get("valid") is True
            and r.get("returncode") == 0
            and all(row.get("correctness") is True for row in r["rows"])
            for r in records
        ),
        "read cache: incomplete collection or full fixture verification",
    )
    analysis = batch / "analysis"
    summary = load(analysis / "summary.json")
    require(
        summary.get("performance_samples") is (not diagnostic)
        and summary.get("valid_environments") == len(records)
        and summary.get("failed_environments") == 0,
        "read cache: incomplete analysis",
    )
    for name, filename in [
        ("protocol", "protocol.json"),
        ("records", "records.json"),
        ("host_postverification", "host-postverification.json"),
    ]:
        require(
            summary["input_sha256"][name] == sha(batch / filename),
            f"read cache: analysis input changed: {name}",
        )
    source = summary["analysis_source"]
    require(sha(source["script"]) == source["script_sha256"], "read cache: analysis source changed")
    for name, value in protocol["artifacts"].items():
        require(
            not Path(name).is_absolute()
            and ".." not in Path(name).parts
            and sha(batch / name) == value,
            f"read cache: frozen artifact changed: {name}",
        )
    if diagnostic:
        validate_projection(analysis / "read_request_summary.csv", summary["request_summary"])
        validate_projection(
            analysis / "read_request_round_summary.csv", summary["request_round_summary"]
        )
    else:
        validate_projection(analysis / "curve_summary.csv", summary["summary"])
        validate_projection(analysis / "paired_ratio_summary.csv", summary["paired_ratio_summary"])
        validate_projection(
            analysis / "cache_round_ratio_summary.csv", summary["round_paired_ratio_summary"]
        )
    # Normalize only the private adapter interface, never the published bytes.
    normalized = dict(
        summary,
        plots=[name for name in summary["output_files"] if Path(name).suffix in (".svg", ".pdf")],
    )
    provenance = dict(
        cohort=batch.name,
        kind="read-diagnostic" if diagnostic else "read-cache",
        trials=len(records),
        samples_per_condition=protocol["samples"],
        resident_rounds=protocol["resident_rounds"],
        reopen_passes=protocol["passes"],
        protocol=ref(batch / "protocol.json", batch),
        records=ref(batch / "records.json", batch),
        host_postverification=ref(batch / "host-postverification.json", batch),
        analysis_summary=ref(analysis / "summary.json", batch),
        analysis_source={key: value for key, value in source.items() if key != "script"},
        product_build_receipt_sha256=protocol["product_receipt_sha256"],
        product_source_files=len(protocol["source_hashes"]),
        product_source_hashes_sha256=digest(protocol["source_hashes"]),
        artifacts=protocol["artifacts"],
        fixture_manifest=protocol["fixture_manifest"],
        rootfs_scope=protocol["rootfs_provenance"],
        independent_unit=summary["independent_unit"],
        statistics=summary["confidence"],
        aggregation=summary["aggregation"],
        evidence=summary["evidence"],
        reuse_labels=summary["reuse_labels"],
        timing_scope=summary["timing"],
        memory_scope=summary["memory"],
        diagnostic_scope="independent request counts only; instrumented timings excluded"
        if diagnostic
        else None,
    )
    return batch, analysis, normalized, provenance


def curve_cohort(batch, kind):
    if kind == "read_cache":
        return read_cohort(batch)
    batch = batch.resolve()
    status = load(batch / "status.json")
    records = load(batch / "records.json")
    protocol = load(batch / "protocol.json")
    require(
        status.get("valid") is True
        and status.get("failed") == 0
        and status.get("excluded") == 0
        and status.get("trials") == len(records)
        and records,
        f"{kind}: incomplete or failed collection",
    )
    require(
        all(
            r.get("valid") is True
            and r.get("returncode") == 0
            and r.get("host_verified") is True
            and r.get("rows")
            and all(v.get("correctness") is True for v in r["rows"])
            for r in records
        ),
        f"{kind}: invalid exit, full-byte verification or payload row",
    )
    require(protocol.get("profiling") is False, f"{kind}: diagnostic cannot supply performance")
    analysis = batch / "analysis-v2"
    if not analysis.is_dir():
        analysis = batch / "analysis"
    summary = load(analysis / "summary.json")
    require(
        summary.get("status") == "analyzed"
        and not summary.get("failures")
        and summary["counts"]["failed_environments"] == 0
        and summary["counts"]["valid_environments"] == len(records),
        f"{kind}: analysis is incomplete or excludes failures",
    )
    require(
        summary["source"]["protocol_sha256"] == sha(batch / "protocol.json")
        and summary["source"]["records_sha256"] == sha(batch / "records.json"),
        f"{kind}: analysis is not bound to these raw inputs",
    )
    flattened = []
    for row in summary["curves"]:
        flat = {}
        for key, value in row.items():
            flat.update(
                {key + "_" + name: item for name, item in value.items()}
                if isinstance(value, dict)
                else {key: value}
            )
        flattened.append(flat)
    validate_projection(analysis / "curve_summary.csv", flattened)
    validate_projection(analysis / "paired_ratio_summary.csv", summary["comparisons"])
    if kind == "shape":
        review = load(analysis / "review.json")
        validate_projection(
            analysis / "targeted_comparisons.csv", review["additional_matched_comparisons"]
        )
    analysis_candidates = [
        batch / "harness/curves_analysis.py",
        REPO / "benchmark.new/filesystem/curves_analysis.py",
    ]
    require(
        any(
            path.is_file() and sha(path) == summary["source"]["analysis_sha256"]
            for path in analysis_candidates
        ),
        f"{kind}: declared analysis source is unavailable",
    )
    for name, expected in protocol["artifacts"].items():
        require(
            not Path(name).is_absolute() and ".." not in Path(name).parts,
            f"{kind}: invalid artifact relative name",
        )
        value = expected.get("sha256") if isinstance(expected, dict) else expected
        require(sha(batch / name) == value, f"{kind}: frozen artifact changed: {name}")
    source_hashes = protocol.get("source_hashes", {})
    require(source_hashes, f"{kind}: product source identity absent")
    plans = {
        name: (v.get("sha256") if isinstance(v, dict) else v)
        for name, v in protocol["artifacts"].items()
        if name.startswith("plan-")
    }
    output = dict(
        cohort=batch.name,
        kind=kind,
        trials=len(records),
        samples_per_condition=protocol["samples"],
        resident_rounds=protocol["resident_rounds"],
        independent_unit=summary["statistical_scope"]["unit"],
        protocol=ref(batch / "protocol.json", batch),
        records=ref(batch / "records.json", batch),
        status=ref(batch / "status.json", batch),
        input_plans=plans,
        analysis_summary=ref(analysis / "summary.json", batch),
        analysis_source_sha256=summary["source"]["analysis_sha256"],
        source_head=protocol.get("source_head"),
        product_source_files=len(source_hashes),
        product_source_hashes_sha256=digest(source_hashes),
        artifacts={
            name: (v.get("sha256") if isinstance(v, dict) else v)
            for name, v in protocol["artifacts"].items()
        },
        statistics=summary["statistical_scope"],
        bootstrap_resamples=summary["bootstrap_resamples"],
        confidence=summary["confidence"],
        counts=summary["counts"],
        input_integrity="exact plan and deterministic payload source; guest plus independent host full-byte checks; no full rootfs tree digest claimed",
        timing_scope=protocol.get("timing"),
        cache_scope=protocol.get("cache"),
        exclusion_scope="no failed or slow valid environment removed; independent batches never pooled",
    )
    return batch, analysis, summary, output


def cli_cohort(batch):
    batch = batch.resolve()
    analysis = load(batch / "timing-analysis.json")
    require(
        analysis.get("state") == "verified"
        and analysis.get("n_paired_environments") == 5
        and analysis.get("dependent_rounds_per_environment") == 3
        and len(analysis["summary"]) == 64
        and len(analysis["comparisons"]) == 32,
        "CLI: wrong analysis/independent sample scope",
    )
    require(
        sum(r["primary"] is True for r in analysis["comparisons"]) == 1,
        "CLI: expected exactly one primary",
    )
    reports = {}
    for phase, count in (("pre", 2), ("profiles", 2), ("timing", 10)):
        path = batch / phase / "report.json"
        report = load(path)
        require(
            report.get("state") == "complete"
            and not report.get("failures")
            and len(report["rows"]) == count,
            f"CLI: {phase} incomplete",
        )
        require(
            all(r.get("normal_cli") is True for r in report["rows"]),
            f"CLI: {phase} is not ordinary CLI",
        )
        reports[phase] = ref(path, batch)
    require(
        analysis["report"]["sha256"] == sha(batch / "timing/report.json"),
        "CLI: analysis/report join differs",
    )
    # Published tables are exact projections of the verified analysis, not a
    # second statistics implementation. Full five-pair values remain in CSV.
    summary = csv_rows(batch / "timing-summary.csv")
    comparisons = csv_rows(batch / "timing-comparisons.csv")
    counters = csv_rows(batch / "diagnostic-counters.csv")
    require(
        len(summary) == 64 and len(comparisons) == 32 and len(counters) == 108,
        "CLI: derived table cardinality differs",
    )
    for table, expected in ((summary, analysis["summary"]), (comparisons, analysis["comparisons"])):
        require(len(table) == len(expected), "CLI: table/analysis differs")
        for row, original in zip(table, expected):
            for key, value in original.items():
                require(
                    row[key] == (json.dumps(value) if isinstance(value, list) else str(value)),
                    f"CLI: derived value differs: {key}",
                )
    before, after = load(batch / "source-before.json"), load(batch / "source-after.json")
    require(
        before["state"] == after["state"] == "verified"
        and before["source_hashes_sha256"] == after["source_hashes_sha256"]
        and before["build_receipt"] == after["build_receipt"],
        "CLI: source association changed",
    )
    build_path = Path(before["build_receipt"]["path"])
    require(
        sha(build_path) == before["build_receipt"]["sha256"], "CLI: current build receipt changed"
    )
    build = load(build_path)
    for name, artifact in build["artifacts"].items():
        require(sha(artifact["path"]) == artifact["sha256"], f"CLI artifact changed: {name}")
    origins = load(batch / "origins.json")
    for key in ("current_build", "original_preparation", "prepared_input_association"):
        require(sha(origins[key]["path"]) == origins[key]["sha256"], f"CLI origin changed: {key}")
    return dict(
        cohort=batch.name,
        kind="ordinary-cli-anchor",
        reports=reports,
        timing_analysis=ref(batch / "timing-analysis.json", batch),
        source_before=ref(batch / "source-before.json", batch),
        source_after=ref(batch / "source-after.json", batch),
        harness_manifest=ref(batch / "harness-manifest.json", batch),
        current_build_receipt_sha256=before["build_receipt"]["sha256"],
        product_source_files=before["files"],
        product_source_hashes_sha256=before["source_hashes_sha256"],
        current_binaries={k: v["sha256"] for k, v in build["artifacts"].items()},
        original_preparation_receipt_sha256=origins["original_preparation"]["sha256"],
        prepared_input_association_sha256=origins["prepared_input_association"]["sha256"],
        prepared_input_scope="existing authenticated owned generations reused; no new import/seal cost charged",
        timing_scope="Ready actual frontend launch to unique marker, fresh private Host+VM, excludes exit; original worker fsync/close clocks",
        statistics="five independent paired environments, three dependent rounds; 5000 paired bootstrap; 32 uncorrected intervals, one aggregate primary",
        profile_scope="separate N1 per arm; checked subprocess intervals include launch and validation; owner-drop peaks are lifetime only; off host-buffer counter is absent by source contract",
    )


def collect(args):
    files, cohorts, resource_rows = {}, [], []

    def add(source, target, kind):
        source = Path(source)
        require(target not in files, f"publication name collision: {target}")
        data = public_bytes(source)
        if source.suffix == ".pdf":
            return  # Verified/read with its analysis; PDF stays in the private batch.
        rows = len(csv_rows(source)) if source.suffix == ".csv" else None
        files[target] = dict(
            source=source, data=data, kind=kind, rows=rows, sha256=hashlib.sha256(data).hexdigest()
        )

    for kind in ("shape", "parameters", "confirm", "read_cache"):
        selected = getattr(args, kind)
        if selected is None:
            continue
        batch, analysis, summary, provenance = curve_cohort(selected, kind)
        cohorts.append(provenance)
        for index, environment in enumerate(load(batch / "records.json")):

            def usage(resource):
                fields = dict(line.split() for line in resource["cpu.stat"].splitlines())
                return int(fields["usage_usec"])

            elapsed = usage(environment["resource"]) - usage(environment["resource_before"])
            require(elapsed >= 0, f"{kind}: negative full-trial CPU difference")
            resource_rows.append(
                dict(
                    cohort=kind.replace("_", "-"),
                    environment=index,
                    pair=environment["pair"],
                    condition=environment["condition"],
                    cpu_delta_usec=elapsed,
                    payload_logical_bytes=sum(row["bytes"] for row in environment["rows"]),
                    payload_rows=len(environment["rows"]),
                    sampled_memory_current_peak_bytes=environment.get(
                        "sampled_memory_current_peak"
                    ),
                    sampled_memory_byte_seconds=environment.get("sampled_memory_byte_seconds"),
                    memory_monitor_samples=environment.get("memory_monitor_samples"),
                    scope="whole trial including launch fixture guest and host verification observer; not per cell; cumulative kernel memory.peak excluded",
                )
            )
        if kind == "shape":
            mapping = {
                "curve_summary.csv": "shape",
                "targeted_comparisons.csv": "shape-comparisons",
            }
        elif kind == "parameters":
            mapping = {
                "paired_ratio_summary.csv": "parameters",
                "curve_summary.csv": "parameters-levels",
            }
        elif kind == "confirm":
            mapping = {"paired_ratio_summary.csv": "confirm", "curve_summary.csv": "confirm-levels"}
        else:
            mapping = {
                "curve_summary.csv": "read-cache",
                "paired_ratio_summary.csv": "read-cache-comparisons",
            }
        for source, name in mapping.items():
            add(analysis / source, f"filesystem-io-{name}.csv", kind)
        if kind == "read_cache":
            for source, target in [
                ("cache_rounds.csv", "read-cache-rounds"),
                ("cache_round_ratios.csv", "read-cache-round-ratios"),
                ("cache_round_ratio_summary.csv", "read-cache-round-comparisons"),
            ]:
                add(analysis / source, f"filesystem-io-{target}.csv", kind)
        # Independent-environment derived points preserve distributions without
        # exposing per-operation raw_rows.csv. They are useful for N5 and splits.
        add(
            analysis / "environment_points.csv",
            f"filesystem-io-{kind.replace('_', '-')}-environments.csv",
            kind,
        )
        for name in summary.get("plots", []):
            source = analysis / name
            require(
                source.suffix in (".svg", ".pdf") and Path(name).name == name,
                "plot path is not a direct SVG/PDF child",
            )
            target = name
            if target in files:
                target = kind.replace("_", "-") + "-" + name
            add(source, target, kind)
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=list(resource_rows[0]))
    writer.writeheader()
    writer.writerows(resource_rows)
    data = stream.getvalue().encode()
    require(not PRIVATE_PATH.search(data), "private path in derived resources")
    files["filesystem-io-resources.csv"] = dict(
        data=data,
        kind="whole-trial-resources",
        rows=len(resource_rows),
        sha256=hashlib.sha256(data).hexdigest(),
    )
    # Whole-trial N30 CPU is a secondary resource measure, not a cell clock.
    cpu_pairs = {}
    for row in resource_rows:
        if row["cohort"] == "confirm":
            cpu_pairs.setdefault(row["pair"], {})[row["condition"]] = row["cpu_delta_usec"]
    require(
        len(cpu_pairs) == 30 and all(set(pair) == {"sync", "async"} for pair in cpu_pairs.values()),
        "resource confirmation requires 30 complete pairs",
    )
    import statistics

    cpu_summary = dict(
        metric="whole_trial_cpu_usage",
        unit="microseconds",
        condition="async",
        baseline="sync",
        sync_median=statistics.median(pair["sync"] for pair in cpu_pairs.values()),
        async_median=statistics.median(pair["async"] for pair in cpu_pairs.values()),
        statistic="median of paired async/sync CPU deltas; 5000 bootstrap resamples; secondary",
        scope="entire four-cell trial including startup preparation guest and host verification observer",
        **describe(
            [pair["async"] / pair["sync"] for pair in cpu_pairs.values()],
            bootstrap=True,
            seed=seed_for("resource-confirm-cpu"),
        ),
    )
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=list(cpu_summary))
    writer.writeheader()
    writer.writerow(
        {
            key: json.dumps(value, sort_keys=True) if isinstance(value, (dict, list)) else value
            for key, value in cpu_summary.items()
        }
    )
    data = stream.getvalue().encode()
    files["filesystem-io-resource-comparisons.csv"] = dict(
        data=data, kind="whole-trial-resources", rows=1, sha256=hashlib.sha256(data).hexdigest()
    )
    cli = args.cli.resolve()
    cohorts.append(cli_cohort(cli))
    for source, target in [
        ("timing-summary.csv", "cli"),
        ("timing-comparisons.csv", "cli-comparisons"),
        ("diagnostic-counters.csv", "counters"),
    ]:
        add(cli / source, f"filesystem-io-{target}.csv", "ordinary-cli-anchor")
    if args.read_diagnostic:
        diagnostic, analysis, summary, provenance = read_cohort(args.read_diagnostic, True)
        for source, target in [
            ("read_requests.csv", "read-counts"),
            ("read_request_summary.csv", "read-count-summary"),
            ("read_request_rounds.csv", "read-count-rounds"),
            ("read_request_round_summary.csv", "read-count-round-summary"),
        ]:
            add(analysis / source, f"filesystem-io-{target}.csv", "read-diagnostic")
        for name in summary["plots"]:
            add(
                analysis / name,
                name if name not in files else "read-diagnostic-" + name,
                "read-diagnostic",
            )
        cohorts.append(provenance)
    referenced_figures = referenced_svg_paths(REPO)
    manifest = dict(
        schema=1,
        benchmark_ids=["B-FS-ENG", "B-FS-DIAG"],
        publisher_sha256=sha(__file__),
        resource_analysis_sha256=sha(Path(__file__).with_name("curves_analysis.py")),
        cohorts=cohorts,
        separation="Each cohort remains independent. SDK microcurves are not ordinary CLI Ready or seven-tool performance. No cross-batch ratios multiplied.",
        files={
            name: {k: v for k, v in spec.items() if k in ("kind", "rows", "sha256")}
            for name, spec in files.items()
        },
        public_outputs={
            str(Path("docs/overrides/assets/benchmarks/filesystem-io") / name): spec["sha256"]
            for name, spec in files.items()
            if name.endswith(".svg") and f"filesystem-io/{name}" in referenced_figures
        },
        precision="CSV bytes archived unchanged; only SVGs public; local analysis PDFs retained and verified; no raw_rows.csv publication or numeric formatting",
        graph_methods="Frozen analysis source hashes and per-cohort statistical scope retained; N5 exploration and independent N30 confirmation remain separate",
    )
    encoded = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    require(not PRIVATE_PATH.search(encoded), "private path in public provenance")
    return files, manifest, encoded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("shape", "parameters", "confirm", "cli"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--read-cache", type=Path)
    parser.add_argument("--read-diagnostic", type=Path)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument("--publish", action="store_true")
    parser.add_argument("--output", type=Path, help="private publication receipt path")
    args = parser.parse_args()
    if args.output:
        require(
            args.output.resolve().is_relative_to(args.shape.resolve() / "publication"),
            "private receipt belongs under the primary batch publication directory",
        )
    files, manifest, encoded = collect(args)
    archives = archive_targets(
        args.shape, "filesystem-io", {name: spec["data"] for name, spec in files.items()}, encoded
    )
    preflight_archive(archives)
    targets = []
    for name, spec in files.items():
        if (
            str(Path("docs/overrides/assets/benchmarks/filesystem-io") / name)
            not in manifest["public_outputs"]
        ):
            continue
        destinations = [REPO / "docs/overrides/assets/benchmarks/filesystem-io" / name]
        for target in destinations:
            if args.publish:
                target.parent.mkdir(parents=True, exist_ok=True)
                # A different prior publication must be reviewed explicitly,
                # rather than silently replacing its source under the same URL.
                require(
                    not target.exists() or target.read_bytes() == spec["data"],
                    f"publication would replace different bytes: {target.name}",
                )
                target.write_bytes(spec["data"])
                require(sha(target) == spec["sha256"], "copied output hash differs")
            targets.append(str(target.relative_to(REPO)))
    if args.publish:
        write_archive(archives)
    result = dict(
        state="published" if args.publish else "checked-not-published",
        files=len(files),
        csv_files=sum(name.endswith(".csv") for name in files),
        figures=sum(not name.endswith(".csv") for name in files),
        provenance_sha256=hashlib.sha256(encoded).hexdigest(),
        outputs=targets,
        archive_directory=str(args.shape.resolve() / "publication/filesystem-io"),
        archive_files=len(archives),
        manifest=manifest,
    )
    if args.output:
        receipt = {args.output.resolve(): (json.dumps(result, indent=2) + "\n").encode()}
        preflight_archive(receipt)
        if args.publish:
            write_archive(receipt)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
