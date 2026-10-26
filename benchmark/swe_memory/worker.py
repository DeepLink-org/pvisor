"""Adapter for an explicitly supplied, hash-pinned historical SWE bridge.

This does not restore the retired bridge to the product tree. Incompatible
bridge sources fail closed at the exact source-shape assertions below.
"""

import os
import sys


def main():
    if not os.environ.get("PVISOR_SWE_CONTEXT") or sys.flags.optimize:
        raise RuntimeError("Explicit context and enabled assertions are required")
    """Isolated real-VM oracle/replay. Same frozen product and policies, no real model."""
    import asyncio
    import inspect
    import json
    import shlex
    import textwrap
    import threading
    import time
    import traceback
    from pathlib import Path

    import replay_reference as replay
    from arrival_common import ROOT, SLICE, SSD, VM_CPUS

    MANIFEST = json.loads((ROOT / "manifest.json").read_text())
    sys.path.insert(0, MANIFEST["source"])
    import environment
    from environment import PVisorEnvironment

    MODE, TASK, POLICY, IDENT = sys.argv[1:5]
    assert (
        MODE in ["oracle", "replay"]
        and POLICY in ["baseline", "offload"]
        and TASK in MANIFEST["tasks"]
    )
    assert all(c.isalnum() or c in "-_" for c in IDENT)
    JOB = ROOT / "jobs" / IDENT
    JOB.mkdir(mode=0o700, exist_ok=False)

    def save(p, x):
        tmp = p.with_suffix(p.suffix + ".tmp")
        tmp.write_text(json.dumps(x, indent=2))
        tmp.replace(p)

    # Same mailbox/notifier semantics, only the explicit stage root moves to SSD.
    init_src = textwrap.dedent(inspect.getsource(PVisorEnvironment.__init__))
    assert init_src.count("super().__init__()") == 1
    init_src = init_src.replace("super().__init__()", "BaseEnvironment.__init__(self)")
    old_path = "self.output / 'stage/upper/.sweeval-control'"
    assert init_src.count(old_path) == 1
    init_src = init_src.replace(old_path, "SSD / IDENT / 'stage/upper/.sweeval-control'")
    ns = dict(environment.__dict__, SSD=SSD, IDENT=IDENT)
    exec(compile(init_src, "<SSD stage mailbox placement>", "exec"), ns)
    PVisorEnvironment.__init__ = ns["__init__"]
    stop_original = textwrap.dedent(inspect.getsource(PVisorEnvironment.stop))
    stop_src = stop_original
    for old_line, before, after in [
        (
            "            await asyncio.to_thread(self.client.call, op='stop', timeout=15)",
            "            event(self.events, 'shutdown.stop_request_begin')",
            "            event(self.events, 'shutdown.stop_ack')",
        ),
        (
            "            await asyncio.wait_for(self.process.wait(), 300)",
            "            event(self.events, 'shutdown.process_wait_begin')",
            "            event(self.events, 'shutdown.process_wait_end', return_code=self.process.returncode)",
        ),
    ]:
        assert stop_src.count(old_line) == 1
        stop_src = stop_src.replace(old_line, before + "\n" + old_line + "\n" + after)
    needle_error = "        except (OSError, RuntimeError, TimeoutError):\n            pass"
    replacement_error = "        except (OSError, RuntimeError, TimeoutError) as exc:\n            event(self.events, 'shutdown.wait_error', error_type=type(exc).__name__, error=str(exc))"
    assert stop_src.count(needle_error) == 1
    stop_src = stop_src.replace(needle_error, replacement_error)
    restored = stop_src.replace(replacement_error, needle_error)
    restored = "\n".join(
        line for line in restored.split("\n") if "event(self.events, 'shutdown." not in line
    )
    assert restored == stop_original, "Timing must not alter stop control flow or bounds"
    ns = dict(environment.__dict__)
    exec(compile(stop_src, "<unchanged shutdown bounds with phase markers>", "exec"), ns)
    PVisorEnvironment.stop = ns["stop"]
    source = textwrap.dedent(inspect.getsource(PVisorEnvironment.start))
    for flag in ["PVISOR_DIAG_IDLE", "PVISOR_DIAG_OFFLOAD"]:
        assert source.count(flag + "=1") == 1
        source = source.replace(flag + "=1", flag + "=0")
    needle = "'CPUAffinity=' + ' '.join(map(str, sorted(os.sched_getaffinity(0))))"
    assert source.count(needle) == 1
    source = source.replace(needle, repr("CPUAffinity=" + VM_CPUS))
    # All VM services use this experiment's slice, which may be budgeted separately.
    needle = "'-p', 'KillMode=mixed',"
    assert source.count(needle) == 1
    source = source.replace(needle, needle + " " + repr("--slice=" + SLICE) + ",")
    needle = "'--setenv=PVISOR_STARTUP_TIMING=0',"
    assert source.count(needle) == 1
    source = source.replace(
        needle,
        needle
        + " '--setenv=PVISOR_SHUTDOWN_TIMING=1', '--setenv=PVISOR_STAGE_SYNC_WORKERS=1', '--setenv=PVISOR_DIAG_STAGE_DIR='+str(self.config['stage_dir']),",
    )
    source = source.replace(
        "'--setenv=PVISOR_STARTUP_TIMING=0',",
        "'--setenv=PVISOR_STARTUP_TIMING=0', '--setenv=PVISOR_SWE_OFFLOAD_AFTER_MS=50', '--property=UnsetEnvironment=PVISOR_HOST_OFFLOAD_LOCK', '--setenv=PVISOR_RAM_RANGE_RECLAIM="
        + "1"
        + "',",
    )
    # Same whole-service quota in both arms; only management config differs.
    quota_needle = "f'CPUQuota={self.config[\"cpus\"] * 100}%'"
    assert source.count(quota_needle) == 1
    source = source.replace(quota_needle, repr("CPUQuota=" + "250%"))
    ns = dict(environment.__dict__)
    exec(compile(source, "<density placement and diagnostic flags>", "exec"), ns)
    base_start = ns["start"]
    sample_src = textwrap.dedent(inspect.getsource(PVisorEnvironment._sample))
    assert sample_src.count("await asyncio.sleep(.25)") == 1
    sample_src = sample_src.replace("await asyncio.sleep(.25)", "await asyncio.sleep(1.0)")
    ns = dict(environment.__dict__)
    exec(compile(sample_src, "<density sampling interval>", "exec"), ns)
    PVisorEnvironment._sample = ns["_sample"]

    async def start(self):
        backing = SSD / IDENT / "live.ram"
        backing.parent.mkdir(mode=0o700, exist_ok=False)
        self.config["ram_backing"] = str(backing)
        self.config["stage_dir"] = str(backing.parent / "stage")
        save(
            JOB / "owned-unit.json",
            {
                "unit": self.unit,
                "backing": str(backing),
                "output": str(self.output),
                "stage_dir": self.config["stage_dir"],
                "created_unix_ns": time.time_ns(),
            },
        )
        try:
            await base_start(self)
            assert self.config["cpus"] == 2 and self.config["management"] == POLICY
            quota, period = map(int, (self.group / "cpu.max").read_text().split())
            expected_percent = 250
            assert quota * 100 == period * expected_percent
            parent = self.group.parent
            pq, pp = map(int, (parent / "cpu.max").read_text().split())
            assert parent.name == SLICE and pq * 100 == pp * 5975
            assert (
                int((parent / "memory.max").read_text()) == 34091302912
                and int((parent / "memory.swap.max").read_text()) == 0
            )
            save(
                JOB / "cpu-budget.json",
                dict(
                    vm_cpu_max=(self.group / "cpu.max").read_text().strip(),
                    parent_cpu_max=(parent / "cpu.max").read_text().strip(),
                    quota_percent=expected_percent,
                    vcpus=2,
                    management=POLICY,
                    range_reclaim=True,
                    offload_concurrency_cap=False,
                ),
            )
            assert backing.stat().st_dev == SSD.stat().st_dev != self.output.stat().st_dev
            stage = Path(self.config["stage_dir"])
            assert stage.resolve() == stage and stage.stat().st_dev == backing.stat().st_dev
            assert (
                (stage / "upper").stat().st_dev
                == (stage / "preimages").stat().st_dev
                == backing.stat().st_dev
            )
            assert not (self.output / "stage").exists()
            runner = self.runner_identity["pid"]
            env = Path(f"/proc/{runner}/environ").read_bytes().split(b"\0")
            expected = b"PVISOR_RAM_RANGE_RECLAIM=" + b"1"
            assert expected in env and not any(
                x.startswith(b"PVISOR_HOST_OFFLOAD_LOCK=") for x in env
            )
            assert b"PVISOR_SWE_OFFLOAD_AFTER_MS=50" in env
            save(
                JOB / "threshold-receipt.json",
                dict(
                    offload_after_ms=50,
                    policy=POLICY,
                    semantics="per-idle-round",
                    binary_sha256=MANIFEST["binary_sha256"],
                ),
            )
            save(
                JOB / "range-condition.json",
                dict(
                    condition=POLICY,
                    range_flag="enabled-in-both-arms",
                    cpu_quota_percent=250,
                    runner_env=expected.decode(),
                    offload_concurrency_cap=False,
                ),
            )

            flags = [
                x.decode()
                for x in Path(f"/proc/{runner}/environ").read_bytes().split(b"\0")
                if x.startswith((b"PVISOR_DIAG_IDLE=", b"PVISOR_DIAG_OFFLOAD="))
            ]
            assert sorted(flags) == ["PVISOR_DIAG_IDLE=0", "PVISOR_DIAG_OFFLOAD=0"]
            allowed = set()
            for chunk in VM_CPUS.split(","):
                bounds = list(map(int, chunk.split("-")))
                allowed.update(range(bounds[0], bounds[-1] + 1))
            threads = {
                p.name: sorted(os.sched_getaffinity(int(p.name)))
                for p in Path(f"/proc/{runner}/task").iterdir()
            }
            assert all(set(v) <= allowed for v in threads.values())
            snapshots = list((self.output / ".pvisor/run").glob(".capture/sessions/*/proxy.toml"))
            assert (
                len(snapshots) == 1 and self.config["gateway_upstream"] in snapshots[0].read_text()
            )
            code = "import json;print(json.dumps(dict(online=open('/sys/devices/system/cpu/online').read().strip(),possible=open('/sys/devices/system/cpu/possible').read().strip(),cmdline=open('/proc/cmdline').read().strip(),psi=open('/proc/pressure/cpu').read(),compaction=open('/proc/sys/vm/compaction_proactiveness').read().strip())))"
            boot = await self.exec("python3 -c " + shlex.quote(code), user="root", timeout_sec=60)
            assert boot.return_code == 0
            boot = json.loads(boot.stdout)
            assert boot["online"] == boot["possible"] == "0-1" and boot["compaction"] == "0"
            assert all(
                f not in boot["cmdline"]
                for f in [
                    "psi=0",
                    "psi_idle_quiesce=1",
                    "psi_idle_defer=1",
                    "pvisor_watchdog_defer=1",
                ]
            )
            save(
                JOB / "scope.json",
                dict(
                    unit=self.unit,
                    cgroup=str(self.group),
                    runner=runner,
                    threads=threads,
                    backing=str(backing),
                    stage_dir=str(stage),
                    placement_devices=dict(
                        stage=stage.stat().st_dev,
                        ram=backing.stat().st_dev,
                        logs=self.output.stat().st_dev,
                    ),
                    boot=boot,
                    diagnostic_flags=flags,
                    host_monotonic_ns=time.monotonic_ns(),
                ),
            )
        except BaseException:
            await self.stop()
            raise

    PVisorEnvironment.start = start

    def run_trial():
        os.umask(0o077)
        for k in list(os.environ):
            if k.upper().endswith("_PROXY") or k.startswith(("OPENAI_", "SWE_MODEL_")):
                os.environ.pop(k, None)
        os.environ.update(
            OPENAI_API_KEY="local-replay-no-credential",
            OPENAI_MODEL="agents-a1",
            NO_PROXY="127.0.0.1,localhost",
            no_proxy="127.0.0.1,localhost",
            SWE_MODEL_KWARGS="{}",
            LITELLM_LOCAL_MODEL_COST_MAP="True",
        )
        assert replay.sha(MANIFEST["binary"]) == MANIFEST["binary_sha256"]
        assert all(replay.sha(p) == h for p, h in MANIFEST["source_files"].items())
        config = json.loads((ROOT / "configs" / f"{TASK}-{POLICY}.json").read_text())
        from run import evaluate

        assert Path(environment.__file__).resolve().parent == Path(MANIFEST["source"])
        save(
            JOB / "started.json",
            dict(
                mode=MODE,
                task=TASK,
                policy=POLICY,
                unix_ns=time.time_ns(),
                host_monotonic_ns=time.monotonic_ns(),
            ),
        )
        if MODE == "oracle":
            asyncio.run(evaluate(config, JOB / "runs", oracle=True))
            check = json.loads((JOB / "runs/oracle-gate.json").read_text())
            assert check["status"] == "passed"
            check = {"passed": True, "oracle": check}
        else:
            path = ROOT / "plans" / f"{TASK}.json"
            assert replay.sha(path) == MANIFEST["tasks"][TASK]["plan_sha256"]
            plan = json.loads(path.read_text())
            state = replay.ReplayState(plan, JOB / "fixture.jsonl")
            server = replay.make_server(state)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            os.environ["OPENAI_BASE_URL"] = f"http://127.0.0.1:{server.server_port}/v1"
            try:
                asyncio.run(evaluate(config, JOB / "runs"))
                check = replay.audit_trial(
                    JOB / "runs" / TASK, Path(MANIFEST["tasks"][TASK]["source_trial"]), plan, state
                )
                save(JOB / "check.json", check)
                assert check["passed"], "Replay mismatch; inspect check.json"
            finally:
                server.shutdown()
                server.server_close()
        save(JOB / "check.json", check)
        save(
            JOB / "completed.json",
            dict(passed=True, host_monotonic_ns=time.monotonic_ns(), unix_ns=time.time_ns()),
        )
        print("PASS " + IDENT, flush=True)

    try:
        run_trial()
    except BaseException as exc:
        save(
            JOB / "error.json",
            dict(
                type=type(exc).__name__,
                error=str(exc),
                traceback=traceback.format_exc(),
                unix_ns=time.time_ns(),
            ),
        )
        raise


if __name__ == "__main__":
    main()
