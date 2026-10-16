"""SweEval BaseEnvironment backed by one persistent native pVisor VM."""
import asyncio
import base64
import json
import os
import re
import shlex
import tarfile
import tempfile
import threading
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

from sweeval.environments.base import BaseEnvironment, ExecResult
from transport import Client, event


def counters(path):
    return {key: int(value) for key, value in
            (line.split() for line in path.read_text().splitlines())}


class PVisorEnvironment(BaseEnvironment):
    """Prepared rootfs input; CPU/memory limits cover the whole VM service.

    First cohort supports shared verifier environments without host mounts.
    Host Agent/API resources are deliberately outside the VM service cgroup.
    """
    def __init__(self, *, rootfs, firmware, binary, output, python='/usr/bin/python3',
                 cpus=2, memory_mib=7168, env=None, workdir='/', apparmor_profile=None,
                 base_commit=None, repository_initialized=False, krun_enomem_workaround=None,
                 host_http_proxy=None):
        super().__init__()
        self.output = Path(output).resolve()
        self.output.mkdir(parents=True, mode=0o700, exist_ok=False)
        self.env, self.workdir = env or {}, workdir
        self.events = self.output / 'events.jsonl'
        self.token = uuid.uuid4().hex
        self.unit = 'pvisor-swe-' + uuid.uuid4().hex
        self.config = dict(rootfs=str(Path(rootfs).resolve()), firmware=str(Path(firmware).resolve()),
                           output=str(self.output), python=python, cpus=cpus, memory_mib=memory_mib,
                           token=self.token, environment={
                               'PATH': '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin',
                               'HOME': '/root', 'LANG': 'C.UTF-8', **self.env,
                               'TMPDIR': '/tmp'})
        self.binary = str(Path(binary).resolve())
        self.client = Client(self.output / 'stage/upper/.sweeval-control', self.token, self.events,
                             is_alive=lambda: self.process is not None and
                             self.process.returncode is None)
        self.process = self.sampler = None
        self.stream = None
        self.group = None
        self.apparmor_profile = apparmor_profile
        if base_commit is not None and not re.fullmatch(r"[0-9a-f]{40}", base_commit):
            raise ValueError("invalid prepared base_commit")
        self.base_commit = base_commit
        self.repository_initialized = repository_initialized
        if krun_enomem_workaround is not None and not isinstance(krun_enomem_workaround, bool):
            raise ValueError('krun_enomem_workaround must be null or boolean')
        self.krun_enomem_workaround = krun_enomem_workaround
        self.config['krun_enomem_workaround'] = krun_enomem_workaround
        if host_http_proxy is not None:
            proxy = urlsplit(host_http_proxy)
            if (proxy.scheme != 'http' or not proxy.hostname or not proxy.port or
                    proxy.username or proxy.password or proxy.path not in ('', '/') or
                    proxy.query or proxy.fragment or any(c.isspace() for c in host_http_proxy)):
                raise ValueError('host_http_proxy must be an HTTP endpoint without credentials')
        self.host_http_proxy = host_http_proxy
        self.config['host_http_proxy'] = host_http_proxy
        self.config['host_nofile_limit'] = 65536

    async def start(self):
        path = self.output / 'vm.json'
        path.write_text(json.dumps(self.config, indent=2))
        path.chmod(0o600)
        self.stream = (self.output / 'driver.log').open('wb')
        # Extra enclosing memory budget is explicit: guest RAM plus 2 GiB for
        # VMM/helpers. CPU rate matches the task's configured vCPU count.
        args = ['systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect',
                '--unit=' + self.unit, '-p', f'CPUQuota={self.config["cpus"] * 100}%',
                '-p', f'MemoryMax={(self.config["memory_mib"] + 2048) * 1024**2}',
                '-p', 'MemorySwapMax=0', '-p', 'KillMode=mixed',
                '-p', 'LimitNOFILE=65536',
                '-p', 'TimeoutStopSec=15',
                '--setenv=PVISOR_RUN_HOME=' + str(self.output / 'run-home'),
                '--setenv=XDG_CONFIG_HOME=' + str(self.output / 'config-home'),
                '--setenv=PVISOR_STARTUP_TIMING=0', '--setenv=PVISOR_FS_PROFILE=0']
        if self.krun_enomem_workaround is not None:
            args += ['--setenv=PVISOR_KRUN_ENOMEM_WORKAROUND=' +
                     str(int(self.krun_enomem_workaround))]
        if self.host_http_proxy:
            args += ['--setenv=HTTPS_PROXY=' + self.host_http_proxy,
                     '--setenv=HTTP_PROXY=' + self.host_http_proxy]
        if self.apparmor_profile:
            args += ['aa-exec', '-p', self.apparmor_profile, '--']
        args += [self.binary, str(path)]
        child_env = {k: v for k, v in os.environ.items()
                     if not any(x in k.upper() for x in ('KEY', 'TOKEN', 'SECRET', 'PASSWORD'))
                     and not k.startswith('PVISOR_')}
        child_env.update(PVISOR_RUN_HOME=str(self.output / 'run-home'),
                         XDG_CONFIG_HOME=str(self.output / 'config-home'))
        self.process = await asyncio.create_subprocess_exec(
            *args, stdout=self.stream, stderr=self.stream, env=child_env)
        event(self.events, 'vm_start', unit=self.unit, config=path.name)
        try:
            for _ in range(1200):
                if self.process.returncode is not None:
                    raise RuntimeError('VM service failed; inspect ' + str(self.output / 'driver.log'))
                if self.group is None:
                    result = await asyncio.create_subprocess_exec(
                        'systemctl', '--user', 'show', self.unit, '-p', 'ControlGroup', '--value',
                        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
                    stdout, _ = await result.communicate()
                    name = stdout.decode().strip()
                    if name:
                        group = Path('/sys/fs/cgroup') / name.lstrip('/')
                        if (group / 'cpu.stat').exists():
                            self.group = group
                            self.sampler = asyncio.create_task(self._sample())
                boot = self.client.root / 'boot.json'
                if boot.exists() and self.group is not None:
                    data = json.loads(self.client.read('boot.json'))
                    if data.get('token') != self.token:
                        raise RuntimeError('VM boot identity mismatch')
                    event(self.events, 'vm_ready', guest_pid=data['pid'], cgroup=str(self.group))
                    if self.base_commit:
                        await self._initialize_repository()
                    return
                await asyncio.sleep(.1)
            raise TimeoutError('VM ready timed out')
        except BaseException:
            await self.stop()
            raise

    async def _initialize_repository(self):
        # Verify the prepared repository (or initialize this private VM overlay).
        # Docker normally supplies /etc/hosts, which is absent from OCI layers.
        event(self.events, 'rootfs_initialize_start')
        command = (f'set -eu; cd {shlex.quote(self.workdir)}; '
                   f'git reset --hard {self.base_commit}; git clean -fd; '
                   f'git checkout {self.base_commit}; mkdir -p /logs; '
                   f'test "$(git rev-parse HEAD)" = {self.base_commit}; '
                   'python3 --version; git status --porcelain; uname -a')
        if self.repository_initialized:
            command = (f'set -eu; cd {shlex.quote(self.workdir)}; '
                       f'test "$(git rev-parse HEAD)" = {self.base_commit}; '
                       'python3 --version; uname -a')
        command = ("set -eu; if [ ! -s /etc/hosts ]; then "
                   "printf '127.0.0.1 localhost\\n::1 localhost ip6-localhost ip6-loopback\\n' "
                   "> /etc/hosts; fi; " + command)
        result = await self.exec(command, user='root', timeout_sec=600)
        (self.output / 'initialization.json').write_text(json.dumps(dict(
            base_commit=self.base_commit, command=command, return_code=result.return_code,
            stdout=result.stdout, stderr=result.stderr), indent=2))
        event(self.events, 'rootfs_initialize_end', return_code=result.return_code)
        if not result.ok:
            raise RuntimeError('public rootfs initialization failed; see initialization.json')

    async def _sample(self):
        with (self.output / 'resources.jsonl').open('a') as stream:
            while True:
                try:
                    stat = counters(self.group / 'memory.stat')
                    row = dict(host_monotonic_ns=time.monotonic_ns(),
                               cpu=counters(self.group / 'cpu.stat'),
                               memory_current=int((self.group / 'memory.current').read_text()),
                               memory_peak=int((self.group / 'memory.peak').read_text()),
                               memory_stat={k: stat.get(k) for k in ('anon', 'file', 'kernel')},
                               memory_events=counters(self.group / 'memory.events'),
                               swap_current=int((self.group / 'memory.swap.current').read_text()))
                    stream.write(json.dumps(row) + '\n')
                    stream.flush()
                except FileNotFoundError:
                    return
                await asyncio.sleep(.25)

    async def stop(self, *, delete=True):
        if self.process is None:
            return
        if self.process.returncode is None:
            self.client.cancel_pending()
            try:
                await asyncio.to_thread(self.client.call, op='stop', timeout=15)
                # VM exit is followed by runtime and overlay/bundle finalization. Large task
                # rootfs trees can need minutes even after the guest acknowledges stop.
                await asyncio.wait_for(self.process.wait(), 300)
            except (OSError, RuntimeError, TimeoutError):
                pass
            finally:
                # Exact random unit belongs to this environment only.
                proc = await asyncio.create_subprocess_exec(
                    'systemctl', '--user', 'stop', self.unit,
                    stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
                await proc.wait()
                await self.process.wait()
        if self.sampler:
            if not self.sampler.done():
                self.sampler.cancel()
            try:
                await self.sampler
            except asyncio.CancelledError:
                pass
        result_path = self.output / 'run-result.json'
        driver_result = json.loads(result_path.read_text()) if result_path.exists() else None
        event(self.events, 'vm_stop', return_code=self.process.returncode,
              driver_result_present=driver_result is not None,
              driver_state=driver_result.get('state') if driver_result else None)
        if self.stream:
            self.stream.close()
        self.process = None
        self.client.close()
        # Keep disk evidence and RAM file; delete means terminate the VM, not
        # erase the experiment. keep_environment is not supported in this cohort.

    async def exec(self, command, *, user=None, env=None, cwd=None, timeout_sec=None):
        cancellation = threading.Event()
        pending = asyncio.create_task(asyncio.to_thread(
            self.client.call, command, timeout=timeout_sec or 3600, cwd=cwd or self.workdir,
            env=self._merged_env(env), user=self.default_user if user is None else user,
            cancel_event=cancellation))
        try:
            result = await asyncio.shield(pending)
        except asyncio.CancelledError:
            cancellation.set()
            try:
                await asyncio.wait_for(asyncio.shield(pending), 15)
            except (OSError, RuntimeError, TimeoutError):
                await self.stop()
                # Retrieve any later transport exception after forced teardown.
                pending.add_done_callback(lambda task: task.exception() if not task.cancelled() else None)
            raise
        return ExecResult(return_code=result['return_code'], stdout=result['stdout'],
                          stderr=result['stderr'], timed_out=result['timed_out'],
                          duration_s=result['duration_s'], raw_return_code=result['raw_return_code'],
                          complete=True, termination_kind=(
                              'timed_out' if result['timed_out'] else
                              'signaled' if result['return_code'] < 0 else 'exited'))

    async def upload_file(self, *, source_path, target_path, mode=0o644):
        name = self.client.payload(source_path)
        cmd = (f'mkdir -p {shlex.quote(str(Path(target_path).parent))} && '
               f'cp -- /.sweeval-control/{name} {shlex.quote(target_path)} && '
               f'chmod {mode:o} {shlex.quote(target_path)}')
        result = await self.exec(cmd, user='root', timeout_sec=300)
        if not result.ok:
            raise RuntimeError('upload failed: ' + result.stderr)

    async def upload_dir(self, *, source_dir, target_dir):
        with tempfile.TemporaryDirectory(dir=self.output) as temp:
            archive = Path(temp) / 'upload.tar'
            with tarfile.open(archive, 'w') as stream:
                for path in Path(source_dir).iterdir():
                    stream.add(path, arcname=path.name)
            dest = '/tmp/sweeval-upload-' + uuid.uuid4().hex + '.tar'
            await self.upload_file(source_path=archive, target_path=dest)
            result = await self.exec(
                f'mkdir -p {shlex.quote(target_dir)} && tar -xf {shlex.quote(dest)} '
                f'-C {shlex.quote(target_dir)} && rm -- {shlex.quote(dest)}',
                user='root', timeout_sec=300)
            if not result.ok:
                raise RuntimeError('upload directory failed: ' + result.stderr)

    async def download_file(self, *, source_path, target_path):
        result = await self.exec('base64 -- ' + shlex.quote(source_path), user='root', timeout_sec=300)
        if not result.ok:
            raise FileNotFoundError(source_path)
        target_path = Path(target_path)
        target_path.parent.mkdir(parents=True, exist_ok=True)
        target_path.write_bytes(base64.b64decode(result.stdout))

    async def download_dir(self, *, source_dir, target_dir):
        dest = '/tmp/sweeval-download-' + uuid.uuid4().hex + '.tar'
        result = await self.exec(
            f'tar -cf {shlex.quote(dest)} -C {shlex.quote(source_dir)} .',
            user='root', timeout_sec=300)
        if not result.ok:
            raise FileNotFoundError(source_dir)
        with tempfile.TemporaryDirectory(dir=self.output) as temp:
            archive = Path(temp) / 'download.tar'
            await self.download_file(source_path=dest, target_path=archive)
            with tarfile.open(archive) as stream:
                stream.extractall(target_dir, filter='data')
        await self.exec('rm -- ' + shlex.quote(dest), user='root')

    async def check_liveness(self):
        return self.process is not None and self.process.returncode is None

    async def run_healthcheck(self):
        result = await self.exec('printf pvisor-ready', timeout_sec=10)
        if not result.ok or result.stdout != 'pvisor-ready':
            raise RuntimeError('guest healthcheck failed')
