"""B-SWE-RESIDENT-ENG diagnostic: bounded logger/controller fault checks.

No model calls. A FIFO models a blocked log write without stressing host disks.
Each case requires a fresh output directory and a prepared Linux/KVM rootfs.
"""
import argparse
import asyncio
import fcntl
import json
import os
import select
import signal
import threading
import time
from pathlib import Path

from environment import PVisorEnvironment


def proc_fields(pid):
    return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()


def pipe_waiters(pid):
    result = []
    for path in Path(f'/proc/{pid}/task').glob('*/wchan'):
        try:
            if path.read_text().strip() == 'pipe_write':
                result.append(int(path.parent.name))
        except FileNotFoundError:
            pass
    return result


async def check(args):
    env = PVisorEnvironment(rootfs=args.rootfs, firmware=args.firmware, binary=args.binary,
                           python=args.python, output=args.output, cpus=1, memory_mib=1024,
                           management='baseline', apparmor_profile=args.apparmor_profile,
                           krun_enomem_workaround=False)
    outcome = dict(case=args.case, model_calls=0, correctness='failed')
    root = Path(args.output)
    pipe = root / 'runtime-events.jsonl'
    fd, drainer, pending, controller = None, None, None, None
    stop = threading.Event()

    def drain():
        with (root / 'runtime-events.fifo-capture.jsonl').open('wb') as output:
            while True:
                if select.select([fd], [], [], .05)[0]:
                    data = os.read(fd, 65536)
                    if data:
                        output.write(data)
                        continue
                if stop.is_set():
                    break

    def release_log():
        nonlocal drainer
        if fd is not None and drainer is None:
            drainer = threading.Thread(target=drain)
            drainer.start()

    try:
        if args.case == 'slow_log':
            os.mkfifo(pipe, 0o600)
            fd = os.open(pipe, os.O_RDONLY | os.O_NONBLOCK)
            outcome['pipe_bytes'] = fcntl.fcntl(fd, fcntl.F_SETPIPE_SZ, 4096)
        await asyncio.wait_for(env.start(), 180)
        await env.run_healthcheck()
        runner = env.runner_identity['pid']
        controller = int(proc_fields(runner)[1])
        outcome.update(runner_pid=runner, controller_pid=controller)
        if args.case == 'slow_log':
            deadline = time.monotonic() + 15
            while not pipe_waiters(controller):
                if time.monotonic() >= deadline:
                    raise AssertionError('logger did not reach a blocked pipe write')
                await asyncio.sleep(.05)
            outcome['blocked_writer_tids'] = pipe_waiters(controller)
            began = time.monotonic()
            # Twice the 3s heartbeat deadline. Control must stay responsive.
            for _ in range(12):
                await asyncio.sleep(.5)
                await env.run_healthcheck()
                assert env.runner(), 'slow logging killed the runner'
            outcome['blocked_window_s'] = time.monotonic() - began
            assert pipe_waiters(controller), 'log unexpectedly drained'
            release_log()
            await env.run_healthcheck()
        else:
            pending = asyncio.create_task(env.exec('sleep 30', timeout_sec=120))
            await asyncio.sleep(.3)
            began = time.monotonic()
            target = runner if args.case == 'kill_runner' else controller
            action = signal.SIGSTOP if args.case == 'stop_controller' else signal.SIGKILL
            os.kill(target, action)
            try:
                await asyncio.wait_for(asyncio.shield(pending), 8)
                raise AssertionError('tool unexpectedly succeeded')
            except RuntimeError as exc:
                assert 'VM process exited' in str(exc), str(exc)
                outcome['tool_error'] = str(exc)
            outcome['fault_to_tool_error_s'] = time.monotonic() - began
            assert not env.runner(), 'native runner still alive'
            if args.case in ('kill_runner', 'stop_controller'):
                outcome['controller_alive_at_notification'] = Path(f'/proc/{controller}').exists()
                assert outcome['controller_alive_at_notification']
    finally:
        # Only signals the exact controller launched in this case, never a PID
        # discovered by a host-wide name match. Resume before systemd teardown.
        if args.case == 'stop_controller' and controller is not None:
            try:
                os.kill(controller, signal.SIGCONT)
            except ProcessLookupError:
                pass
        release_log()
        if pending is not None and not pending.done():
            pending.cancel()
            await asyncio.gather(pending, return_exceptions=True)
        await env.stop()
        stop.set()
        if drainer is not None:
            drainer.join(timeout=5)
            assert not drainer.is_alive()
        if fd is not None:
            os.close(fd)
        if args.case == 'slow_log' and drainer is not None:
            pipe.unlink()
            (root / 'runtime-events.fifo-capture.jsonl').rename(pipe)
        (root / 'fault-result.json').write_text(json.dumps(outcome, indent=2))
    if args.case == 'slow_log':
        live = [json.loads(line) for line in pipe.read_text().splitlines()]
        final = json.loads((root / 'runtime-events.json').read_text())
        assert live == final, 'live log tail lost/reordered'
        outcome['event_count'] = len(live)
        assert json.loads((root / 'run-result.json').read_text())['state'] == 'completed'
    if args.case == 'stop_controller':
        events = json.loads((root / 'runtime-events.json').read_text())
        # Runner stderr is retained in execution output, not necessarily driver.log.
        texts = [json.dumps(events), (root / 'run-result.json').read_text()]
        assert any('controller_read' in text or 'status_write' in text for text in texts), 'missing runner failure phase'
    outcome['correctness'] = 'passed'
    (root / 'fault-result.json').write_text(json.dumps(outcome, indent=2))
    print(json.dumps(outcome), flush=True)


if __name__ == '__main__':
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ('rootfs', 'firmware', 'binary', 'output'):
        parser.add_argument('--' + field, required=True)
    parser.add_argument('--python', default='/usr/bin/python3')
    parser.add_argument('--apparmor-profile')
    parser.add_argument('--case', choices=['slow_log', 'stop_controller', 'kill_controller', 'kill_runner'], required=True)
    asyncio.run(check(parser.parse_args()))
