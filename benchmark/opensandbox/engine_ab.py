#!/usr/bin/env python3
"""Benchmark: B-SANDBOX-ENGINE-ENG, role engineering A/B.
Motivation: verify whether native guest execution improves SDK startup/commands.
Conclusion sought: paired old/upstream vs new/native median differences and CIs.
Design: pinned prepared Ubuntu/pool, 1 CPU/512 MiB whole-tree cap, 3 warmup and
20 seeded randomized pairs per SDK; first and steady requests from fresh VMs.
Retain failures, verify outputs/private writes/quotas/identity/deletion, no exclusions.
"""

import argparse
import asyncio
import hashlib
import importlib.metadata
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import time
import traceback
from datetime import timedelta
from pathlib import Path

KEY = "pvisor-engine-ab-test-key-at-least-32-bytes"
COMMAND = (
    'test ! -e /tmp/engine-ab && printf %s "$PERF_VALUE" > /tmp/engine-ab && cat /tmp/engine-ab'
)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def verify_runtime(request, sandbox_id):
    identity = json.loads((Path(request["state"]) / sandbox_id / "identity.json").read_text())
    assert identity["image"].get("native_guest", False) == (request["arm"] == "after")
    assert identity["spec"]["cpu_millis"] == 1000
    assert identity["spec"]["memory_bytes"] == 512 * 1024 * 1024
    group = Path(identity["cgroup"])
    cpu = (group / "cpu.max").read_text().strip()
    memory = (group / "memory.max").read_text().strip()
    assert cpu == "100000 100000" and memory == "536870912"
    return {"cpu_max": cpu, "memory_max": memory, "generation": identity["generation"]}


async def opensandbox_trial(request):
    import httpx
    from opensandbox.config import ConnectionConfig
    from opensandbox.sandbox import Sandbox

    config = ConnectionConfig(
        domain=request["domain"], protocol="http", api_key=KEY, use_server_proxy=True
    ).with_transport_if_missing()
    row = {}
    sandbox = None
    try:
        begin = time.perf_counter_ns()
        sandbox = await Sandbox.create(
            request["image"],
            connection_config=config,
            resource={"cpu": "1", "memory": "512Mi"},
            entrypoint=["/bin/sleep", "infinity"],
            env={"PERF_VALUE": "verified"},
            ready_timeout=timedelta(seconds=120),
        )
        created = time.perf_counter_ns()
        first = await sandbox.commands.run(COMMAND)
        finished = time.perf_counter_ns()
        second_begin = time.perf_counter_ns()
        second = await sandbox.commands.run("printf repeat")
        second_end = time.perf_counter_ns()
        row.update(
            sandbox_id=sandbox.id,
            create_ms=(created - begin) / 1e6,
            first_command_ms=(finished - created) / 1e6,
            create_to_first_command_ms=(finished - begin) / 1e6,
            second_command_ms=(second_end - second_begin) / 1e6,
        )
        assert first.exit_code == second.exit_code == 0
        assert "".join(item.text for item in first.logs.stdout) == "verified"
        assert "".join(item.text for item in second.logs.stdout) == "repeat"
        assert await sandbox.files.read_file("/tmp/engine-ab") == "verified"
        row.update(verify_runtime(request, sandbox.id))
        row["startup_us"] = json.loads(
            (Path(request["state"]) / sandbox.id / "startup.json").read_text()
        )
        row["success"] = True
    except BaseException as error:
        row.update(success=False, error=str(error), traceback=traceback.format_exc())
    finally:
        if sandbox:
            try:
                await sandbox.kill()
                async with httpx.AsyncClient(
                    base_url="http://" + request["domain"],
                    headers={"OPEN-SANDBOX-API-KEY": KEY},
                    trust_env=False,
                ) as client:
                    assert (await client.get("/v1/sandboxes/" + sandbox.id)).status_code == 404
                row["delete_confirmed"] = True
            except BaseException as error:
                row.update(success=False, cleanup_error=str(error))
            await sandbox.close()
        await config.close_transport_if_owned()
    return row


def e2b_trial(request):
    import httpx
    from e2b import Sandbox

    sandbox = None
    row = {}
    try:
        begin = time.perf_counter_ns()
        sandbox = Sandbox.create(
            request["image"],
            timeout=600,
            envs={"PERF_VALUE": "verified"},
            api_key=KEY,
            api_url="http://" + request["domain"],
            sandbox_url="http://" + request["domain"] + "/e2b",
        )
        created = time.perf_counter_ns()
        first = sandbox.commands.run(COMMAND)
        finished = time.perf_counter_ns()
        second_begin = time.perf_counter_ns()
        second = sandbox.commands.run("printf repeat")
        second_end = time.perf_counter_ns()
        row.update(
            sandbox_id=sandbox.sandbox_id,
            create_ms=(created - begin) / 1e6,
            first_command_ms=(finished - created) / 1e6,
            create_to_first_command_ms=(finished - begin) / 1e6,
            second_command_ms=(second_end - second_begin) / 1e6,
        )
        assert first.exit_code == second.exit_code == 0
        assert first.stdout == "verified" and second.stdout == "repeat"
        assert sandbox.files.read("/tmp/engine-ab") == "verified"
        row.update(verify_runtime(request, sandbox.sandbox_id))
        row["startup_us"] = json.loads(
            (Path(request["state"]) / sandbox.sandbox_id / "startup.json").read_text()
        )
        row["success"] = True
    except BaseException as error:
        row.update(success=False, error=str(error), traceback=traceback.format_exc())
    finally:
        if sandbox:
            try:
                sandbox.kill()
                with httpx.Client(
                    base_url="http://" + request["domain"],
                    headers={"X-API-Key": KEY},
                    trust_env=False,
                ) as client:
                    assert client.get("/sandboxes/" + sandbox.sandbox_id).status_code == 404
                row["delete_confirmed"] = True
            except BaseException as error:
                row.update(success=False, cleanup_error=str(error))
    return row


def worker(protocol):
    package, version = ("opensandbox", "1.1.0") if protocol == "opensandbox" else ("e2b", "2.59.0")
    assert importlib.metadata.version(package) == version
    if protocol == "opensandbox":
        from opensandbox.sandbox import Sandbox  # noqa: F401
    else:
        from e2b import Sandbox  # noqa: F401
    print(json.dumps({"ready": True, "sdk": version}), flush=True)
    for line in sys.stdin:
        request = json.loads(line)
        result = (
            asyncio.run(opensandbox_trial(request))
            if protocol == "opensandbox"
            else e2b_trial(request)
        )
        print(json.dumps(result), flush=True)


def host_state():
    processes = subprocess.run(
        ["ps", "-eo", "pid,comm,pcpu,psr", "--sort=-pcpu"],
        text=True,
        capture_output=True,
        check=True,
    ).stdout.splitlines()[:25]
    return {
        "loadavg": Path("/proc/loadavg").read_text().strip(),
        "processes": processes,
        "affinity": sorted(os.sched_getaffinity(0)),
    }


def bootstrap(pairs, seed):
    rng = random.Random(seed)
    before = [p[0] for p in pairs]
    after = [p[1] for p in pairs]
    b, a = statistics.median(before), statistics.median(after)
    differences = []
    percentages = []
    for _ in range(10000):
        sample = [pairs[rng.randrange(len(pairs))] for _ in pairs]
        x = statistics.median(p[0] for p in sample)
        y = statistics.median(p[1] for p in sample)
        differences.append(y - x)
        percentages.append((y / x - 1) * 100)
    differences.sort()
    percentages.sort()
    return {
        "n_pairs": len(pairs),
        "before_median_ms": b,
        "after_median_ms": a,
        "difference_ms": a - b,
        "change_percent": (a / b - 1) * 100,
        "difference_bootstrap_95_ms": [differences[250], differences[9749]],
        "change_bootstrap_95_percent": [percentages[250], percentages[9749]],
        "before_sorted_ms": sorted(before),
        "after_sorted_ms": sorted(after),
    }


def main(args):
    import httpx

    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    question = (
        "B-SANDBOX-ENGINE-ENG: Does the complete native-guest implementation improve "
        "first-after-API-restart / steady SDK create, first/second command, and same-trial "
        "create-to-first-correct-command versus the frozen pre-change upstream-service binary? "
        "Report per-SDK paired medians/differences/95% CIs; no exclusions, no Docker or pure language claim.\n"
    )
    (output / "QUESTION.txt").write_text(question)
    scope = Path("/sys/fs/cgroup") / Path(
        next(
            line.split(":", 2)[2]
            for line in Path("/proc/self/cgroup").read_text().splitlines()
            if line.startswith("0::")
        )
    ).relative_to("/")
    coordinator = scope / "coordinator"
    coordinator.mkdir()
    (coordinator / "cgroup.procs").write_text(str(os.getpid()))
    (scope / "cgroup.subtree_control").write_text("+cpu +memory +pids")
    os.environ["OPEN_SANDBOX_API_KEY"] = KEY
    os.environ["E2B_API_KEY"] = KEY
    state = Path("/tmp/pveab-" + str(os.getpid()))
    rng = random.Random(args.seed)
    plan = []
    for stage, count in [("warmup", 3), ("formal", args.pairs)]:
        for block in range(count):
            protocols = ["opensandbox", "e2b"]
            rng.shuffle(protocols)
            for protocol in protocols:
                arms = ["before", "after"]
                rng.shuffle(arms)
                plan.append({"stage": stage, "block": block, "protocol": protocol, "arms": arms})
    (output / "order.json").write_text(json.dumps(plan, indent=2))
    shutil.copy2(__file__, output / "harness.py")
    binaries = {}
    for arm, path in [("before", args.before), ("after", args.after)]:
        target = output / ("daemon-" + arm)
        shutil.copy2(path, target)
        binaries[arm] = target
    templates = {
        "opensandbox": {"before": "ubuntu", "after": "native-ubuntu"},
        "e2b": {"before": "e2b-base", "after": "native-e2b"},
    }
    receipt = {
        "benchmark": "B-SANDBOX-ENGINE-ENG",
        "question": question,
        "seed": args.seed,
        "kernel": list(os.uname()),
        "host_before": host_state(),
        "exclusion_rule": "none",
        "binaries": {
            arm: {"path": str(path), "sha256": digest(path)} for arm, path in binaries.items()
        },
        "templates": {},
        "samples": [],
        "api_inventories": [],
    }
    for protocol, arms in templates.items():
        for arm, image in arms.items():
            path = Path(args.images_dir) / (image + ".json")
            manifest = json.loads(path.read_text())
            assert manifest.get("native_guest", False) == (arm == "after")
            receipt["templates"][image] = {
                "sha256": digest(path),
                "cache": manifest["pooled_image"]["cache"],
            }
            shutil.copy2(path, output / (image + ".json"))

    def save():
        (output / "samples.json").write_text(json.dumps(receipt, indent=2))

    save()
    workers = {}
    server = None
    domain = None
    try:
        for protocol, python in [
            ("opensandbox", args.opensandbox_python),
            ("e2b", args.e2b_python),
        ]:
            log = (output / (protocol + "-worker.log")).open("w")
            process = subprocess.Popen(
                [python, str(output / "harness.py"), "--worker", protocol],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=log,
                text=True,
                bufsize=1,
            )
            assert json.loads(process.stdout.readline())["ready"]
            workers[protocol] = process
        for item in plan:
            for arm in item["arms"]:
                protocol = item["protocol"]
                log_path = output / (f"{item['stage']}-{item['block']}-{protocol}-{arm}.log")
                server_state = state / (protocol + "-" + arm)
                with log_path.open("w") as log:
                    server = subprocess.Popen(
                        [
                            str(binaries[arm]),
                            "serve",
                            "--sandbox",
                            "--images-dir",
                            args.images_dir,
                            "--cgroup-root",
                            str(scope),
                            "--state",
                            str(server_state),
                            "--listen",
                            "127.0.0.1:0",
                        ],
                        stdout=log,
                        stderr=log,
                    )
                domain = None
                for _ in range(1000):
                    assert server.poll() is None, log_path.read_text()
                    import re

                    match = re.search(r"listening on (127\.0\.0\.1:\d+)", log_path.read_text())
                    if match:
                        domain = match.group(1)
                        break
                    time.sleep(0.01)
                assert domain, "API startup timeout"
                for phase in ["first", "steady"]:
                    request = {
                        **item,
                        "arm": arm,
                        "phase": phase,
                        "domain": domain,
                        "image": templates[protocol][arm],
                        "state": str(server_state),
                    }
                    process = workers[protocol]
                    process.stdin.write(json.dumps(request) + "\n")
                    process.stdin.flush()
                    line = process.stdout.readline()
                    assert line, "SDK worker exited"
                    row = {
                        **{k: request[k] for k in ["stage", "block", "protocol", "arm", "phase"]},
                        **json.loads(line),
                    }
                    receipt["samples"].append(row)
                    save()
                    print(
                        json.dumps(
                            {
                                k: row.get(k)
                                for k in [
                                    "stage",
                                    "block",
                                    "protocol",
                                    "arm",
                                    "phase",
                                    "create_ms",
                                    "first_command_ms",
                                    "success",
                                    "error",
                                ]
                            }
                        ),
                        flush=True,
                    )
                    assert row["success"] and row["delete_confirmed"], "failed sample retained"
                with httpx.Client(
                    base_url="http://" + domain,
                    headers={"OPEN-SANDBOX-API-KEY": KEY},
                    trust_env=False,
                    timeout=120,
                ) as client:
                    assert client.get("/v1/sandboxes").json()["items"] == []
                receipt["api_inventories"].append({**item, "arm": arm, "empty": True})
                server.terminate()
                server.wait(timeout=20)
                server = None
        formal = [row for row in receipt["samples"] if row["stage"] == "formal"]
        assert len(formal) == args.pairs * 8
        assert len({row["sandbox_id"] for row in receipt["samples"]}) == len(receipt["samples"])
        summary = {}
        for protocol in templates:
            summary[protocol] = {}
            for phase in ["first", "steady"]:
                summary[protocol][phase] = {}
                for key in [
                    "create_ms",
                    "first_command_ms",
                    "second_command_ms",
                    "create_to_first_command_ms",
                ]:
                    pairs = []
                    for block in range(args.pairs):
                        rows = {
                            row["arm"]: row
                            for row in formal
                            if row["protocol"] == protocol
                            and row["phase"] == phase
                            and row["block"] == block
                        }
                        pairs.append((rows["before"][key], rows["after"][key]))
                    summary[protocol][phase][key] = bootstrap(pairs, args.seed)
        receipt.update(
            success=True,
            cleanup_confirmed=not list(scope.glob("pvisor-sb-*")),
            host_after=host_state(),
        )
        assert receipt["cleanup_confirmed"]
        (output / "summary.json").write_text(json.dumps(summary, indent=2))
        save()
        print(
            json.dumps(
                {
                    p: {
                        phase: {
                            k: {
                                x: v[x]
                                for x in [
                                    "before_median_ms",
                                    "after_median_ms",
                                    "change_percent",
                                    "difference_bootstrap_95_ms",
                                ]
                            }
                            for k, v in metrics.items()
                        }
                        for phase, metrics in phases.items()
                    }
                    for p, phases in summary.items()
                },
                indent=2,
            ),
            flush=True,
        )
    except BaseException as error:
        receipt.update(success=False, error=str(error), traceback=traceback.format_exc())
        save()
        raise
    finally:
        if server is not None and server.poll() is None:
            if domain:
                with httpx.Client(
                    base_url="http://" + domain,
                    headers={"OPEN-SANDBOX-API-KEY": KEY},
                    trust_env=False,
                    timeout=120,
                ) as client:
                    for row in client.get("/v1/sandboxes").json()["items"]:
                        assert client.delete("/v1/sandboxes/" + row["id"]).status_code == 204
            server.terminate()
            server.wait(timeout=20)
        for process in workers.values():
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
        save()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--worker":
        worker(sys.argv[2])
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        for name in ["output", "before", "after", "images-dir", "opensandbox-python", "e2b-python"]:
            parser.add_argument("--" + name, required=True)
        parser.add_argument("--pairs", type=int, default=20)
        parser.add_argument("--seed", type=int, default=20261024)
        main(parser.parse_args())
