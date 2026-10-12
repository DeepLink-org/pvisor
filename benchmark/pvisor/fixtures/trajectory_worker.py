#!/usr/bin/env python3
"""Python 3.9 guest worker; receives commands only, never reference observations."""
import argparse
import ctypes
import errno
import hashlib
import json
import os
import signal
import subprocess
import sys
import time
import traceback
import uuid
from pathlib import Path

SCHEMA = 'pvisor-trajectory-barrier/v1'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def publish(path, value):
    temporary = path.with_name('.' + path.name + '.tmp')
    with temporary.open('w') as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    fd = os.open(str(path.parent), os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def execute(command, output, timeout=60, cwd='/testbed'):
    # Adopt orphaned grandchildren so timeout cleanup can reap, not just signal them.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'cannot enable child reaping')
    start = time.monotonic_ns()
    with output.open('wb') as stream:
        child = subprocess.Popen(['/bin/bash', '-c', command], cwd=cwd,
                                 stdout=stream, stderr=subprocess.STDOUT,
                                 start_new_session=True)
        timed_out = False
        try:
            code = child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            processes = []
            for entry in Path('/proc').iterdir():
                if entry.name.isdigit():
                    try:
                        processes.append(dict(pid=entry.name, stat=(entry / 'stat').read_text(),
                                              cmdline=(entry / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')))
                    except OSError:
                        pass
            publish(output.with_suffix('.timeout.json'), dict(processes=processes, timeout=timeout))
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
            code = child.returncode
        # Reap adopted descendants; zombies alone are not continuing work.
        until = time.monotonic() + 2
        while True:
            try:
                pid, _ = os.waitpid(-child.pid, os.WNOHANG)
            except ChildProcessError:
                break
            if pid == 0:
                if time.monotonic() > until:
                    break
                time.sleep(.01)
        # A foreground command must not leave live group members behind.
        try:
            os.killpg(child.pid, 0)
        except ProcessLookupError:
            pass
        else:
            os.killpg(child.pid, signal.SIGKILL)
            raise RuntimeError('command left a background process group')
        stream.flush()
        os.fsync(stream.fileno())
    return dict(returncode=code, timed_out=timed_out,
                guest_start_ns=start, guest_end_ns=time.monotonic_ns(),
                output_sha256=sha(output.read_bytes()), output_bytes=output.stat().st_size)


def preflight(output, kind):
    # Seal only the injected input mount, inside this VM. No host mount changes.
    libc = ctypes.CDLL(None, use_errno=True)
    target = b'/replay-input'
    for flags in (4096, 4096 | 32 | 1):  # MS_BIND, then MS_BIND|MS_REMOUNT|MS_RDONLY
        if libc.mount(target, target, None, flags, None) != 0:
            raise OSError(ctypes.get_errno(), 'cannot seal replay input mount')
    command = """python -c 'import faulthandler; f=open("/replay-output/preflight-stacks.txt","w"); faulthandler.enable(file=f); import sys,sympy,json; print(json.dumps(dict(python=sys.version.split()[0], executable=sys.executable, sympy=sympy.__file__)))'
"""
    if kind == 'handshake':
        command = "python -c 'import sys,importlib.util,json; print(json.dumps(dict(python=sys.version.split()[0], executable=sys.executable, sympy=importlib.util.find_spec(\"sympy\").origin)))'"
    result = execute(command, output / 'preflight.txt', timeout=180)
    with (output / 'preflight-kernel.txt').open('wb') as kernel_log:
        subprocess.run(['dmesg'], stdout=kernel_log, stderr=subprocess.STDOUT)
    lines = (output / 'preflight.txt').read_text().splitlines()
    assert result['returncode'] == 0 and not result['timed_out'], result
    marker = next(i for i, line in enumerate(lines) if line.startswith('{"python":'))
    warnings = lines[:marker]
    lines = lines[marker:]
    environment = json.loads(lines[0])
    assert environment['python'] == '3.9.20', environment
    assert environment['sympy'].startswith('/testbed/'), environment
    git_command = 'git rev-parse HEAD^{tree} 5e17a90c19f7eecfa10c1ab872648ae7e2131323^{tree}'
    if kind != 'handshake':
        git_command = 'git status --porcelain && ' + git_command
    git_result = execute(git_command, output / 'preflight-git.txt', timeout=300)
    git_lines = (output / 'preflight-git.txt').read_text().splitlines()
    assert git_result['returncode'] == 0 and not git_result['timed_out'], git_result
    assert len(git_lines) == 2 and git_lines[0] == git_lines[1], 'dirty workspace or incorrect base tree: ' + repr(git_lines)
    assert sys.version_info[:3] == (3, 9, 20), sys.version
    assert Path('/root/.bashrc').is_file()
    interfaces = sorted(path.name for path in Path('/sys/class/net').iterdir())
    assert interfaces == ['lo'], 'unexpected guest network: ' + repr(interfaces)
    try:
        with Path('/replay-input/commands.jsonl').open('ab'):
            pass
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM, errno.EROFS):
            raise
        readonly = True
    else:
        raise RuntimeError('replay input is writable')
    return dict(scope='basic-handshake' if kind == 'handshake' else 'full-replay', environment=environment, tree=git_lines[0], python_probe=result, git_probe=git_result, interfaces=interfaces, import_warnings=warnings,
                testbed_uid=Path('/testbed').stat().st_uid, git_uid=Path('/testbed/.git').stat().st_uid,
                input_readonly=readonly, uid=os.getuid(), pid=os.getpid(),
                env={key: os.environ.get(key) for key in ('PATH', 'HOME', 'BASH_ENV', 'TZ')})


def matching_go(message, identity, step):
    return all(message.get(key) == value for key, value in
               dict(identity, kind='go', step=step, next_step=step + 1).items())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--run-token', required=True)
    parser.add_argument('--kind', choices=['preflight', 'handshake', 'replay'], required=True)
    parser.add_argument('--barriers', default='1,17,39')
    parser.add_argument('--barrier-timeout', type=float, default=600)
    args = parser.parse_args()
    output, control = Path('/replay-output'), Path('/replay-control')
    identity = dict(schema=SCHEMA, run_token=args.run_token, execution_token=uuid.uuid4().hex,
                    pid=os.getpid())
    canary = bytearray(bytes(range(256)) * 256)
    expected = sha(canary)
    completed = 0
    try:
        check = preflight(output, args.kind)
        command_bytes = Path('/replay-input/commands.jsonl').read_bytes()
        publish(control / 'boot.json', dict(identity, kind='boot', preflight=check,
                                           commands_sha256=sha(command_bytes)))
        if args.kind == 'preflight':
            publish(control / 'done.json', dict(identity, kind='done', completed=0))
            return
        commands = [json.loads(line) for line in command_bytes.splitlines()]
        barriers = set(map(int, args.barriers.split(','))) if args.barriers else set()
        if args.kind == 'handshake':
            commands = [dict(step=i, tool_call_id='handshake-' + str(i), command='true',
                             command_sha256=sha(b'true'), terminal=False) for i in range(1, 4)]
            barriers = {1, 2, 3}
        for row in commands:
            step = row['step']
            assert step == completed + 1
            assert sha(row['command'].encode()) == row['command_sha256']
            publish(control / ('started-%04d.json' % step), dict(identity, kind='started', step=step))
            result = execute(row['command'], output / ('tool-%04d.txt' % step))
            result.update(row)
            result.update(execution_token=identity['execution_token'], pid=os.getpid(),
                          source_sha256=sha(Path('/testbed/sympy/core/mod.py').read_bytes()))
            publish(output / ('step-%04d.json' % step), result)
            if result['timed_out']:
                raise RuntimeError('tool timeout at step %d' % step)
            completed = step
            if row['terminal']:
                raw = (output / ('tool-%04d.txt' % step)).read_bytes()
                marker, patch = raw.split(b'\n', 1)
                assert result['returncode'] == 0 and marker == b'COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT'
                with (output / 'final.patch').open('wb') as stream:
                    stream.write(patch)
                    stream.flush()
                    os.fsync(stream.fileno())
            if step not in barriers:
                continue
            # Check absence before publishing ready: exercise a previously missing go lookup.
            go_path = control / ('go-%04d.json' % step)
            assert not go_path.exists(), 'unexpected early go'
            publish(control / ('ready-%04d.json' % step), dict(identity, kind='ready', step=step,
                    next_step=step + 1, completed=completed,
                    result_sha256=sha((output / ('step-%04d.json' % step)).read_bytes())))
            deadline = time.monotonic() + args.barrier_timeout
            rejected = set()
            while True:
                if time.monotonic() >= deadline:
                    raise TimeoutError('no valid go for step %d' % step)
                try:
                    raw = go_path.read_bytes()
                    message = json.loads(raw)
                except FileNotFoundError:
                    time.sleep(.05)
                    continue
                if matching_go(message, identity, step):
                    break
                key = sha(raw)
                if key not in rejected:
                    publish(control / ('rejected-%04d.json' % step),
                            dict(identity, kind='rejected', step=step, received_sha256=key))
                    rejected.add(key)
                time.sleep(.05)
            assert completed == step and sha(canary) == expected
            publish(control / ('resumed-%04d.json' % step), dict(identity, kind='resumed', step=step,
                    next_step=step + 1, completed=completed, memory_ok=True, canary_sha256=expected))
        if args.kind == 'replay':
            # Hash the guest's final merged view, not just copy-up entries.
            files = {}
            for name in ('sympy/core/mod.py', 'patch.txt'):
                files[name] = sha((Path('/testbed') / name).read_bytes())
            publish(output / 'files.json', files)
            final_git = execute('git status --porcelain && git diff --binary', output / 'final-git.txt')
            publish(output / 'final-git.json', final_git)
            assert final_git['returncode'] == 0 and not final_git['timed_out'], final_git
        publish(control / 'done.json', dict(identity, kind='done', completed=completed,
                canary_sha256=sha(canary), memory_ok=sha(canary) == expected))
    except BaseException as error:
        publish(control / 'error.json', dict(identity, kind='error', completed=completed,
                error=repr(error), traceback=traceback.format_exc()))
        raise


if __name__ == '__main__':
    main()
