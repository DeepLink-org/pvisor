#!/usr/bin/env python3
"""Benchmark: B-OPENSANDBOX-ENG (benchmark/README.md#b-opensandbox-eng).
Role: engineering A/B.
Motivation: measure the first usable sandbox after each local server restart.
Conclusion sought: first-request distributions, separate from steady creates.
Design: 20 fresh server processes per arm, prepared images, seeded paired order;
exclude server boot, retain every failure, verify output/private files/deletion.
"""

import argparse
import asyncio
import importlib.metadata
import json
import os
import random
import subprocess
import time
from pathlib import Path

import httpx
from run import measure


async def ready(service, domain):
    deadline = time.monotonic() + 30
    async with httpx.AsyncClient(
        base_url=f"http://{domain}",
        headers={"OPEN-SANDBOX-API-KEY": os.environ["OPEN_SANDBOX_API_KEY"]},
        timeout=2,
    ) as client:
        while time.monotonic() < deadline:
            if service.poll() is not None:
                raise RuntimeError("owned service exited before API readiness")
            try:
                response = await client.get("/v1/sandboxes")
                if response.status_code == 200:
                    assert response.json()["items"] == [], "use isolated empty state"
                    return
            except httpx.HTTPError:
                pass
            await asyncio.sleep(0.02)
    raise RuntimeError("owned service startup timed out")


async def main(args):
    assert importlib.metadata.version("opensandbox") == "1.1.0"
    configuration = json.loads(Path(args.configuration).read_text())
    assert set(configuration) == {"native", "docker"}
    assert args.repetitions >= 1
    root = Path(args.output)
    root.mkdir(parents=True, exist_ok=False)
    evidence = {
        "benchmark": "B-OPENSANDBOX-ENG",
        "scenario": "first-after-server-restart",
        "seed": args.seed,
        "configuration": configuration,
        "samples": [],
    }
    rng = random.Random(args.seed)
    for repeat in range(args.repetitions):
        services, logs = {}, []
        try:
            for arm, item in configuration.items():
                log = (root / f"{repeat}-{arm}.log").open("wb")
                logs.append(log)
                services[arm] = subprocess.Popen(
                    item["argv"], stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT
                )
            await asyncio.gather(
                *(ready(services[arm], item["domain"]) for arm, item in configuration.items())
            )
            order = list(configuration)
            rng.shuffle(order)
            for arm in order:
                item = configuration[arm]
                row = await measure(arm, item["domain"], item["image"], cold=True)
                row["pair"] = repeat
                evidence["samples"].append(row)
                (root / "samples.json").write_text(json.dumps(evidence, indent=2) + "\n")
                print(json.dumps(row), flush=True)
                if not row["success"]:
                    raise RuntimeError("failed sample retained; stop and reconcile")
        finally:
            for service in services.values():
                service.terminate()
            for service in services.values():
                try:
                    service.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.wait()
            for log in logs:
                log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--configuration", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--repetitions", type=int, default=20)
    parser.add_argument("--seed", type=int, default=20261024)
    asyncio.run(main(parser.parse_args()))
