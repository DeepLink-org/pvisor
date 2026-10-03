import concurrent.futures
import json
import os
import re
import shutil
import signal
import subprocess
import time
from pathlib import Path


def prepare(ctx, count, name):
    root = ctx.fresh(name)
    work = root / "workspace"
    (work / "files").mkdir(parents=True)
    for i in range(count):
        (work / "files" / f"f{i:06d}").write_text(f"old-{i}\n")
    shutil.copy2(Path(__file__).with_name("apply_worker.py"), work / "worker.py")
    stage = root / "stage"
    command = ctx.command("staged", work, stage, ["/usr/bin/python3", "worker.py", str(count)])
    command[command.index("--timeout") + 1] = "600s"
    ctx.run(
        command,
        cwd=work,
        env={"PVISOR_RUN_HOME": str(root / "runs"), "XDG_CONFIG_HOME": str(root / "config")},
        timeout=600,
    )
    ctx.validate_bundle("staged", root / "runs", stage)
    assert all((work / "files" / f"f{i:06d}").read_text() == f"old-{i}\n" for i in range(count))
    assert len(list((stage / "upper/files").iterdir())) == count
    return root, work, stage


def check_target(work, count, prefix):
    assert all(
        (work / "files" / f"f{i:06d}").read_text() == f"{prefix}-{i}\n" for i in range(count)
    )


def run(ctx):
    for count in map(int, ctx.args.apply_sizes.split(",")):
        samples = min(ctx.args.samples, 3 if count == 100000 else (10 if count == 1000 else 30))
        warmups = 0 if count == 100000 else min(ctx.args.warmups, 1)
        for action in ("apply", "drop", "conflict"):
            trials = list(range(-warmups, samples))
            if count == 100000:
                print(f"preparing {action} {count}: {samples} independent stages", flush=True)
                with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                    prepared = list(
                        pool.map(lambda _: prepare(ctx, count, f"{action}-{count}"), trials)
                    )
            else:
                prepared = [None] * len(trials)
            for trial, item in zip(trials, prepared):
                print(f"{action} {count}: trial {trial}", flush=True)
                root, work, stage = item or prepare(ctx, count, f"{action}-{count}")
                if action == "conflict":
                    (work / "files/f000000").write_text("concurrent-host-edit\n")
                argv = [str(ctx.binary), action if action != "conflict" else "apply", str(stage)]
                if action != "drop":
                    argv += ["--all"]
                wall, _, _ = ctx.run(
                    argv, cwd=work, expected=1 if action == "conflict" else 0, timeout=600
                )
                if action == "apply":
                    check_target(work, count, "new")
                    ledger = json.loads((stage / "apply-ledger.json").read_text())
                    assert ledger["records"][-1]["state"] == "committed"
                elif action == "drop":
                    check_target(work, count, "old")
                else:
                    assert (work / "files/f000000").read_text() == "concurrent-host-edit\n"
                    check_target_except_first(work, count)
                    assert len(list((stage / "upper/files").iterdir())) == count
                if trial >= 0:
                    ctx.record(
                        dict(
                            suite="apply",
                            workload=action,
                            files=count,
                            backend="staged",
                            trial=trial,
                            wall_ms=wall,
                            correctness="passed",
                            logs=str(root),
                        )
                    )
                shutil.rmtree(work)
                shutil.rmtree(stage / "upper", ignore_errors=True)
        # Copy is a cost baseline; it has neither a conflict ledger nor crash recovery.
        for trial in range(samples):
            root = ctx.fresh(f"copy-{count}")
            work = root / "workspace"
            work.mkdir()
            source = root / "source"
            source.mkdir()
            for i in range(count):
                (source / f"f{i:06d}").write_text(f"new-{i}\n")
            target = work / "files"
            target.mkdir()
            wall, _, _ = ctx.run(
                ["cp", "-a", str(source) + "/.", str(target)], cwd=work, timeout=600
            )
            check_target(work, count, "new")
            ctx.record(
                dict(
                    suite="apply",
                    workload="copy",
                    files=count,
                    backend="native",
                    trial=trial,
                    wall_ms=wall,
                    correctness="passed",
                    logs=str(root),
                )
            )
            shutil.rmtree(work)
            shutil.rmtree(source)
    crashes(ctx)


def check_target_except_first(work, count):
    assert all((work / "files" / f"f{i:06d}").read_text() == f"old-{i}\n" for i in range(1, count))


def crashes(ctx):
    count = ctx.args.crash_files
    ctx.metadata["crash_protocol"] = {
        "files": count,
        "states": ctx.args.crash_states.split(","),
        "kill": "SIGKILL after observed ledger state; durable state recorded separately",
    }
    for state in ctx.args.crash_states.split(","):
        for trial in range(min(ctx.args.samples, 3)):
            root, work, stage = prepare(ctx, count, f"crash-{state}")
            with (
                (root / "crash.stdout").open("wb") as stdout,
                (root / "crash.stderr").open("wb") as stderr,
            ):
                process = subprocess.Popen(
                    [str(ctx.binary), "apply", str(stage), "--all"],
                    cwd=work,
                    env=ctx.env,
                    stdout=stdout,
                    stderr=stderr,
                    start_new_session=True,
                )
                seen = None
                deadline = time.monotonic() + 120
                while process.poll() is None and time.monotonic() < deadline:
                    try:
                        # Ledger publication is atomic. Read the final record's state
                        # without reparsing megabytes of file/preimage metadata:
                        # parsing that payload can exceed the committed kill window.
                        with (stage / "apply-ledger.json").open("rb") as ledger_file:
                            ledger_file.seek(0, os.SEEK_END)
                            ledger_file.seek(max(0, ledger_file.tell() - 1024))
                            tail = ledger_file.read()
                        match = re.search(
                            rb'"state": "(prepared|target_applied|committed)",\s*"remaining_changes":',
                            tail,
                        )
                        seen = match.group(1).decode() if match else None
                    except (FileNotFoundError, json.JSONDecodeError, IndexError):
                        continue
                    if seen == state:
                        os.killpg(process.pid, signal.SIGKILL)
                        break
                code = process.wait(timeout=120)
                assert code == -signal.SIGKILL and seen == state, (
                    f"crash window missed: {state}, {seen}, {code}"
                )
            durable = json.loads((stage / "apply-ledger.json").read_text())
            # Polling observes a state before kill; durable state at death is authoritative.
            actual = durable["records"][-1]["state"]
            (root / "ledger-at-kill.json").write_text(json.dumps(durable, indent=2))
            wall, _, _ = ctx.run(
                [str(ctx.binary), "apply", str(stage), "--all"], cwd=work, timeout=600
            )
            check_target(work, count, "new")
            final = json.loads((stage / "apply-ledger.json").read_text())
            assert all(r["state"] == "committed" for r in final["records"])
            ctx.record(
                dict(
                    suite="apply",
                    workload="crash-recovery",
                    files=count,
                    backend="staged",
                    trial=trial,
                    requested_state=state,
                    state_at_death=actual,
                    recovery_ms=wall,
                    correctness="passed",
                    logs=str(root),
                )
            )
            shutil.rmtree(work)
            shutil.rmtree(stage / "upper", ignore_errors=True)
