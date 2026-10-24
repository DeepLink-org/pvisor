#!/usr/bin/env python3
"""Benchmark: B-PEER-CHAOS-ENG (benchmark/README.md#b-peer-chaos-eng), diagnostic.

Motivation: verify UDP integrity while full-mesh TCP bulk occupies the packet path.
Conclusion sought: three independent overlap checks, not throughput or reliability.
Design: one fresh four-VM group, same 512MiB/swap0/CPU budget and frozen image;
three 384MiB TCP transfers with 12-direction UDP after each GO; retain failures.
"""

import argparse
import hashlib
import json
import os
import shutil
import threading
import time
from pathlib import Path

from chaos import Chaos
from four_node import REPO, save


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    os.sched_setaffinity(0, {8, 9})
    for name, digest in config["artifact_sha256"].items():
        assert hashlib.sha256(Path(name).read_bytes()).hexdigest() == digest
    source = output / "harness"
    source.mkdir()
    for name in (
        "mixed_chaos.py",
        "chaos.py",
        "four_node.py",
        "udp_worker.rs",
        "four_node_worker.rs",
    ):
        shutil.copy2(REPO / "benchmark/peer" / name, source / name)
    save(
        output / "manifest.json",
        {
            "benchmark": "B-PEER-CHAOS-ENG",
            "mode": "TCP/UDP overlap",
            "rounds": 3,
            "config": config,
            "harness_sha256": {
                p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in source.iterdir()
            },
            "exclusions": "none; first unexpected failure stops the cohort",
        },
    )
    chaos = Chaos(config, output)
    monitor = None
    overlaps = []
    try:
        chaos.group.start()
        chaos.active = dict(enumerate(chaos.group.cgroups))
        for node in range(4):
            chaos.udp_server(node, "initial")
        monitor = threading.Thread(target=chaos.monitor)
        monitor.start()
        for index in range(3):
            directory = output / f"overlap-{index}"

            def concurrent_udp():
                started = time.monotonic()
                chaos.udp(directory / "udp")
                pending = [
                    node
                    for node in range(4)
                    if len(
                        (directory / "bulk-long" / f"client-{node}.stdout").read_text().splitlines()
                    )
                    == 1
                ]
                assert pending, "no evidence that UDP completed while TCP was still in flight"
                record = {
                    "index": index,
                    "udp_seconds": time.monotonic() - started,
                    "tcp_pending_after_udp": pending,
                    "verified": True,
                }
                save(directory / "overlap.json", record)
                overlaps.append(record)

            chaos.scenario(directory, "bulk-long", after_go=concurrent_udp)
        summary = {
            "verified": True,
            "rounds": 3,
            "overlaps": overlaps,
            "failures": 0,
            "excluded": 0,
            "tcp_payload_bytes": sum(r["payload_bytes"] for r in chaos.results),
            "udp_requests": sum(r["requests"] for r in chaos.udp_results),
            "udp_attempts": sum(r["attempts"] for r in chaos.udp_results),
            "udp_timeouts": sum(r["timeouts"] for r in chaos.udp_results),
            "udp_duplicates": sum(r["duplicates"] for r in chaos.udp_results),
        }
    except BaseException as error:
        save(output / "failure.json", {"error": repr(error), "completed_overlaps": overlaps})
        raise
    finally:
        chaos.stop.set()
        if monitor is not None:
            monitor.join(timeout=5)
        chaos.close()
    save(output / "summary.json", summary)
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
