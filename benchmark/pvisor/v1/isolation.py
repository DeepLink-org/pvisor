"""Effective isolation per mode, judged by the host's final state.

Benchmark: B-ISOLATION (benchmark/README.md#b-isolation), role user-facing
(correctness, not performance).
Motivation: performance numbers mean nothing if isolation did not hold.
Conclusion sought: a mode-by-behavior table of whether out-of-view reads,
writes and network access are blocked, against common OCI writable mounts.
Design: a negative control (must block) and a positive control (must allow)
for every behavior; host final state is the verdict, not request results.
"""

import json
import random
import shutil
import socket
import tempfile
import threading
import traceback
from pathlib import Path


def run(ctx):
    ctx.metadata['isolation_protocol'] = dict(controls='native, host, staged, safe, VM, pVisor OCI and Podman writable workspace mount',
        positives='same fixture inside read/write and local Unix socketpair must succeed',
        negatives='outside absolute/symlink/proc-root/traversal reads, writes and host Unix socket; final host state decides',
        repetitions=min(ctx.args.samples,3),ordering='seeded shuffled backends per repetition',
        scope='path/socket fixtures, not comprehensive security proof; TCP policy controls measured separately in B-NETWORK')
    ctx.save()
    rng=random.Random(ctx.args.seed)
    for trial in range(min(ctx.args.samples,3)):
        backends=['native','host','staged','safe','vm','container','podman'];rng.shuffle(backends)
        for backend in backends:
            try:run_condition(ctx,backend,trial)
            except Exception as error:
                failure=ctx.capabilities.setdefault('isolation/'+backend,dict(state='failed',failures=[]))
                failure['failures'].append(dict(trial=trial,error=str(error),traceback=traceback.format_exc()));ctx.save()


def run_condition(ctx, backend, trial):
    root = ctx.fresh("isolation-" + backend)
    work = root / "workspace"
    work.mkdir()
    outside = root / "outside"
    outside.mkdir()
    (outside / "secret").write_text("benchmark-secret")
    (work / 'inside').write_text('benchmark-inside')
    (work / "escape").symlink_to(outside, target_is_directory=True)
    shutil.copy2(Path(__file__).with_name("isolation_worker.py"), work / "worker.py")
    short = Path(tempfile.mkdtemp(prefix="pv-iso-"))
    (short / "outside").symlink_to(outside, target_is_directory=True)
    socket_path = str(short / "outside/socket")
    stop = threading.Event()
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(socket_path)
    listener.listen(4)
    listener.settimeout(0.1)

    def serve():
        while not stop.is_set():
            try:
                client, _ = listener.accept()
            except TimeoutError:
                continue
            with client:
                client.sendall(b"benchmark-socket")

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    try:
        stage = root / "stage"
        runs = root / "runs"
        argv = ctx.command(
            backend,
            work,
            stage,
            ["/usr/bin/python3", "worker.py", str(outside), str(work), socket_path,
             '0,1' if backend == 'vm' else ctx.args.cpu_affinity],
        )
        if backend == "vm":
            argv[argv.index("--rootfs") + 1] = str(ctx.rootfs)
        home = root / "home"
        home.mkdir()
        wall, stdout, _ = ctx.run(
            argv,
            cwd=work,
            env={
                "HOME": str(home),
                "PVISOR_RUN_HOME": str(runs),
                "XDG_CONFIG_HOME": str(root / "config"),
            },
        )
        bundle = ctx.validate_bundle(backend, runs, stage)
        if bundle:
            stdout = bundle["run"]["output"]["stdout"]
        value = json.loads(stdout.strip().splitlines()[-1])
        assert all(value[key] for key in ('inside-read','inside-write','inside-socket')), 'positive fixture failed'
        value["host-outside-mutated"] = (outside / "written").exists() or (
            outside / "symlink-written"
        ).exists()
        value["host-lower-mutated"] = (work / "alias-written").exists()
        value["workspace-staged"] = not (work / "staged-marker").exists()
        # Observe every path, then assess against the requested boundary.
        if backend == 'native':
            assert all(value[key] for key in ('absolute-read','symlink-read','proc-root-read','traversal-read','unix-socket'))
            assert value['host-outside-mutated'] and value['host-lower-mutated']
        if backend in ("safe", "vm", "container", "podman"):
            assert not value["host-outside-mutated"]
            assert (
                not value["absolute-read"]
                and not value["symlink-read"]
                and not value['proc-root-read']
                and not value['traversal-read']
                and not value["unix-socket"]
            )
        if backend in ("staged", "safe", "vm"):
            assert value["workspace-staged"]
            assert not value['host-lower-mutated'], 'absolute alias bypassed stage'
            assert (stage/'upper/staged-marker').read_text() == 'benchmark-write'
        else:
            assert (work/'staged-marker').read_text() == 'benchmark-write'
        ctx.record(
            dict(
                suite="isolation",
                workload="escape-fixtures",
                backend=backend,
                trial=trial,
                rootfs="prepared OCI" if backend in ("vm", "container") else "host",
                wall_ms=wall,
                observations=value,
                safety=bundle["safety"] if bundle else None,
                observed_isolation=bundle["run"]["executor"]["isolation"] if bundle else ('container' if backend == 'podman' else 'native'),
                correctness="passed",
                logs=str(root),
            )
        )
    finally:
        stop.set()
        thread.join()
        listener.close()
        shutil.rmtree(short)
