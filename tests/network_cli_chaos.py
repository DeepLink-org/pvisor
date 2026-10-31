"""Seeded black-box network CLI correctness campaign (no VM required).

Example: python3 tests/network_cli_chaos.py --binary target/debug/pvisor
Each command uses a private store. JSONL evidence records seed, argv, exit,
stdout and stderr; failed assertions stop the campaign and retain its store.
"""

import argparse
import hashlib
import json
import math
import os
import random
import signal
import subprocess
import tempfile
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--seconds", type=float, default=10800)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--interval", type=float, default=0.2, help="Idle seconds between scenarios"
    )
    parser.add_argument("--workload", choices=["generated", "regression"], default="generated")
    parser.add_argument("--workers", type=int, choices=range(2, 5), default=4)
    args = parser.parse_args()
    if not math.isfinite(args.seconds) or args.seconds <= 0:
        parser.error("seconds must be finite and positive")
    if not math.isfinite(args.interval) or args.interval < 0.2:
        parser.error("interval must be finite and at least 0.2 seconds")
    binary = args.binary.resolve(strict=True)
    root = args.output or Path(tempfile.mkdtemp(prefix="pvisor-network-chaos-"))
    root.mkdir(parents=True, exist_ok=True)
    store = root / "store"
    if store.exists():
        parser.error("output must not contain an existing store")
    seed = args.seed if args.seed is not None else random.SystemRandom().getrandbits(64)
    rng = random.Random(seed)
    env = dict(os.environ, PVISOR_DOCKER_HOME=str(store.resolve()))
    commands = 0
    counts = {}
    started = time.monotonic()
    (root / "campaign.json").write_text(
        json.dumps(
            dict(
                seed=seed,
                binary=str(binary),
                seconds=args.seconds,
                interval=args.interval,
                workers=args.workers,
                workload=args.workload,
                unix=time.time(),
                binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
            )
        )
    )

    def raw(argv, kill_delay=None):
        command_started = time.monotonic()
        proc = subprocess.Popen(
            [str(binary), "network", *argv],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        if kill_delay is not None:
            time.sleep(kill_delay)
            if proc.poll() is None:
                proc.kill()
        try:
            out, err = proc.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            diagnostics = {}
            paths = [
                Path("/proc/loadavg"),
                Path("/proc/meminfo"),
                Path("/proc/pressure/cpu"),
                Path("/proc/pressure/io"),
                Path("/proc/pressure/memory"),
            ]
            process = Path(f"/proc/{proc.pid}")
            paths.extend(process / name for name in ["status", "stat", "wchan", "syscall", "stack"])
            try:
                paths.extend(process.glob("task/*/wchan"))
            except OSError:
                pass
            for path in paths:
                try:
                    diagnostics[str(path)] = path.read_text()
                except OSError as error:
                    diagnostics[str(path)] = str(error)
            proc.kill()
            out, err = proc.communicate()
            evidence = dict(
                argv=argv,
                pid=proc.pid,
                elapsed=time.monotonic() - command_started,
                stdout=out.decode(errors="replace"),
                stderr=err.decode(errors="replace"),
                diagnostics=diagnostics,
                unix=time.time(),
            )
            (root / f"timeout-{proc.pid}.json").write_text(json.dumps(evidence))
            raise AssertionError(
                f"CLI hung: {argv}; diagnostics: timeout-{proc.pid}.json"
            ) from None
        return dict(
            argv=argv,
            code=proc.returncode,
            stdout=out.decode(errors="replace"),
            stderr=err.decode(errors="replace"),
            kill_delay=kill_delay,
            elapsed=time.monotonic() - command_started,
        )

    def record(result):
        nonlocal commands
        commands += 1
        with (root / "commands.jsonl").open("a") as log:
            log.write(json.dumps(result) + "\n")
        assert "panicked at" not in result["stderr"], result
        return result

    def call(*argv, success=True):
        result = record(raw(list(argv)))
        assert result["code"] in [0, 1, 2], result
        assert (result["code"] == 0) == success, result
        if success:
            assert result["stderr"] == "", result
        return result["stdout"]

    def snapshot():
        rows = call("ls", "--no-trunc").splitlines()[1:]
        names = [row.split("\t")[1] for row in rows]
        values = json.loads(call("inspect", *names))
        assert len(values) == len(names) and len(set(names)) == len(names)
        assert {"bridge", "none"} <= set(names)
        ids = [v["Id"] for v in values]
        assert len(set(ids)) == len(ids)
        assert set(call("ls", "-q", "--no-trunc").splitlines()) == set(ids)
        subnets = [v["IPAM"]["Config"][0]["Subnet"] for v in values if v["Driver"] == "bridge"]
        assert len(set(subnets)) == len(subnets)
        return {v["Name"]: v for v in values}

    def invalid():
        before = snapshot()
        argv = rng.choice(
            [
                [
                    "create",
                    rng.choice(
                        ["bridge", "none", "host", "../escape", "", "x" * 129, "空格", "a b"]
                    ),
                ],
                [
                    "create",
                    "--subnet",
                    rng.choice(["10.0.0.1/24", "10.0.0.0/16", "8.8.8.0/24", "::/64", "garbage"]),
                    "invalid",
                ],
                [
                    "create",
                    "--subnet",
                    "10.0.0.0/24",
                    "--gateway",
                    rng.choice(["10.0.0.0", "10.0.0.255", "10.0.1.1"]),
                    "invalid",
                ],
                ["rm", "-f", rng.choice(["bridge", "none"])],
                ["inspect", "missing"],
            ]
        )
        call(*argv, success=False)
        assert snapshot() == before

    def lifecycle():
        name = "net-" + str(rng.getrandbits(64))
        cidr = f"10.{rng.randrange(256)}.{rng.randrange(256)}.0/24"
        gateway = cidr.split("/")[0].rsplit(".", 1)[0] + "." + str(rng.randrange(1, 255))
        identity = call("create", "--subnet", cidr, "--gateway", gateway, name).strip()
        value = json.loads(call("inspect", rng.choice([name, identity, identity[:12]])))[0]
        assert value["Name"] == name and value["Id"] == identity and value["Containers"] == {}
        assert value["IPAM"]["Config"] == [{"Subnet": cidr, "Gateway": gateway}]
        call("create", name, success=False)
        call("create", "--subnet", cidr, name + "-overlap", success=False)
        assert snapshot()[name] == value
        call("rm", rng.choice([name, identity, identity[:12]]))
        call("inspect", name, success=False)
        call("rm", "-f", name)
        assert name not in snapshot()

    def race():
        name = "race-" + str(rng.getrandbits(64))
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            results = list(pool.map(lambda _: raw(["create", name]), range(args.workers)))
        for result in results:
            record(result)
        assert sum(r["code"] == 0 for r in results) == 1, results
        assert name in snapshot()
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            results = list(pool.map(lambda _: raw(["rm", name]), range(args.workers)))
        for result in results:
            record(result)
        assert sum(r["code"] == 0 for r in results) == 1, results
        assert name not in snapshot()

    def interrupted():
        name = "crash-" + str(rng.getrandbits(64))
        result = record(raw(["create", name], rng.uniform(0, 0.006)))
        state = snapshot()
        if result["code"] == 0:
            assert state[name]["Id"] == result["stdout"].strip()
        elif result["code"] != -9:
            raise AssertionError(result)
        # A killed caller can have committed. Inspect first; never infer rollback.
        if name in state:
            result = record(raw(["rm", name], rng.uniform(0, 0.006)))
            assert result["code"] in [0, -9], result
        snapshot()
        call("rm", "-f", name)

    model = None
    generated_steps = 0
    operation_counts = {}

    def expected_network(name, identity, cidr, gateway):
        return {
            "Name": name,
            "Id": identity,
            "Driver": "bridge" if cidr else "null",
            "Containers": {},
            "EnableIPv4": bool(cidr),
            "EnableIPv6": False,
            "IPAM": {
                "Driver": "default",
                "Config": [{"Subnet": cidr, "Gateway": gateway}] if cidr else [],
            },
        }

    def check_model(actual):
        assert set(actual) == set(model), (actual, model)
        for name, expected in model.items():
            for key, value in expected.items():
                assert actual[name][key] == value, (name, key, actual[name], expected)

    def resolve(selector, state):
        if selector in state:
            return selector
        matches = [
            name for name, value in state.items() if selector and value["Id"].startswith(selector)
        ]
        return matches[0] if len(matches) == 1 else None

    def selector(name):
        identity = model[name]["Id"]
        return rng.choice([name, identity, identity[: rng.choice([1, 2, 8, 12, 32])]])

    def generated():
        nonlocal model, generated_steps
        if model is None:
            initial = snapshot()
            assert set(initial) == {"bridge", "none"}
            model = {
                "bridge": expected_network(
                    "bridge", initial["bridge"]["Id"], "172.17.0.0/24", "172.17.0.1"
                ),
                "none": expected_network("none", initial["none"]["Id"], None, None),
            }
            check_model(initial)
        # Existing networks persist across commands: each new action depends on
        # the independent model, rather than starting every case from scratch.
        operation = rng.choice(
            ["create"] * (5 if len(model) < 10 else 2)
            + ["inspect", "list", "remove", "invalid", "race", "interrupted"]
        )
        if len(model) >= 34 and operation == "create":
            operation = "remove"
        before = json.loads(json.dumps(model))
        plan = {"step": generated_steps, "operation": operation, "before": before}
        with (root / "workloads.jsonl").open("a") as log:
            log.write(json.dumps(plan) + "\n")
        operation_counts[operation] = operation_counts.get(operation, 0) + 1
        if operation == "create":
            length = rng.choice([1, 2, 12, 64, 127, 128])
            name = rng.choice("abcdefghijklmnopqrstuvwxyz0123456789") + "".join(
                rng.choices(
                    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-",
                    k=length - 1,
                )
            )
            if rng.randrange(5) == 0:
                name = rng.choice(list(model))
            occupied = {
                v["IPAM"]["Config"][0]["Subnet"] for v in model.values() if v["Driver"] == "bridge"
            }
            explicit = rng.choice([False, True, True])
            if explicit:
                base = rng.choice(
                    [
                        f"10.{rng.randrange(256)}.{rng.randrange(256)}",
                        f"192.168.{rng.randrange(256)}",
                        f"172.{rng.randrange(16, 32)}.{rng.randrange(256)}",
                        "192.0.2",
                    ]
                )
                cidr = base + ".0/24"
                if rng.randrange(6) == 0:
                    cidr = rng.choice(sorted(occupied))
                gateway = (
                    cidr.split("/")[0].rsplit(".", 1)[0]
                    + "."
                    + str(rng.choice([1, 2, 128, 254, rng.randrange(1, 255)]))
                )
            else:
                cidr = next(
                    f"172.{b}.{c}.0/24"
                    for b in range(18, 32)
                    for c in range(256)
                    if f"172.{b}.{c}.0/24" not in occupied
                )
                gateway = cidr.split("/")[0].rsplit(".", 1)[0] + ".1"
            options = []
            if explicit:
                options += [["--subnet", cidr]]
                if rng.choice([True, False]):
                    options += [["--gateway", gateway]]
                else:
                    gateway = cidr.split("/")[0].rsplit(".", 1)[0] + ".1"
            if rng.choice([True, False]):
                options += [[rng.choice(["--driver", "-d"]), "bridge"]]
            rng.shuffle(options)
            argv = ["create", *(part for option in options for part in option), name]
            allowed = (
                name not in model
                and name not in ["bridge", "none", "host"]
                and cidr not in occupied
            )
            identity = call(*argv, success=allowed).strip()
            if allowed:
                assert len(identity) == 64 and all(c in "0123456789abcdef" for c in identity)
                assert identity not in {v["Id"] for v in model.values()}
                model[name] = expected_network(name, identity, cidr, gateway)
            else:
                assert identity == "", identity
        elif operation == "inspect":
            selectors = [selector(rng.choice(list(model))) for _ in range(rng.randrange(1, 7))]
            if rng.randrange(4) == 0:
                selectors.insert(
                    rng.randrange(len(selectors) + 1), "missing-" + str(rng.getrandbits(64))
                )
            names = [resolve(s, model) for s in selectors]
            output = call("inspect", *selectors, success=all(n is not None for n in names))
            if all(n is not None for n in names):
                values = json.loads(output)
                assert [v["Name"] for v in values] == names
                for name, value in zip(names, values, strict=True):
                    for key, expected in model[name].items():
                        assert value[key] == expected, (value, model[name])
            else:
                assert output == "", output
        elif operation == "list":
            quiet, full = rng.choice([True, False]), rng.choice([True, False])
            argv = [rng.choice(["ls", "list"])]
            if quiet:
                argv.append(rng.choice(["-q", "--quiet"]))
            if full:
                argv.append("--no-trunc")
            rows = call(*argv).splitlines()
            expected_ids = {v["Id"] if full else v["Id"][:12] for v in model.values()}
            if quiet:
                assert set(rows) == expected_ids and len(rows) == len(model)
            else:
                assert rows[0] == "NETWORK ID\tNAME\tDRIVER\tSCOPE"
                assert {tuple(row.split("\t")) for row in rows[1:]} == {
                    (v["Id"] if full else v["Id"][:12], name, v["Driver"], "local")
                    for name, v in model.items()
                }
        elif operation == "remove":
            force = rng.choice([True, False])
            selectors = [selector(rng.choice(list(model))) for _ in range(rng.randrange(1, 7))]
            if rng.choice([True, False]):
                selectors += ["missing-" + str(rng.getrandbits(64))]
            expected_output, failures = [], 0
            for value in selectors:
                name = resolve(value, model)
                if name in ["bridge", "none"] or (name is None and not force):
                    failures += 1
                elif name is None:
                    # Force ignores missing names, but never ambiguous IDs.
                    matches = [n for n, v in model.items() if value and v["Id"].startswith(value)]
                    if len(matches) > 1:
                        failures += 1
                    else:
                        expected_output.append(value)
                else:
                    expected_output.append(value)
                    del model[name]
            argv = [rng.choice(["rm", "remove"])]
            if force:
                argv += [rng.choice(["-f", "--force"])]
            output = call(*argv, *selectors, success=not failures)
            assert output.splitlines() == expected_output, (selectors, output, expected_output)
        else:
            {"invalid": invalid, "race": race, "interrupted": interrupted}[operation]()
        check_model(snapshot())
        generated_steps += 1

    def terminated(signum, _frame):
        raise SystemExit(f"campaign interrupted by signal {signum}")

    signal.signal(signal.SIGTERM, terminated)

    try:
        scenarios = (
            [generated] if args.workload == "generated" else [lifecycle, invalid, race, interrupted]
        )
        round_number = 0
        while time.monotonic() - started < args.seconds:
            scenario = (
                scenarios[round_number] if round_number < len(scenarios) else rng.choice(scenarios)
            )
            scenario()
            counts[scenario.__name__] = counts.get(scenario.__name__, 0) + 1
            round_number += 1
            status = dict(
                seed=seed,
                elapsed=time.monotonic() - started,
                commands=commands,
                counts=counts,
                operations=operation_counts,
                generated_steps=generated_steps,
                unix=time.time(),
            )
            (root / "status.json").write_text(json.dumps(status))
            time.sleep(min(args.interval, max(0, args.seconds - (time.monotonic() - started))))
        if model is not None:
            check_model(snapshot())
            for name in list(model):
                if name not in ["bridge", "none"]:
                    call("rm", name)
        assert set(snapshot()) == {"bridge", "none"}
        status.update(elapsed=time.monotonic() - started, commands=commands, unix=time.time())
        (root / "outcome.json").write_text(json.dumps(dict(passed=True, **status)))
        print(json.dumps(dict(output=str(root), passed=True, **status)), flush=True)
    except BaseException:
        (root / "failure.txt").write_text(traceback.format_exc())
        (root / "outcome.json").write_text(
            json.dumps(
                dict(
                    passed=False,
                    seed=seed,
                    commands=commands,
                    counts=counts,
                    elapsed=time.monotonic() - started,
                )
            )
        )
        raise


if __name__ == "__main__":
    main()
