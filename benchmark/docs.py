"""Check dynamic documentation tables and publish audited user benchmark results.

Fixed historical tables live in Markdown. Publication only updates
explicit live bindings, never historical tables or narrative conclusions.
"""

import argparse
import csv
import json
import math
import re
import shlex
from pathlib import Path

from common import METRIC_SCOPE, READY_EXIT_SCOPE, REPO, ROOT, artifact_summary, digest, save
from public_assets import check_public_assets, is_public_csv
from public_assets import csv_path as public_csv_path
from update import _publish_csv, display_table, validate_batch

INVENTORY = ROOT / "docs.json"
MACRO = re.compile(r"\{\{ table\((.*?)\) \}\}")


def article_tables(path):
    # Ignore illustrative fenced macros, like semspec render itself.
    text = re.sub(r"(?ms)^(`{3,}|~{3,})[^\n]*\n.*?^\1[^\n]*$", "", path.read_text())
    return [json.loads("[" + match[1] + "]") for match in MACRO.finditer(text)]


def csv_table(csv_path, names, columns, scope, benchmark_id):
    """Rebuild display cells from each condition's latest successful CSV row."""
    latest = {}
    with Path(csv_path).open() as stream:
        for row in csv.DictReader(stream):
            if (
                row["metric_scope"] != scope
                or row["benchmark_id"] != benchmark_id
                or row["role"] != "user-facing"
            ):
                raise ValueError("public CSV has a different metric or benchmark scope")
            latest[row["condition"]] = row
    table = {}
    for label, condition in names.items():
        source = latest.get(condition)
        values = {}
        for display, field in columns.items():
            if source is None:
                values[display] = None
            elif field == "Batch":
                values[display] = source["batch"]
            elif field == "N":
                values[display] = int(source["N"])
                if values[display] != int(source["runs"]):
                    raise ValueError("public CSV sample count differs from configured runs")
            else:
                value = float(source[field])
                if not math.isfinite(value):
                    raise ValueError("non-finite public CSV statistic")
                values[display] = f"{value:.2f}"
        table[label] = values
    return table


def check_live_tables(inventory):
    from importlib.util import module_from_spec, spec_from_file_location

    spec = spec_from_file_location("startup_public_data", ROOT / "startup/subjects.py")
    startup = module_from_spec(spec)
    spec.loader.exec_module(startup)
    for page in inventory["pages"]:
        base = REPO / "docs/src" / page["locale"]
        for descriptor in page["tables"]:
            if descriptor["kind"] != "published":
                continue
            target = (base / descriptor["json"]).resolve()
            columns = dict(zip(descriptor["columns"], descriptor["columns"]))
            if page["page"] == "benchmarks/startup.md":
                scope = READY_EXIT_SCOPE
                names = {startup.LABELS[name]: name for name in startup.NAMES}
                if page["locale"] == "zh":
                    for display in columns:
                        for zh, en in (
                            ("中位数", "median"),
                            ("均值", "mean"),
                            ("标准差", "SD"),
                            ("最小", "min"),
                            ("最大", "max"),
                        ):
                            columns[display] = columns[display].replace(zh, en)
                csv_path = public_csv_path(REPO, "startup-timing.csv")
                benchmark_id = "B-STARTUP"
            else:
                binding = next(
                    (
                        b
                        for b in inventory["bindings"].values()
                        if b.get("table") == descriptor["json"]
                    ),
                    None,
                )
                if binding is None:
                    raise ValueError("published table has no auditable binding: " + str(target))
                entry = next(e for e, b in inventory["bindings"].items() if b is binding)
                names = binding.get("conditions", {})
                if not names:
                    for label, expression in json.loads((ROOT / entry).read_text()).items():
                        argv = shlex.split(expression[2:])
                        names[label] = argv[argv.index("--command-name") + 1]
                csv_path = public_csv_path(REPO, binding["csv"])
                scope = binding.get("metric_scope", METRIC_SCOPE)
                benchmark_id = binding["benchmark_id"]
            expected = csv_table(csv_path, names, columns, scope, benchmark_id)
            if json.loads(target.read_text()) != expected:
                raise ValueError("public JSON differs from latest audited CSV: " + str(target))


def check_inventory():
    inventory = json.loads(INVENTORY.read_text())
    if inventory["format"] != 1:
        raise ValueError("unsupported documentation inventory")
    paths = set()
    for page in inventory["pages"]:
        path = REPO / "docs/src" / page["locale"] / page["page"]
        if path in paths:
            raise ValueError("duplicate article")
        paths.add(path)
        tables = page["tables"]
        macros = article_tables(path)
        actual = [(path.parent / args[0]).resolve() for args in macros]
        expected = [(REPO / "docs/src" / page["locale"] / t["json"]).resolve() for t in tables]
        if sorted(actual) != sorted(expected) or len(actual) != len(set(actual)):
            raise ValueError(f"table bindings differ: {path}")
        for table, target in zip(tables, expected):
            if table["kind"] not in ("published", "coverage"):
                raise ValueError(f"fixed historical tables belong in Markdown: {target}")
            data = json.loads(target.read_text())
            if not data or len(data) != table["rows"]:
                raise ValueError(f"row inventory differs: {target}")
            columns = table["columns"]
            if any(list(row) != columns for row in data.values()):
                raise ValueError(f"column inventory differs: {target}")
            for row in data.values():
                for value in row.values():
                    if (
                        isinstance(value, (list, dict))
                        or isinstance(value, str)
                        and value.startswith("$ ")
                    ):
                        raise ValueError(f"unresolved/non-scalar public data: {target}")
            args = macros[actual.index(target)]
            if (args[1] if len(args) == 2 else {}) != table["options"]:
                raise ValueError(f"render options differ: {path}")
        for source in page["source_csv"]:
            if not is_public_csv(Path(source).name):
                raise ValueError(f"static or intermediate CSV belongs in local evidence: {source}")
            if not (path.parent / source).is_file():
                raise ValueError(f"missing source CSV: {source}")
    for locale in ("zh", "en"):
        if not set((REPO / f"docs/src/{locale}/benchmarks").rglob("*.md")) <= paths:
            raise ValueError("benchmark article missing from inventory")
    check_public_assets(REPO)
    check_live_tables(inventory)
    return len(paths), sum(len(p["tables"]) for p in inventory["pages"])


def publish_docs(batch):
    inventory = json.loads(INVENTORY.read_text())
    metadata = json.loads((batch / "manifest.json").read_text())
    binding = inventory["bindings"].get(metadata["entry"])
    if (
        binding is None
        or metadata["benchmark_id"] != binding["benchmark_id"]
        or metadata["role"] != "user-facing"
    ):
        raise ValueError("batch has no matching user-facing documentation binding")
    if metadata["metric_scope"] != binding.get("metric_scope", METRIC_SCOPE):
        raise ValueError("measurement scope differs from documentation binding")
    if "fixture_scope" in binding and metadata.get("fixture_scope") != binding["fixture_scope"]:
        raise ValueError("workload configuration differs from documentation binding")
    if binding.get("kind") == "startup":
        import sys

        sys.path.insert(0, str(ROOT / "startup"))
        from publish import publish_startup

        publish_startup(batch, REPO / "docs/src")
        from measurement_coverage import coverage

        coverage()
        return [
            REPO / "docs/src" / locale / f"benchmarks/startup-timing.{metric}.render.json"
            for locale in ("zh", "en")
            for metric in ("ready", "exit")
        ]
    manifest, resolved, results = validate_batch(batch)
    table = {
        name: ({"Batch": manifest["batch"]} if binding.get("show_batch", True) else {}) | row
        for name, row in display_table(resolved).items()
    }
    targets = []
    for locale in ("zh", "en"):
        page = next(
            p for p in inventory["pages"] if p["locale"] == locale and p["page"] == binding["page"]
        )
        target = next(t for t in page["tables"] if t["json"] == binding["table"])
        if (
            target["kind"] != "published"
            or target["columns"] != list(next(iter(table.values())))
            or target["rows"] != len(table)
        ):
            raise ValueError("public table schema differs; create a new binding")
        targets.append((REPO / "docs/src" / locale / target["json"]).resolve())
    targets = list(dict.fromkeys(targets))
    if not is_public_csv(binding["csv"]):
        raise ValueError("public CSV is not in the current download inventory")
    provenance = {
        key: manifest.get(key)
        for key in (
            "batch",
            "benchmark_id",
            "role",
            "entry",
            "recorded_at",
            "runs",
            "warmup",
            "metric_scope",
            "platform",
            "kernel",
            "cpu_affinity",
            "source_commit",
            "source_commit_scope",
            "fixture_scope",
            "measurement_engine",
            "python_version",
            "exports",
        )
    }
    provenance["manifest_sha256"] = digest(batch / "manifest.json")
    provenance["artifacts"] = artifact_summary(manifest["artifacts"])
    provenance["publisher_sha256"] = digest(Path(__file__))
    # Validate both CSV schemas and batch collisions before changing any table.
    csv_paths = [public_csv_path(REPO, binding["csv"])]
    for path in csv_paths:
        _publish_csv(batch, path, manifest, resolved, results, dry_run=True)
    for path in csv_paths:
        _publish_csv(batch, path, manifest, resolved, results)
    provenance_path = (
        REPO / "docs/overrides/assets/benchmarks" / (Path(binding["csv"]).stem + ".provenance.json")
    )
    save(provenance_path, provenance)
    for path in targets:
        save(path, table)
    from measurement_coverage import coverage

    coverage()
    return targets


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="action", required=True)
    sub.add_parser("check")
    pub = sub.add_parser("publish")
    pub.add_argument("batch", type=Path)
    args = p.parse_args()
    try:
        if args.action == "check":
            pages, tables = check_inventory()
            print(f"{pages} articles, {tables} dynamic JSON tables verified")
        else:
            print("\n".join(str(path) for path in publish_docs(args.batch.resolve())))
    except (ValueError, KeyError, OSError) as error:
        p.exit(1, f"{error}\n")
