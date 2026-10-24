#!/usr/bin/env python3
"""Benchmark: B-OPENSANDBOX-ENG (benchmark/README.md#b-opensandbox-eng).
Role: engineering A/B.
Motivation: measure official SDK wait for a fresh local usable sandbox.
Conclusion sought: scoped readiness distributions and failures, no full API claim.
Design: prepared Ubuntu/execd, 1 CPU/512 MiB, seeded balanced pairs, real output,
private-file and confirmed-delete checks; retain all failures and slow samples.
"""

import argparse
import asyncio
import importlib.metadata
import json
import os
import random
import time
from datetime import timedelta
from pathlib import Path

import httpx
from opensandbox.config import ConnectionConfig
from opensandbox.sandbox import Sandbox


async def measure(arm, domain, image, cold):
    config = ConnectionConfig(
        domain=domain,
        protocol="http",
        api_key=os.environ["OPEN_SANDBOX_API_KEY"],
        use_server_proxy=True,
    )
    row = {"arm": arm, "image": image, "cold_request": cold, "success": False}
    sandbox = None
    try:
        started = time.perf_counter()
        sandbox = await Sandbox.create(
            image,
            connection_config=config,
            resource={"cpu": "1", "memory": "512Mi"},
            entrypoint=["tail", "-f", "/dev/null"],
            env={"PVOS_BENCH": "sdk-ready"},
            ready_timeout=timedelta(seconds=120),
        )
        row["create_seconds"] = time.perf_counter() - started
        row["sandbox_id"] = sandbox.id
        result = await sandbox.commands.run(
            "test ! -e /tmp/pvisor-sdk-isolation && "
            'printf "%s" "$PVOS_BENCH" > /tmp/pvisor-sdk-isolation && '
            "cat /tmp/pvisor-sdk-isolation"
        )
        row["first_command_seconds"] = time.perf_counter() - started
        assert result.exit_code == 0
        assert "".join(x.text for x in result.logs.stdout) == "sdk-ready"
        assert await sandbox.files.read_file("/tmp/pvisor-sdk-isolation") == "sdk-ready"
        row["success"] = True
    except Exception as error:
        row["error"] = str(error)
    finally:
        if sandbox is not None:
            try:
                await sandbox.kill()
                async with httpx.AsyncClient(
                    base_url=f"http://{domain}", headers={"OPEN-SANDBOX-API-KEY": config.api_key}
                ) as client:
                    response = await client.get(f"/v1/sandboxes/{sandbox.id}")
                    assert response.status_code == 404, "deletion not confirmed"
                row["delete_confirmed"] = True
            except Exception as error:
                row["success"] = False
                row["cleanup_error"] = str(error)
            finally:
                await sandbox.close()
        await config.close_transport_if_owned()
    return row


async def main(args):
    assert importlib.metadata.version("opensandbox") == "1.1.0"
    assert (args.docker_domain is None) == (args.docker_image is None)
    assert args.pairs >= 1
    arms = [("native", args.native_domain, args.native_image)]
    if args.docker_domain:
        arms.append(("docker", args.docker_domain, args.docker_image))
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    assert not output.exists(), "use a fresh evidence path"
    evidence = {
        "benchmark": "B-OPENSANDBOX-ENG",
        "seed": args.seed,
        "sdk": "1.1.0",
        "kernel": list(os.uname()),
        "affinity": sorted(os.sched_getaffinity(0)),
        "cpu": "1",
        "memory": "512Mi",
        "samples": [],
    }
    rng = random.Random(args.seed)
    for pair in range(-1, args.pairs):
        order = arms.copy()
        rng.shuffle(order)
        for arm, domain, image in order:
            row = await measure(arm, domain, image, cold=pair == -1)
            row["pair"] = pair
            evidence["samples"].append(row)
            output.write_text(json.dumps(evidence, indent=2) + "\n")
            print(json.dumps(row), flush=True)
            if not row["success"]:
                raise RuntimeError(
                    "failed sample retained; stop admission and reconcile before another cohort"
                )
    if any(not row["success"] for row in evidence["samples"]):
        raise SystemExit("failed samples retained")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-domain", required=True)
    parser.add_argument("--native-image", default="ubuntu")
    parser.add_argument("--docker-domain")
    parser.add_argument("--docker-image")
    parser.add_argument("--pairs", type=int, default=20)
    parser.add_argument("--seed", type=int, default=20261024)
    parser.add_argument("--output", required=True)
    asyncio.run(main(parser.parse_args()))
