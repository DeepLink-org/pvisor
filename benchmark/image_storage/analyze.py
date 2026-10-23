#!/usr/bin/env python3
"""Summarize the static inventories from run.py without measuring time."""

import argparse
import csv
import json
import random
import statistics
from pathlib import Path


def interval(values):
    rng = random.Random(20261024)
    medians = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(5000))
    return [medians[125], medians[4874]]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("batch", type=Path)
    args = parser.parse_args()
    root = args.batch
    manifest = json.loads((root / "manifest.json").read_text())
    results = json.loads((root / "results.json").read_text())
    legacy = {r["name"]: r for r in json.loads((root / "legacy-results.json").read_text())}
    labels = [i["name"] for i in manifest["inputs"]] + ["combined"]
    summary = []
    for label in labels:
        rows = [r for r in results if r["label"] == label]
        assert len(rows) == manifest["repeats"] and all(r["status"] == "passed" for r in rows)
        pvisor = [r["pvisor"]["allocated"] for r in rows]
        docker = [r["docker_incremental_allocated"] for r in rows]
        savings = [1 - p / d for p, d in zip(pvisor, docker, strict=True)]
        raw_rootfs = (
            sum(r["rootfs"]["allocated"] for r in legacy.values())
            if label == "combined"
            else legacy[label]["rootfs"]["allocated"]
        )
        summary.append(
            dict(
                image=label,
                repeats=len(rows),
                unpacked_rootfs_allocated=raw_rootfs,
                pvisor_allocated=statistics.median(pvisor),
                pvisor_range=[min(pvisor), max(pvisor)],
                docker_incremental_allocated=statistics.median(docker),
                docker_range=[min(docker), max(docker)],
                docker_empty_allocated=statistics.median(
                    r["docker_empty"]["allocated"] for r in rows
                ),
                docker_content_allocated=statistics.median(
                    r["docker_content"]["allocated"] for r in rows
                ),
                docker_snapshots_allocated=statistics.median(
                    r["docker_snapshots"]["allocated"] for r in rows
                ),
                saved_fraction=statistics.median(savings),
                saved_fraction_bootstrap95=interval(savings),
                saved_vs_unpacked_fraction=1 - statistics.median(pvisor) / raw_rootfs,
                pool_raw_content_bytes=statistics.median(r["pool"]["raw_bytes"] for r in rows),
                pool_encoded_content_bytes=statistics.median(
                    r["pool"]["encoded_bytes"] for r in rows
                ),
                after_one_read_allocated=statistics.median(
                    r["pvisor_after_validation"]["allocated"] for r in rows
                ),
            )
        )
    individual = sum(r["pvisor_allocated"] for r in summary[:-1])
    combined = summary[-1]["pvisor_allocated"]
    report = dict(
        benchmark="B-IMAGE-STORAGE-ENG",
        status="passed",
        rows=summary,
        shared_store_saved_fraction=1 - combined / individual,
        individual_store_sum=individual,
        combined_store=combined,
        units="bytes; display as MiB = bytes/1048576",
        primary_metric="st_blocks*512; unique inodes; full image stores; Docker empty baseline subtracted",
        statistical_scope="three fresh imports of the same exact images; bootstrap interval reflects inventory variation only, not variation across images or workloads",
        limitations=[
            "Btrfs transparent compression disabled in the experiment directory; global filesystem metadata excluded",
            "source-download cache, harness and executable excluded from both stores",
            "image-only inventory before execution; after-read pVisor inventory is also retained",
            "rootless Docker 29, containerd overlayfs; no host Docker images/containers modified",
            "does not measure import peak disk, mmap performance or image deletion garbage collection",
            "pVisor currently retains unreferenced pool objects after rmi; no automatic GC",
        ],
    )
    (root / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    with (root / "summary.csv").open("w", newline="") as stream:
        columns = [
            "image",
            "repeats",
            "unpacked_rootfs_allocated",
            "docker_incremental_allocated",
            "pvisor_allocated",
            "saved_fraction",
            "after_one_read_allocated",
        ]
        writer = csv.DictWriter(stream, fieldnames=columns, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(summary)
    for row in summary:
        print(
            f"{row['image']}: unpacked {row['unpacked_rootfs_allocated'] / 1048576:.2f} MiB; Docker {row['docker_incremental_allocated'] / 1048576:.2f} MiB; pVisor {row['pvisor_allocated'] / 1048576:.2f} MiB; saved {row['saved_fraction']:.2%}"
        )
    print(
        f"Combined versus independent pVisor stores: saved {report['shared_store_saved_fraction']:.2%}"
    )


if __name__ == "__main__":
    main()
