"""Apply/drop cost, conflict detection and interrupted-apply recovery.

Benchmark: B-APPLY (benchmark/README.md#b-apply), role user-facing.
Motivation: staged changes pay off only when applied; users need the cost by
file count and assurance that concurrent host edits are never overwritten.
Conclusion sought: apply time from 10 to 100,000 files and the scale that
stays interactive; every injected host edit detected as a conflict; a known
final state after SIGKILL at each apply phase.
Design: file-count sweep against same-batch Git patch apply, conflict
injection before and during apply, kill injection per phase.
"""

import json
import os
import re
import random
import traceback
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
    assert all((stage / "upper/files" / f"f{i:06d}").read_text() == f"new-{i}\n" for i in range(count))
    return root, work, stage


def check_target(work, count, prefix):
    assert all(
        (work / "files" / f"f{i:06d}").read_text() == f"{prefix}-{i}\n" for i in range(count)
    )


def staged_trial(ctx, count, action, trial, *, syscall_tracer=None):
    root, work, stage = prepare(ctx, count, f"{action}-{count}")
    if action == "conflict":
        (work / "files/f000000").write_text("concurrent-host-edit\n")
    argv = [str(ctx.binary), action if action != "conflict" else "apply", str(stage)]
    if action != "drop":
        argv += ["--all"]
    if syscall_tracer is not None:
        argv=[str(syscall_tracer),'-ff','--decode-fds=path','-ttt','-T',
            '-e','trace=%file,%desc',
            '-o',str(root/'syscalls'),'--',*argv]
    wall, _, _ = ctx.run(argv, cwd=work, expected=1 if action == "conflict" else 0, timeout=600)
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
    row=dict(suite="apply", workload=action, files=count, backend="staged",
             trial=trial, wall_ms=wall, correctness="passed", logs=str(root))
    if trial >= 0:
        ctx.record(row)
    shutil.rmtree(work)
    shutil.rmtree(stage / "upper", ignore_errors=True)
    return row


def copy_trial(ctx, count, trial):
    # Copy is a cost control without conflict detection or recovery semantics.
    root = ctx.fresh(f"copy-{count}")
    work, source = root / "workspace", root / "source"
    (work / "files").mkdir(parents=True)
    source.mkdir()
    for i in range(count):
        (source / f"f{i:06d}").write_text(f"new-{i}\n")
    wall, _, _ = ctx.run(["cp", "-a", str(source) + "/.", str(work / "files")], cwd=work, timeout=600)
    check_target(work, count, "new")
    if trial >= 0:
        ctx.record(dict(suite="apply", workload="copy", files=count, backend="native",
                        trial=trial, wall_ms=wall, correctness="passed", logs=str(root)))
    shutil.rmtree(work)
    shutil.rmtree(source)


def run(ctx, *, include_git=False):
    from .baselines import git_trial

    sizes = list(map(int, ctx.args.apply_sizes.split(",")))
    if not sizes or min(sizes) < 1 or len(sizes) != len(set(sizes)):
        raise ValueError("apply sizes must be distinct positive file counts")
    actions = ["apply", "drop", "conflict", "copy"] + (["git-apply"] if include_git else [])
    counts = {count: ctx.args.samples for count in sizes}
    warmups = {count: ctx.args.warmups for count in sizes}
    seed = ctx.args.seed
    ctx.metadata["apply_protocol"] = dict(
        sizes=sizes, actions=actions, samples_per_size=counts, warmups_per_size=warmups,
        order="seeded random order of all eligible size/action conditions per paired round",
        seed=seed, preparation="fresh independent stage/target per trial, outside timer",
        timing="application/drop/copy command only, excludes creation of changes and patch",
        correctness="full expected content; conflict refuses without any target writes",
        interference="retain every valid slow sample; no timing-based exclusions",
    )
    ctx.save()
    rng = random.Random(seed)
    for trial in range(-max(warmups.values()), max(counts.values())):
        conditions = [(count, action) for count in sizes for action in actions
                      if -warmups[count] <= trial < counts[count]]
        rng.shuffle(conditions)
        for count, action in conditions:
            print(f"apply files={count} action={action} trial={trial}", flush=True)
            try:
                if action == "git-apply":
                    git_trial(ctx, count, trial)
                elif action == "copy":
                    copy_trial(ctx, count, trial)
                else:
                    staged_trial(ctx, count, action, trial)
            except Exception as error:
                key = f"apply/{count}/{action}"
                failure = ctx.capabilities.setdefault(key, dict(state="failed", failures=[]))
                failure["failures"].append(dict(trial=trial, error=str(error), traceback=traceback.format_exc()))
                ctx.save()
                if trial < 0:
                    raise
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
            if actual != state:
                ctx.capabilities[f"crash/{state}/{trial}"] = dict(state="window-missed", requested_state=state, state_at_death=actual, logs=str(root))
                ctx.save()
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
                    kill_window_hit=actual == state,
                    recovery_ms=wall,
                    correctness="passed",
                    logs=str(root),
                )
            )
            shutil.rmtree(work)
            shutil.rmtree(stage / "upper", ignore_errors=True)


def concurrent_conflicts(ctx):
    """Inject an external edit after actual target writes, without a test hook.

    This is correctness evidence, not latency. SIGSTOP freezes only our apply
    process; editing an untouched target then SIGCONT tests the running apply,
    rather than disguising a pre-apply edit as concurrency coverage.
    """
    count=ctx.args.crash_files
    if count<1000:raise ValueError('mid-apply probe requires at least 1000 files to observe a write window')
    ctx.metadata['concurrent_apply_protocol']=dict(files=count,repetitions=min(ctx.args.samples,3),
        injection='observe first real target change, SIGSTOP owned apply, verify Prepared ledger and remaining original file, edit/fsync that file, SIGCONT',
        scope='external writer without cooperative target lock; correctness only; missed windows are untested, not passed',
        required='nonzero conflict and preserved injected host bytes; partial previous application recorded')
    ctx.save()
    for trial in range(min(ctx.args.samples,3)):
        root,work,stage=prepare(ctx,count,'concurrent-apply')
        process=None;stopped=False;row=dict(suite='apply',workload='conflict-during-target-writes',backend='staged',files=count,trial=trial,correctness='failed',logs=str(root))
        try:
            with (root/'apply.stdout').open('wb') as stdout,(root/'apply.stderr').open('wb') as stderr:
                argv=['taskset','--cpu-list',ctx.args.cpu_affinity,str(ctx.binary),'apply',str(stage),'--all']
                process=subprocess.Popen(argv,cwd=work,env=ctx.env,stdout=stdout,stderr=stderr,start_new_session=True)
                deadline=time.monotonic()+120
                first=work/'files/f000000'
                while process.poll() is None and time.monotonic()<deadline:
                    if first.read_text()=='new-0\n':break
                    time.sleep(.001)
                if process.poll() is not None or first.read_text()!='new-0\n':raise RuntimeError('mid-apply write window missed')
                os.killpg(process.pid,signal.SIGSTOP);stopped=True
                deadline=time.monotonic()+5
                while time.monotonic()<deadline:
                    status=Path(f'/proc/{process.pid}/status').read_text()
                    if any(line.startswith('State:') and line.split()[1]=='T' for line in status.splitlines()):break
                    time.sleep(.001)
                else:raise RuntimeError('owned apply did not stop')
                ledger=json.loads((stage/'apply-ledger.json').read_text())
                row['state_at_injection']=ledger['records'][-1]['state']
                if row['state_at_injection']!='prepared':raise RuntimeError('mid-apply write window missed; ledger already advanced')
                remaining=[i for i in range(count-1,max(-1,count-100),-1) if (work/'files'/f'f{i:06d}').read_text()==f'old-{i}\n']
                if not remaining:raise RuntimeError('no original file remains in observed injection window')
                index=remaining[0];path=work/'files'/f'f{index:06d}';edit=f'concurrent-host-edit-{trial}\n'
                row['injected_path']=str(path.relative_to(work));row['injected_content']=edit
                row['already_applied_before_injection']=sum((work/'files'/f'f{i:06d}').read_text()==f'new-{i}\n' for i in range(count))
                with path.open('w') as target:target.write(edit);target.flush();os.fsync(target.fileno())
                (root/'injection.json').write_text(json.dumps(row,indent=2)+'\n')
                os.killpg(process.pid,signal.SIGCONT);stopped=False
                row['exit_code']=process.wait(timeout=120)
                row['host_edit_preserved']=path.read_text()==edit;row['final_injected_content']=path.read_text()
                row['ledger_final']=json.loads((stage/'apply-ledger.json').read_text())['records'][-1]['state']
                error_output=(root/'apply.stderr').read_text()
                row['detected_conflict']=row['exit_code']!=0 and 'target changed after staging at' in error_output and path.name in error_output
                if not row['host_edit_preserved']:raise ValueError('apply silently overwrote the external edit injected during target writes')
                if not row['detected_conflict']:raise ValueError('concurrent host edit did not produce an explicit conflict outcome')
                row['correctness']='passed';ctx.record(row)
        except Exception as error:
            row.update(error=str(error),traceback=traceback.format_exc())
            failure=ctx.capabilities.setdefault('apply/concurrent-conflicts',dict(state='failed',failures=[]))
            failure['failures'].append(row);ctx.save()
        finally:
            if process and process.poll() is None:
                os.killpg(process.pid,signal.SIGKILL);process.wait(timeout=10)
            (root/'result.json').write_text(json.dumps(row,indent=2)+'\n')
