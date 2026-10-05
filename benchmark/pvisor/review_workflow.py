#!/usr/bin/env python3
"""Compare complete sparse-change workflows, including private-view creation.

Benchmark: B-WORKFLOW (benchmark/README.md#b-workflow), role user-facing.
Motivation: users pay for creating, executing, reviewing and disposing an
independent Agent workspace, not only for executing its tool commands.
Conclusion sought: machine cost of retaining ten of twenty changes against
Git worktree and reflink-copy workflows, with host conflicts preserved.
Design: same prepared Git inputs; 100/10,000 files; randomized alternating
backends, three warmups, thirty samples; content diff and exact output checks.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import subprocess
import sys
import time
import traceback

from reference_baselines import validate_bundle_execution

BACKENDS = ('pvisor-stage', 'git-worktree', 'reflink-copy')
WORKER = '''import json
from pathlib import Path
for i in range(20):
    path = Path('files') / f'f{i:06d}'
    assert path.read_text().startswith(f'old-{i}\\n')
    path.write_text(f'new-{i}\\n')
print(json.dumps({'modified': 20}))
'''


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class Runner:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        (self.output / 'bin').mkdir()
        self.binary = self.output / 'bin/pvisor'
        shutil.copy2(args.binary.resolve(), self.binary)
        shutil.copy2(__file__, self.output / 'review_workflow.py')
        shutil.copy2(Path(__file__).with_name('reference_baselines.py'), self.output / 'reference_baselines.py')
        if args.source_manifest:
            shutil.copy2(args.source_manifest, self.output / 'binary-source-manifest.json')
        self.worker = self.output / 'worker.py'
        self.worker.write_text(WORKER)
        self.env = os.environ.copy() | {'PVISOR_STARTUP_TIMING': '0', 'GIT_CONFIG_NOSYSTEM': '1',
            'GIT_CONFIG_GLOBAL': '/dev/null', 'PYTHONDONTWRITEBYTECODE': '1', 'LC_ALL': 'C'}
        self.env.pop('PVISOR_TEST_ALLOW_NO_USERNS', None)
        self.report = dict(benchmark_id='B-WORKFLOW', recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
            arguments={k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
            host_kernel=platform.release(), host_platform=platform.platform(),
            cpu_model=next(x.split(':', 1)[1].strip() for x in Path('/proc/cpuinfo').read_text().splitlines() if x.startswith('model name')),
            host_load_before=os.getloadavg(), binary_sha256=sha(self.binary),
            binary_source_commit=args.binary_source_commit,
            source_manifest_sha256=sha(args.source_manifest) if args.source_manifest else None,
            harness_sha256=sha(__file__), worker_sha256=sha(self.worker),
            git_version=subprocess.check_output(['git', '--version'], text=True).strip(),
            cp_version=subprocess.check_output(['cp', '--version'], text=True).splitlines()[0],
            filesystem=subprocess.check_output(['stat', '-f', '-c', '%T', str(self.output)], text=True).strip(),
            protocol={'changes': 20, 'selected': 10, 'bytes_per_original_file': 4096,
                'cache': 'warm; no eviction', 'human_time': 'unmeasured',
                'timing': 'sum of view preparation, run, full content review, selected patch extraction/check/apply, disposal; fixture reset and correctness checks excluded',
                'interference': 'no timing-based exclusions; retain every valid slow sample',
                'isolation': 'pVisor rootless process; Git/reflink native processes, not a security ranking',
                'git_config': 'system/global config disabled; local core.autocrlf=false and core.hooksPath=/dev/null',
                'failures': 'retained with logs, never treated as zero or replaced by reruns'}, rows=[], failures=[], fixtures={})
        self.save()

    def save(self):
        self.report['host_load_after'] = os.getloadavg()
        temp = self.output / 'report.tmp'
        temp.write_text(json.dumps(self.report, indent=2) + '\n')
        temp.replace(self.output / 'report.json')

    def command(self, argv, cwd, logs, name, *, expected=(0,), input=None, env=None):
        begin = time.perf_counter_ns()
        process = subprocess.run(['taskset', '--cpu-list', self.args.cpu_affinity, *map(str, argv)],
            cwd=cwd, env=self.env | (env or {}), input=input, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, timeout=120)
        elapsed = (time.perf_counter_ns() - begin) / 1e6
        (logs / (name + '.stdout')).write_bytes(process.stdout)
        (logs / (name + '.stderr')).write_bytes(process.stderr)
        (logs / (name + '.json')).write_text(json.dumps(dict(argv=list(map(str, argv)),
            exit_code=process.returncode, wall_ms=elapsed)) + '\n')
        if process.returncode not in expected:
            raise RuntimeError(f'{name}: unexpected exit {process.returncode}; {process.stderr.decode(errors="replace")[-2000:]}')
        return elapsed, process.stdout, process.returncode

    def prepare_fixture(self, count):
        root = self.output / f'fixture-{count}'
        (root / 'files').mkdir(parents=True)
        for i in range(count):
            (root / 'files' / f'f{i:06d}').write_bytes(self.original(i))
        for argv in (['git', 'init', '-q'], ['git', 'config', 'core.autocrlf', 'false'],
                ['git', 'config', 'core.hooksPath', '/dev/null'], ['git', 'config', 'gc.auto', '0'],
                ['git', 'config', 'maintenance.auto', 'false'], ['git', 'add', 'files'],
                ['git', '-c', 'user.name=Benchmark', '-c', 'user.email=benchmark@example.invalid', 'commit', '-qm', 'fixture'], ['git', 'repack', '-ad'], ['git', 'prune-packed']):
            self.command(argv, root, root, 'prepare-' + argv[1])
        for path in root.glob('prepare-*'):
            path.unlink()
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, env=self.env, text=True).strip()
        self.report['fixtures'][str(count)] = dict(git_commit=commit, files=count, bytes=count * 4096)
        self.save()
        return root

    @staticmethod
    def original(i):
        prefix = f'old-{i}\n'.encode()
        return prefix + b'x' * (4096 - len(prefix))

    def check_original(self, work, count, *, selected=False, conflict=False):
        if {p.name for p in (work / 'files').iterdir()} != {f'f{i:06d}' for i in range(count)}:
            raise AssertionError('workspace file inventory changed')
        for i in range(count):
            expected = b'concurrent-host-edit\n' if conflict and i == 0 else f'new-{i}\n'.encode() if selected and i < 10 else self.original(i)
            if (work / 'files' / f'f{i:06d}').read_bytes() != expected:
                raise AssertionError(f'wrong original content at {i}')

    def trial(self, fixture, count, case, backend, index):
        root = self.output / 'trials' / f'{count}-{case}-{index:+04d}-{backend}'
        root.mkdir(parents=True)
        work, view, stage = root / 'original', root / 'view', root / 'stage'
        # Each control starts from the same private copy; this reset is not a product operation.
        self.command(['cp', '-a', '--reflink=auto', str(fixture), str(work)], root, root, 'fixture-reset')
        parts = dict(prepare_ms=0.0)
        if backend == 'git-worktree':
            parts['prepare_ms'], _, _ = self.command(['git', 'worktree', 'add', '--detach', str(view), 'HEAD'], work, root, 'view-prepare')
        elif backend == 'reflink-copy':
            parts['prepare_ms'], _, _ = self.command(['cp', '-a', '--reflink=always', str(work), str(view)], root, root, 'view-prepare')
        else:
            view = work
        payload = ['/usr/bin/python3', str(self.worker)]
        argv = payload if backend != 'pvisor-stage' else [str(self.binary), 'run', '--no-agent-defaults',
            '--overlaynet', 'off', '--stdio', 'inherit', '--timeout', '120s', '--stage', str(stage), '--', *payload]
        env = {'PVISOR_RUN_HOME': str(root / 'runs'), 'XDG_CONFIG_HOME': str(root / 'config')}
        parts['run_ms'], out, _ = self.command(argv, view, root, 'run', env=env)
        if json.loads(out) != {'modified': 20}:
            raise AssertionError('worker output differs')
        self.check_original(work, count)
        if backend == 'pvisor-stage':
            bundle = json.loads((stage / 'run-bundle.json').read_text())
            validate_bundle_execution(bundle, 'pvisor-staged', 'rootless_process')
            argv = [str(self.binary), 'status', '--review', '--diff', str(stage)]
        else:
            argv = ['git', 'diff', '--no-ext-diff', 'HEAD', '--', 'files']
        parts['review_ms'], out, _ = self.command(argv, view, root, 'review')
        text = out.decode()
        # Review must contain all changed paths and both sides of the actual content diff.
        if any(f'f{i:06d}' not in text or f'old-{i}' not in text or f'new-{i}' not in text for i in range(20)):
            raise AssertionError('review omitted a changed path or its content')
        if case == 'conflict':
            (work / 'files/f000000').write_text('concurrent-host-edit\n')
        selected = [f'files/f{i:06d}' for i in range(10)]
        if backend == 'pvisor-stage':
            argv = [str(self.binary), 'apply', str(stage)]
            for path in selected:
                argv.extend(['--path', path])
            parts['apply_ms'], _, code = self.command(argv, work, root, 'apply', expected=(1,) if case == 'conflict' else (0,))
            parts['dispose_ms'], _, _ = self.command([str(self.binary), 'drop', str(stage)], work, root, 'dispose')
        else:
            extract_ms, patch, _ = self.command(['git', 'diff', '--no-ext-diff', 'HEAD', '--', *selected], view, root, 'select-patch')
            check_ms, _, code = self.command(['git', 'apply', '--check', '-'], work, root, 'check-patch',
                input=patch, expected=(1,) if case == 'conflict' else (0,))
            parts['apply_ms'] = extract_ms + check_ms
            if case == 'normal':
                apply_ms, _, _ = self.command(['git', 'apply', '-'], work, root, 'apply', input=patch)
                parts['apply_ms'] += apply_ms
            if backend == 'git-worktree':
                parts['dispose_ms'], _, _ = self.command(['git', 'worktree', 'remove', '--force', str(view)], work, root, 'dispose')
            else:
                parts['dispose_ms'], _, _ = self.command(['rm', '-rf', str(view)], root, root, 'dispose')
        self.check_original(work, count, selected=case == 'normal', conflict=case == 'conflict')
        row = dict(files=count, case=case, backend=backend, trial=index,
            correctness='passed', conflict_refused=case == 'conflict' and code != 0,
            wall_ms=sum(parts.values()), **parts)
        (root / 'result.json').write_text(json.dumps(row, indent=2) + '\n')
        # Retain command evidence, remove only reproducible trial file trees outside timers.
        shutil.rmtree(work)
        if stage.exists():
            shutil.rmtree(stage / 'upper', ignore_errors=True)
        return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--binary-source-commit', required=True)
    parser.add_argument('--source-manifest', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=30)
    parser.add_argument('--warmups', type=int, default=3)
    parser.add_argument('--sizes', default='100,10000')
    parser.add_argument('--cases', default='normal,conflict')
    parser.add_argument('--cpu-affinity', default='0,1')
    parser.add_argument('--seed', type=int, default=20261006)
    args = parser.parse_args()
    sizes, cases = list(map(int, args.sizes.split(','))), args.cases.split(',')
    if args.samples < 1 or args.warmups < 0 or min(sizes) < 20 or len(set(sizes)) != len(sizes) or len(set(cases)) != len(cases) or set(cases) - {'normal', 'conflict'}:
        parser.error('invalid samples, warmups, sizes or cases')
    os.sched_setaffinity(0, {int(cpu) for cpu in args.cpu_affinity.split(',')})
    runner = Runner(args)
    fixtures = {size: runner.prepare_fixture(size) for size in sizes}
    rng = random.Random(args.seed)
    for trial in range(-args.warmups, args.samples):
        combinations = [(size, case, backend) for size in sizes for case in cases for backend in BACKENDS]
        rng.shuffle(combinations)
        for size, case, backend in combinations:
            print(f'trial={trial} files={size} case={case} backend={backend}', flush=True)
            try:
                row = runner.trial(fixtures[size], size, case, backend, trial)
                if trial >= 0:
                    runner.report['rows'].append(row)
            except Exception as error:
                runner.report['failures'].append(dict(files=size, case=case, backend=backend, trial=trial, error=str(error), traceback=traceback.format_exc()))
                runner.save()
                # A preflight/warmup failure stops the batch; measured failures remain visible.
                if trial < 0:
                    raise
            runner.save()
    if runner.report['failures']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
