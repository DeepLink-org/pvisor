"""Evidence migrations must preserve values and detect damaged records."""

import csv
import hashlib
import json
import math

import pytest
from evidence_tsv import check, copy_evidence, load, migrate, retire, semantic_sha256, write


def test_nested_evidence_round_trip_and_one_physical_line_per_field(tmp_path):
    evidence = {
        "": {"0": None, "01": {}, "~/": []},
        "samples": [{"elapsed_ms": -0.0, "passed": True}, {"elapsed_ms": 1.2345678901234567}],
        'log\t\n"': '中文\t\r\nquote: "; literal \\n; escape \x1b; slash /',
        "numbers": [2**90, 1, 1.0, False, None],
        "trailing spaces ": "retain three spaces   ",
        "empty strings": ["", '""'],
    }
    path = tmp_path / "report.tsv"
    count = write(path, evidence)
    restored = load(path)
    assert restored == evidence
    assert type(restored["numbers"][1]) is int
    assert type(restored["numbers"][2]) is float
    assert math.copysign(1, restored["samples"][0]["elapsed_ms"]) == -1
    assert len(path.read_text().splitlines()) == count + 1
    assert all(not line.endswith((" ", "\t")) for line in path.read_text().splitlines())
    with path.open(newline="") as stream:
        assert all(
            len(row) == 3 for row in csv.reader(stream, delimiter="\t", quoting=csv.QUOTE_NONE)
        )


@pytest.mark.parametrize("value", [None, True, 12, -1.5, "", [], {}, [[], {}]])
def test_root_values(tmp_path, value):
    path = tmp_path / "root.tsv"
    write(path, value)
    assert load(path) == value


def test_determinism_and_metric_change_affects_only_one_line(tmp_path):
    first, second = tmp_path / "first.tsv", tmp_path / "second.tsv"
    write(first, {"summary": {"p95": 25.0, "p50": 10.0}, "samples": []})
    write(second, {"samples": [], "summary": {"p50": 10.0, "p95": 25.0}})
    assert first.read_bytes() == second.read_bytes()
    write(second, {"summary": {"p95": 25.0, "p50": 11.0}, "samples": []})
    changed = [
        (a, b)
        for a, b in zip(first.read_text().splitlines(), second.read_text().splitlines())
        if a != b
    ]
    assert changed == [("/summary/p50\tfloat\t10.0", "/summary/p50\tfloat\t11.0")]


@pytest.mark.parametrize(
    "body",
    [
        "",
        "/samples\tarray\t\n",
        "\tobject\t\n\tobject\t\n",
        "\tobject\t\n/x\tfloat\ttrue\n",
        "\tarray\t\n/1\tinteger\t1\n",
        "\tobject\t\n/a/b\tinteger\t1\n",
        "\tobject\t\n/a~2\tinteger\t1\n",
        "\tobject\t\n/x\tstring\tbad\\q\n",
        "\tarray\t[]\n",
        "\tobject\t\n/x\tinteger\t1\n/x\tinteger\t2\n",
    ],
)
def test_rejects_truncated_or_malformed_evidence(tmp_path, body):
    path = tmp_path / "broken.tsv"
    path.write_text("path\ttype\tvalue\n" + body)
    with pytest.raises(ValueError):
        load(path)


def test_migration_preserves_original_identity_and_detects_tampering(tmp_path):
    source = tmp_path / "batch/report.json"
    source.parent.mkdir()
    original = b'{"samples": [{"elapsed_ms": 10.125}], "passed": true}\n'
    source.write_bytes(original)
    assert migrate(tmp_path) == 1
    assert not source.exists()
    assert load(source) == json.loads(original)  # old paths can resolve new evidence
    assert check(tmp_path) == 1
    assert migrate(tmp_path) == 0
    with (tmp_path / "conversion.tsv").open() as stream:
        row = next(csv.DictReader(stream, delimiter="\t"))
    assert row["source_sha256"] == hashlib.sha256(original).hexdigest()
    assert row["semantic_sha256"] == semantic_sha256(json.loads(original))
    target = tmp_path / row["target"]
    target.write_text(target.read_text().replace("10.125", "10.25"))
    with pytest.raises(ValueError, match="hash mismatch"):
        check(tmp_path)


def test_migration_keeps_sources_on_invalid_input_and_refuses_overwrite(tmp_path):
    (tmp_path / "a.json").write_text('{"metric": 1}')
    (tmp_path / "b.json").write_text('{"duplicate": 1, "duplicate": 2}')
    with pytest.raises(ValueError, match="Duplicate JSON"):
        migrate(tmp_path)
    assert (tmp_path / "a.json").exists()
    assert (tmp_path / "b.json").exists()
    # Partial converted output does not authorize overwriting an existing record.
    with pytest.raises(FileExistsError):
        migrate(tmp_path)


def test_migration_appends_a_later_batch_without_changing_prior_hashes(tmp_path):
    (tmp_path / "first.json").write_text('{"p50": 1.25}')
    migrate(tmp_path)
    first = (tmp_path / "first.tsv").read_bytes()
    (tmp_path / "second.json").write_text('{"p50": 2.5}')
    migrate(tmp_path)
    assert (tmp_path / "first.tsv").read_bytes() == first
    assert check(tmp_path) == 2


def test_retirement_retains_exact_original_bytes_and_refuses_changed_backup(tmp_path):
    publication, backup = tmp_path / "published", tmp_path / "originals"
    publication.mkdir()
    source = publication / "report.json"
    original = b'{  "metric": 12.25, "notes": [] }\n'
    source.write_bytes(original)
    migrate(publication, keep_json=True)
    assert source.read_bytes() == original
    backup.mkdir()
    (backup / "report.json").write_bytes(b"different original")
    with pytest.raises(ValueError, match="backup differs"):
        retire(publication, backup)
    assert source.read_bytes() == original
    (backup / "report.json").write_bytes(original)
    assert retire(publication, backup) == 1
    assert not source.exists()
    assert (backup / "report.json").read_bytes() == original
    assert check(publication) == 1


def test_retirement_does_not_remove_unconverted_or_changed_sources(tmp_path):
    publication, backup = tmp_path / "published", tmp_path / "originals"
    publication.mkdir()
    source = publication / "report.json"
    source.write_text('{"metric": 1}')
    migrate(publication, keep_json=True)
    source.write_text('{"metric": 2}')
    with pytest.raises(ValueError, match="changed source"):
        retire(publication, backup)
    assert source.exists()


def test_publication_copy_accepts_tsv_inputs_and_records_replaced_exports(tmp_path):
    source, destination = tmp_path / "input.tsv", tmp_path / "export/report.json"
    destination.parent.mkdir()
    write(source, {"metric": 1.25})
    copy_evidence(source, destination)
    migrate(destination.parent)
    write(source, {"metric": 2.5})
    copy_evidence(source, destination)
    migrate(destination.parent, replace=True)
    assert load(destination) == {"metric": 2.5}
    assert check(destination.parent) == 1


def test_product_summary_accepts_tsv_without_changing_statistics(tmp_path):
    from summarize_product_v1 import summarize

    report = {
        "rows": [
            {
                "suite": "density",
                "workload": "hold",
                "backend": "pvisor-vm",
                "correctness": "passed",
                "wall_ms": value,
            }
            for value in (12.125, 15.5)
        ],
        "capabilities": {
            "z/unsupported": {"state": "failed-preflight", "reason": "fixture"},
            "a/unsupported": {"state": "failed-preflight", "reason": "fixture"},
        },
    }
    source = tmp_path / "report.json"
    source.write_text(json.dumps(report))
    target = tmp_path / "report.tsv"
    write(target, report)
    assert summarize([source]) == summarize([target])
    assert summarize([target])[0]["n"] == 2


@pytest.mark.parametrize("publisher", ["render_reference_baselines", "render_ubuntu_baselines"])
@pytest.mark.parametrize("input_format", ["json", "tsv"])
def test_publishers_keep_runtime_input_and_publish_tsv_only(
    tmp_path, monkeypatch, publisher, input_format
):
    import importlib
    import sys

    module = importlib.import_module(publisher)
    report = {
        "arguments": {"modes": "ready", "backends": "native", "samples": "2"},
        "capabilities": {"ready/native": {"state": "available"}},
        "summary": {},
        "rows": [
            {
                "mode": "ready",
                "backend": "native",
                "trial": i,
                "correctness": "passed",
                "ready_ms": 12.125 + i,
                "result_ms": 15.0,
                "completion_ms": 16.0,
                "prepare_ms": 1.0,
                "peak_tree_rss_kib": 123,
                "memory_scope": "fixture",
                "result": {"worker_ms": 1.0},
            }
            for i in range(2)
        ],
    }
    batch = tmp_path / "batch"
    batch.mkdir()
    source = batch / f"report.{input_format}"
    if input_format == "tsv":
        write(source, report)
    else:
        source.write_text(json.dumps(report))
    assets = tmp_path / "inputs"
    assets.mkdir()
    (assets / "assets.json").write_text('{"schema": "fixture"}')
    output = tmp_path / "publication"
    monkeypatch.setattr(
        module, "figures" if publisher == "render_reference_baselines" else "plot", lambda *_: None
    )
    monkeypatch.setattr(
        sys,
        "argv",
        [publisher, "--report", str(source), "--assets", str(assets), "--output", str(output)],
    )
    module.main()
    assert source.exists()
    assert not list(output.rglob("*.json"))
    assert load(output / "assets.tsv") == {"schema": "fixture"}
    assert load(output / "summary.tsv") == module.summarize(report)
    assert check(output) >= 3
    if publisher == "render_ubuntu_baselines":
        assert load(output / "manifest.tsv")["batches"]["batch"]["report"] == "batch/report.tsv"
