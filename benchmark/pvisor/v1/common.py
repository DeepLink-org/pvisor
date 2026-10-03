"""Shared evidence, process and validation helpers for product benchmarks."""
from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time
from zoneinfo import ZoneInfo

from bench import percentile


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def distribution(values):
    return dict(n=len(values), p50=percentile(values, 50), p95=percentile(values, 95),
                p99=percentile(values, 99), minimum=min(values), maximum=max(values))


def checked(argv, **kwargs):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=kwargs.pop('timeout', 120), **kwargs)
    if result.returncode:
        raise RuntimeError(f'{argv!r}: exit {result.returncode}\n{result.stderr[-4000:]}')
    return result


class Context:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=False, mode=0o700)
        self.repo = Path(__file__).resolve().parents[3]
        (self.output / 'bin').mkdir()
        self.binary = self.output / 'bin/pvisor'
        shutil.copy2(args.binary.resolve(), self.binary)
        self.firmware = self.output/'firmware'
        shutil.copytree(args.firmware.resolve(), self.firmware, symlinks=False)
        self.toolchain = Path(checked(['rustc', '--print', 'sysroot']).stdout.strip())
        self.image = None
        self.rootfs = None
        self.counter = 0
        self.rows = []
        self.capabilities = {}
        shutil.copytree(self.repo/'benchmark/pvisor',self.output/'harness',ignore=shutil.ignore_patterns('__pycache__','.pytest_cache'))
        self.env = os.environ.copy()
        for key in ('http_proxy','https_proxy','all_proxy','HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY','no_proxy'):
            self.env.pop(key,None)
        self.env['PVISOR_STARTUP_TIMING'] = '0'
        self.env.pop('PVISOR_TEST_ALLOW_NO_USERNS', None)
        self.metadata = dict(
            schema='pvisor-benchmark/v1', suite='product-v1',
            recorded_at=dt.datetime.now(ZoneInfo('Asia/Shanghai')).isoformat(),
            platform=platform.platform(), cpu=next(l.split(':', 1)[1].strip() for l in Path('/proc/cpuinfo').read_text().splitlines() if l.startswith('model name')),
            memory_total_kib=next(int(l.split()[1]) for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemTotal:')),
            logical_cpus=os.cpu_count(), source_commit=checked(['git','rev-parse','HEAD'],cwd=self.repo).stdout.strip(),
            source_status=checked(['git','status','--porcelain'],cwd=self.repo).stdout,
            binary_sha256=digest(self.binary), source_binary=str(args.binary.resolve()),
            firmware_sha256=digest(self.firmware/'libkrunfw.so.5'),
            python=checked(['/usr/bin/python3','--version']).stdout.strip(),
            protocol=dict(samples=args.samples, warmups=args.warmups, caches='warm; no host cache eviction',
                          startup_logging=False, correctness_required=True, percentile='linear interpolation',
                          timing='wall includes process launch and teardown; worker_ms is workload only'),
            harness_sha256={str(p.relative_to(self.output/'harness')):digest(p) for p in (self.output/'harness').rglob('*.py')},
            load_before=os.getloadavg(),
        )
        self.save()

    def save(self):
        value=self.metadata | dict(capabilities=self.capabilities, rows=self.rows, load_after=os.getloadavg())
        temporary=self.output/'report.tmp'
        temporary.write_text(json.dumps(value,indent=2)+'\n')
        temporary.replace(self.output/'report.json')

    def fresh(self, name):
        self.counter += 1
        path=self.output/'trials'/f'{self.counter:05d}-{name}'
        path.mkdir(parents=True)
        return path

    def run(self, argv, *, cwd, env=None, timeout=120, expected=0):
        runenv=self.env | (env or {})
        if argv[0]=='podman':
            runenv['HOME']=os.environ['HOME']
        started=time.perf_counter_ns()
        try:
            process=subprocess.Popen(argv,cwd=cwd,env=runenv,stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
            try:
                stdout,stderr=process.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                import signal
                os.killpg(process.pid,signal.SIGKILL)
                stdout,stderr=process.communicate()
                raise TimeoutError(f'command timed out: {argv!r}')
        except BaseException:
            if 'process' in locals() and process.poll() is None:
                import signal
                os.killpg(process.pid,signal.SIGKILL)
                process.communicate()
            raise
        elapsed=(time.perf_counter_ns()-started)/1e6
        (Path(cwd).parent/'command.stdout').write_bytes(stdout)
        (Path(cwd).parent/'command.stderr').write_bytes(stderr)
        if process.returncode != expected:
            raise RuntimeError(f'exit {process.returncode}, expected {expected}; logs in {cwd}\n{stderr.decode(errors="replace")[-2000:]}')
        return elapsed,stdout.decode(errors='replace'),stderr.decode(errors='replace')

    def command(self, backend, workspace, stage, payload, *, network=None):
        if backend=='native':
            return payload
        if backend=='podman':
            if self.image is None:
                raise RuntimeError('prepared OCI image unavailable')
            return ['podman','run','--rm','--network','none','--entrypoint',payload[0],
                    '-v',f'{workspace}:/work:Z','-v',f'{self.toolchain}:{self.toolchain}:ro,Z',
                    '-w','/work',self.image,*payload[1:]]
        argv=[str(self.binary),'run','--no-agent-defaults','--stdio','capture',
              '--timeout','120s','--overlaynet','off']
        if backend in ('staged','safe'):
            argv += ['--stage',str(stage)]
        if backend=='safe':
            argv[argv.index('--overlaynet')+1]='proxy'
            argv += ['--safe','--filesystem','sandbox','--mount',f'{self.toolchain}:read']
        if backend=='vm':
            argv += ['--vm','--rootfs','/','--vm-library-dir',str(self.firmware),
                     '--cpu','2','--memory','1GiB','--stage',str(stage)]
        if backend=='container':
            argv += ['--executor','container','--container-rootfs',str(self.rootfs),
                     '--container-runtime','crun','--container-network','none',
                     '--container-mount',f'source="{workspace}",target="/work",read_only=false',
                     '--container-mount',f'source="{self.toolchain}",target="{self.toolchain}",read_only=true',
                     '--container-workdir','/work']
        if network:
            off=argv.index('--overlaynet')
            argv[off+1]=network[0]
            argv+=network[1:]
        return argv+['--',*payload]

    def validate_bundle(self, backend, runs, stage, expected_isolation=None):
        if backend in ('native','podman'):
            return None
        files=list(Path(runs).glob('*/run-bundle.json'))
        if Path(stage,'run-bundle.json').is_file():
            files.append(Path(stage,'run-bundle.json'))
        files=list(set(files))
        if len(files)!=1:
            raise RuntimeError(f'expected one Run Bundle, found {len(files)}')
        bundle=json.loads(files[0].read_text())
        if bundle['run']['state']!='completed' or bundle['run']['exit_code']!=0:
            raise RuntimeError('completed zero-exit Run Bundle required')
        observed=bundle['run']['executor']['isolation']
        expected={'host':'host_process','staged':'host_process','safe':'rootless_process','vm':'virtual_machine','container':'container'}[backend]
        expected=expected_isolation or expected
        if observed!=expected:
            raise RuntimeError(f'observed isolation {observed}, expected {expected}')
        if backend in ('staged','safe','vm') and not bundle['safety']['filesystem_changes_staged']:
            raise RuntimeError('staging not observed')
        if backend=='safe' and not bundle['safety']['filesystem_non_bypassable']:
            raise RuntimeError('non-bypassable filesystem required')
        return bundle

    def record(self, row):
        if row.get('correctness')!='passed':
            raise RuntimeError('failed samples cannot enter performance summary')
        self.rows.append(row)
        with (self.output/'samples.jsonl').open('a') as f:
            f.write(json.dumps(row)+'\n')
        self.save()
