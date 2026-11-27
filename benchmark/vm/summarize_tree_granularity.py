"""Read one completed B-VM-GUEST-ENG granularity batch; never run or time a VM."""

import json
import random
import sys
from pathlib import Path
from statistics import median


def measured_blocks(batch):
    sample = json.loads(
        (batch / "criterion/native-tree/producer-write-size/new/sample.json").read_text()
    )
    measured = sum(sample["iters"])
    total = json.loads((batch / "raw/complete.json").read_text())["blocks"]
    if not 0 < measured <= total or int(measured) != measured:
        raise ValueError("incomplete Criterion cohort")
    rows = [
        json.loads((batch / f"raw/block-{i:04}/block.json").read_text())
        for i in range(total - int(measured) + 1, total + 1)
    ]
    blocks = [{(r["write_kib"], r["arm"]): r for r in block} for block in rows]
    cells = {(size, arm) for size in (1, 16, 256) for arm in ("shared", "local")}
    if any(len(row) != len(cells) or set(b) != cells for row, b in zip(rows, blocks)):
        raise ValueError("incomplete paired block")
    return blocks, total - len(blocks)


def summarize(batch):
    blocks, warmup = measured_blocks(batch)
    print(f"{len(blocks)} measured blocks; {warmup} warm-up blocks excluded")
    print("write_KiB clock shared_ms local_ms paired_delta_ms [95% interval]")
    for size in (1, 16, 256):
        for clock in ("end_to_end_ns", "guest_publish_ns"):
            shared, local = (
                [b[size, arm][clock] / 1e6 for b in blocks] for arm in ("shared", "local")
            )
            diff = [b - a for a, b in zip(shared, local)]
            rng = random.Random(21316)
            boot = sorted(median(rng.choices(diff, k=len(diff))) for _ in range(20_000))
            print(
                f"{size} {clock} {median(shared):.2f} {median(local):.2f} "
                f"{median(diff):+.2f} [{boot[499]:+.2f}, {boot[19499]:+.2f}]"
            )


if __name__ == "__main__":
    summarize(Path(sys.argv[1]))
