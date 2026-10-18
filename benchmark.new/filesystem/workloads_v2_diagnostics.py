#!/usr/bin/env python3
"""B-FS-DIAG: whole-environment N1 counters, separate from V2 timing samples."""

import argparse
import csv
import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import curves_diagnostics as DIAG  # noqa: E402 - import after module-loading setup


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def collect(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    records = json.loads((batch / "records.json").read_text())
    if protocol["benchmark_id"] != "B-FS-DIAG" or protocol["samples"] != 1:
        raise ValueError("expected independent N1 diagnostic batch")
    expected = {
        (case, arm, 0)
        for case in protocol["inventory"]["selected_cell_ids"]
        for arm in ("baseline", "candidate")
    }
    observed, counters, summary = set(), [], []
    for record in records:
        key = (record["case_id"], record["condition"], record["pair"])
        if (
            key not in expected
            or key in observed
            or not record["valid"]
            or not record["host_verified"]
        ):
            raise ValueError("missing/failed/duplicate diagnostic environment")
        observed.add(key)
        trial = batch / "trials" / f"{key[0]}-{key[2]:03}-{key[1]}"
        profiles = DIAG.collect_profiles((trial / "stderr.log").read_text().splitlines())
        if profiles != record["profiles"]:
            raise ValueError("record counters differ from raw registered/final profiles")
        by_component = {p["component"]: p["measurements"] for p in profiles}
        actual = by_component["virtio-fs-overlay"]["guest_writeback_enabled"]["units"]
        if actual != int(key[1] == "candidate"):
            raise ValueError("actual guest WB differs")
        for profile in profiles:
            for label, measurement in profile["measurements"].items():
                counters.append(
                    dict(
                        batch=batch.name,
                        case_id=key[0],
                        condition=key[1],
                        pair=key[2],
                        component=profile["component"],
                        label=label,
                        calls=measurement["calls"],
                        units=measurement["units"],
                        scope="whole environment, three rounds plus setup/validation/exit",
                    )
                )
        protocol_counts = by_component["virtio-fs-protocol"]

        def value(label, field="calls"):
            return protocol_counts.get(label, {}).get(field)  # Unemitted labels remain null.

        count, payload = value("WRITE"), value("write_payload_bytes", "units")
        if not isinstance(count, int) or count <= 0 or not isinstance(payload, int) or payload <= 0:
            raise ValueError("missing positive WRITE counters")
        summary.append(
            dict(
                batch=batch.name,
                case_id=key[0],
                condition=key[1],
                pair=0,
                guest_writeback_enabled=actual,
                WRITE=count,
                write_payload_bytes=payload,
                mean_write_payload_bytes=payload / count,
                logical_rust_write_bytes=sum(r["bytes_written"] for r in record["rows"]),
                logical_counter_scope="explicit Rust calls only; subprocess writes excluded",
                **{
                    label: value(label)
                    for label in (
                        "OPEN",
                        "CREATE",
                        "READ",
                        "LOOKUP",
                        "GETATTR",
                        "FSYNC",
                        "FSYNCDIR",
                        "FLUSH",
                        "RELEASE",
                    )
                },
                layer_parent_stat_attempts=by_component["overlay-core"]
                .get("layer_parent_stats", {})
                .get("units"),
                layer_leaf_stat_attempts=by_component["overlay-core"]
                .get("layer_leaf_stats", {})
                .get("units"),
                backing_lookup_spans=by_component["virtio-fs-overlay"]
                .get("backing_lookup", {})
                .get("calls"),
                scope="whole environment, not isolated task clock or physical disk I/O",
            )
        )
    if observed != expected:
        raise ValueError("diagnostic inventory incomplete")
    return counters, summary


def write_csv(path, rows):
    with path.open("x", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    batch = args.batch.resolve()
    counters, summary = collect(batch)
    output = args.output or batch / "counter-analysis"
    output.mkdir(parents=True, exist_ok=False)
    write_csv(output / "counters.csv", counters)
    write_csv(output / "summary.csv", summary)
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    receipt = dict(
        benchmark_id="B-FS-DIAG",
        environments=len(summary),
        samples_per_case_arm=1,
        independent_from_timing=True,
        script_sha256=sha(Path(__file__)),
        protocol_sha256=sha(batch / "protocol.json"),
        records_sha256=sha(batch / "records.json"),
        output_sha256={p.name: sha(p) for p in output.iterdir() if p.is_file()},
        scope="registered complete owners; case lifetime includes setup/validation/exit; absent labels are unknown, no phase attribution",
    )
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(dict(environments=len(summary), output=str(output))))


if __name__ == "__main__":
    main()
