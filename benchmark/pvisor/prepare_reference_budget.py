#!/usr/bin/env python3
"""Prepare a fresh owned whole-parent budget; no timed/public measurements.

Draft supporting B-STARTUP/B-FS-TOOLS/B-AGENT-TASK. Run only after unrelated
formal timing ends. Existing daemons/units and global delegation stay untouched.
Evidence is retained even when setup fails. Successful setup is not proof of
all future payload scopes; reference capability/inheritance gates remain.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / 'benchmark/pvisor'))
from resource_budget import ResourceBudget, parse_cpus


def save(path, value):
    temp = path.with_suffix('.tmp')
    temp.write_text(json.dumps(value, indent=2) + '\n')
    temp.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--memory-mib', type=int, default=16384)
    parser.add_argument('--cpu-affinity', default='0,1')
    parser.add_argument('--cpu-placement', choices=('affinity', 'cpuset'), default='affinity',
                        help='cpuset requires prior user-manager delegation; no global settings are changed')
    args = parser.parse_args()
    cpus = parse_cpus(args.cpu_affinity)
    if len(cpus) != 2 or args.memory_mib <= 0:
        parser.error('exactly two CPU IDs and a positive memory cap are required')
    if not cpus <= os.sched_getaffinity(0):
        parser.error('requested CPU IDs are not allowed to the caller')
    tools = {name: shutil.which(name) for name in
             ('busctl', 'systemctl', 'systemd-run', 'dockerd-rootless.sh',
              'dockerd', 'rootlesskit', 'pasta', 'docker', 'taskset')}
    if any(value is None for value in tools.values()):
        parser.error('required private rootless setup tools unavailable: ' + str(tools))
    root = args.output.resolve()
    if '.data' not in root.parts:
        parser.error('raw setup evidence must be under .data')
    if len(str(root / 'kit/api.sock').encode()) >= 104 or len(str(root / 'exec/containerd/containerd.sock').encode()) >= 104:
        parser.error('use a short .data output path for Unix sockets')
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    (root / 'commands').mkdir()
    nonce = uuid.uuid4().hex[:16]
    slice_unit = 'pvisorref' + nonce + '.slice'
    docker_unit = 'pvisorref' + nonce + '-docker.service'
    socket = Path('/tmp') / ('pvrb-' + nonce + '.sock')
    docker_host = 'unix://' + str(socket)
    record = dict(state='preparing', recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                  setup_pid=os.getpid(), uid=os.getuid(), root=str(root),
                  slice_unit=slice_unit, docker_unit=docker_unit, docker_host=docker_host,
                  memory_bytes=args.memory_mib * 1024 * 1024,
                  cpu_ids=sorted(cpus), cpu_placement=args.cpu_placement, cpu_quota=2, swap_bytes=0,
                  source_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), tools=tools,
                  scope='setup evidence only; not complete timed-payload compliance',
                  cleanup='retain data and failed evidence; only explicitly recorded owned units may later be stopped')
    save(root / 'setup.json', record)
    command_number = 0

    def command(purpose, argv, timeout=30, check=True):
        nonlocal command_number
        command_number += 1
        print(purpose + ': ' + json.dumps(argv), flush=True)
        dest = root / 'commands' / f'{command_number:04d}'
        dest.mkdir()
        started = time.monotonic_ns()
        details = dict(purpose=purpose, argv=argv, started_at=dt.datetime.now(dt.timezone.utc).isoformat())
        save(dest / 'command.json', details)
        try:
            result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            for name, value in [('stdout', error.stdout), ('stderr', error.stderr)]:
                (dest / name).write_bytes(value if isinstance(value, bytes) else (value or '').encode())
            save(dest / 'command.json', details | dict(timeout=True, duration_ms=(time.monotonic_ns()-started)/1e6))
            raise
        (dest / 'stdout').write_text(result.stdout)
        (dest / 'stderr').write_text(result.stderr)
        save(dest / 'command.json', details | dict(exit_code=result.returncode, timeout=False,
                                                  duration_ms=(time.monotonic_ns()-started)/1e6))
        if check and result.returncode:
            raise RuntimeError(f'{purpose}: exit {result.returncode}; retained {dest}')
        return result

    try:
        props = [('Description', 's', 'Owned pVisor reference benchmark budget'),
                 ('CPUAccounting', 'b', 'true'), ('MemoryAccounting', 'b', 'true'),
                 ('CPUQuotaPerSecUSec', 't', '2000000'),
                 ('MemoryMax', 't', str(record['memory_bytes'])), ('MemorySwapMax', 't', '0')]
        if args.cpu_placement == 'cpuset':
            mask = sum(1 << cpu for cpu in cpus).to_bytes(max(cpus) // 8 + 1, 'little')
            props.append(('AllowedCPUs', 'ay', str(len(mask)), *(str(byte) for byte in mask)))
        argv = ['busctl', '--user', '--quiet', 'call', 'org.freedesktop.systemd1',
                '/org/freedesktop/systemd1', 'org.freedesktop.systemd1.Manager',
                'StartTransientUnit', 'ssa(sv)a(sa(sv))', slice_unit, 'fail', str(len(props))]
        argv.extend(value for prop in props for value in prop)
        argv.append('0')
        command('Create a fresh private transient slice', argv)
        record['slice_created'] = True
        save(root / 'setup.json', record)
        state = command('Inspect the created slice',
                        ['systemctl', '--user', 'show', slice_unit, '--property=Id',
                         '--property=Transient', '--property=ActiveState', '--property=ControlGroup'])
        properties = dict(line.split('=', 1) for line in state.stdout.splitlines() if '=' in line)
        if properties.get('Id') != slice_unit or properties.get('Transient') != 'yes' or properties.get('ActiveState') != 'active':
            raise ValueError('new private transient slice is not active')
        relative = properties.get('ControlGroup', '')
        if not relative.startswith('/') or Path(relative).name != slice_unit or '..' in Path(relative).parts:
            raise ValueError('unexpected private slice cgroup identity')
        group = Path('/sys/fs/cgroup') / relative.lstrip('/')
        budget = ResourceBudget(group, record['memory_bytes'], frozenset(cpus),
                                cpu_placement=args.cpu_placement)
        record.update(cgroup=str(group), initial_budget=budget.read(), slice_properties=properties)
        save(root / 'setup.json', record)
        config = dict(**{'data-root': str(root / 'docker'), 'exec-root': str(root / 'exec'),
                         'storage-driver': 'overlay2', 'exec-opts': ['native.cgroupdriver=systemd'],
                         'features': {'containerd-snapshotter': False},
                         'iptables': False, 'ip6tables': False, 'bridge': 'none',
                         'pidfile': str(root / 'dockerd.pid')})
        save(root / 'daemon.json', config)
        runtime = Path('/run/user') / str(os.getuid())
        argv = ['systemd-run', '--user', '--quiet', '--unit=' + docker_unit,
                '--slice=' + slice_unit, '--property=Delegate=yes', '--property=Restart=no',
                '--property=MemoryAccounting=yes', '--property=CPUAccounting=yes',
                '--property=TimeoutStopSec=15s', '--working-directory=' + str(REPO),
                '--setenv=XDG_RUNTIME_DIR=' + str(runtime),
                '--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=' + str(runtime / 'bus'),
                '--setenv=DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR=' + str(root / 'kit'),
                '--setenv=DOCKERD_ROOTLESS_ROOTLESSKIT_NET=pasta',
                '--setenv=DOCKERD_ROOTLESS_ROOTLESSKIT_PORT_DRIVER=implicit',
                'taskset', '--cpu-list', args.cpu_affinity, tools['dockerd-rootless.sh'],
                '--config-file', str(root / 'daemon.json'), '--host', docker_host]
        command('Start a new private rootless daemon inside that slice', argv)
        record['docker_service_created'] = True
        save(root / 'setup.json', record)
        deadline = time.monotonic() + 60
        info = None
        while time.monotonic() < deadline:
            response = command('Wait for this exact private Docker endpoint',
                               ['docker', '--host', docker_host, 'info', '--format', '{{json .}}'],
                               timeout=5, check=False)
            if response.returncode == 0:
                info = json.loads(response.stdout)
                break
            time.sleep(0.5)
        if info is None:
            raise RuntimeError('private daemon did not become ready')
        if (info.get('CgroupDriver') != 'systemd' or str(info.get('CgroupVersion')) != '2'
                or 'name=rootless' not in info.get('SecurityOptions', [])
                or info.get('Driver') != 'overlay2'):
            raise ValueError('actual private Docker rootless/cgroup/storage setup does not match')
        record['docker_info'] = info
        daemons = []
        for pid in budget.members():
            proc = Path('/proc') / str(pid)
            argv = [v.decode() for v in (proc / 'cmdline').read_bytes().split(b'\0') if v]
            if argv and Path(argv[0]).name == 'dockerd' and docker_host in argv and proc.stat().st_uid == os.getuid():
                daemons.append(dict(pid=pid, argv=argv, uid=proc.stat().st_uid,
                                    start_ticks=int((proc / 'stat').read_text().rsplit(') ', 1)[1].split()[19])))
        if len(daemons) != 1:
            raise ValueError('missing unique owned dockerd identity inside the private budget')
        record.update(daemon=daemons[0], startup_witness=budget.witness_all_members([daemons[0]['pid']]),
                      state='ready-for-image-import-and-independent-runtime-capability-probes')
        save(root / 'setup.json', record)
        print(json.dumps({k: record[k] for k in ('state', 'slice_unit', 'cgroup', 'docker_host', 'daemon')}, indent=2), flush=True)
    except BaseException as error:
        record.update(state='failed', error_type=type(error).__name__, reason=str(error))
        save(root / 'setup.json', record)
        if record.get('docker_service_created'):
            command('Retain only this owned daemon service journal',
                    ['journalctl', '--user', '--unit=' + docker_unit, '--no-pager', '--output=short-monotonic'], check=False)
        raise


if __name__ == '__main__':
    main()
