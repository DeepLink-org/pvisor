"""B-FS-TOOLS: prepare the current native metadata/read table and its shared fixture."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import ROOT, create_batch, materialize, save, table_template
from fs_timing import FIXTURE_SCOPE, ROWS, RUNS, SCOPE, WARMUP, run_trial
from workload import prepare_fixture, validate_fixture


def validate_template(template):
    table = table_template(template)
    expected = {
        label: f"$ python3 @REPO@/benchmark.new/filesystem/fs_timing.py @BATCH@ {mode}"
        for label, mode in ROWS.items()
    }
    if table != expected or list(table) != list(expected):
        raise ValueError("filesystem table requires the fixed metadata/read timing rows")
    return table


def prepare(template, output=None, row=None):
    table = validate_template(template)
    if row is not None:
        if row not in table:
            raise ValueError("unknown filesystem row")
        table = {row: table[row]}
    batch = create_batch(template, output)
    try:
        prepare_fixture(batch / "fixture")
        validate_fixture(batch / "fixture")
        materialize(
            template,
            batch,
            artifacts=[batch / "fixture/fixture.json"],
            cwd=batch,
            helpers=[
                Path(__file__),
                *[
                    ROOT / "filesystem" / name
                    for name in (
                        "fs_timing.py",
                        "audit.py",
                        "workload.py",
                    )
                ],
            ],
            row_config=dict(
                rows={name: name for name in table}, runs=RUNS, warmup=WARMUP, metric_scope=SCOPE
            ),
        )
        manifest = json.loads((batch / "manifest.json").read_text())
        manifest["fixture_scope"] = FIXTURE_SCOPE
        save(batch / "manifest.json", manifest)
        for label in table:
            run_trial(batch, ROWS[label], "preflight", 0)
        return batch
    except BaseException as error:
        save(batch / "failure.json", dict(error=str(error)))
        if (batch / "manifest.json").exists():
            manifest = json.loads((batch / "manifest.json").read_text())
            manifest.update(status="failed", error=str(error))
            save(batch / "manifest.json", manifest)
        raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("template", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--row")
    args = parser.parse_args()
    try:
        print(prepare(args.template, args.output, args.row))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"{error}\n")
