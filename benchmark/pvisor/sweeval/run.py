"""Benchmark: B-SWE-RESIDENT-ENG, role engineering A/B (resident baseline).

Motivation: assess whether model waits leave resident VM memory at low CPU use.
Conclusion sought: phase-aligned CPU/memory evidence, not measured offload savings.
Design: explicit Pro cohorts, serial VMs, 250 ms sampling; retain failed attempts.
"""
import argparse
import asyncio
import json
import os
import sys
import time
from pathlib import Path

from agent import HostMiniAgent
from agent_runner import model_key, model_settings
from config import load_config, validate_oracle
from environment import PVisorEnvironment
from transport import event


def load_plan(config):
    from sweeval.task.task import Task

    tasks = []
    for entry in config['tasks']:
        task = Task(Path(config['task_dir']) / entry['directory'])
        if task.config.resolve_verifier_mode().value != 'shared':
            raise ValueError('this adapter only supports shared verifier environments')
        prepared = Path(entry['prepared'])
        ready = json.loads(prepared.read_text()) if prepared.exists() else None
        if ready and (ready['source_image'] != task.config.environment.docker_image or
                      ready.get('resolved_image', ready['source_image']) !=
                      entry.get('image', task.config.environment.docker_image)):
            raise ValueError('prepared rootfs image does not match task/override')
        tasks.append((entry, task, ready))
    return tasks


async def evaluate(config, output, *, oracle=False):
    from sweeval.agents.factory import AGENT_REGISTRY
    from sweeval.trial.config import AgentTrialConfig, EnvironmentTrialConfig, TrialConfig
    from sweeval.trial.hooks import TrialEvent
    from sweeval.trial.single_step import SingleStepTrial

    plan = load_plan(config)
    if any(ready is None for _, _, ready in plan):
        raise ValueError('task images not prepared; run prepare.py first')
    model = dict(profile='oracle') if oracle else model_settings()
    if not oracle:
        model_key(model['profile'])
        validate_oracle(config, plan)
    AGENT_REGISTRY[HostMiniAgent.name()] = HostMiniAgent
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    (output / 'model-settings.json').write_text(json.dumps(model, indent=2))
    summaries = []
    for entry, task, prepared in plan:
        trial_dir = output / entry['name']
        trial_dir.mkdir()
        environment = PVisorEnvironment(
            rootfs=prepared['rootfs'], firmware=config['firmware'], binary=config['binary'],
            output=trial_dir / 'vm', python=prepared.get('python', '/usr/bin/python3'),
            cpus=task.config.environment.cpus or 2,
            memory_mib=task.config.environment.memory_mb or 7168,
            env=prepared.get('environment', {}),
            apparmor_profile=config.get('apparmor_profile'),
            krun_enomem_workaround=config.get('krun_enomem_workaround'),
            host_http_proxy=config.get('host_http_proxy'),
            base_commit=prepared.get('base_commit'),
            repository_initialized=prepared.get('repository_initialized', False),
            workdir=task.config.environment.workdir or prepared.get('workdir') or '/')
        trial_config = TrialConfig(
            trial_name=entry['name'], task_dir=Path(config['task_dir']) / entry['directory'],
            trial_dir=trial_dir,
            agent=AgentTrialConfig(name='oracle') if oracle else AgentTrialConfig(name=HostMiniAgent.name(),
                                   model_name=model['model'],
                                   model_base_url=model['base_url'],
                                   kwargs={'max_steps': config.get('max_steps', 200),
                                           'profile': model['profile']}),
            environment=EnvironmentTrialConfig(backend='pvisor', mount_logs=False),
            environment_start_timeout_sec=900,
        )

        async def phase(hook_event):
            event(environment.events, hook_event.event_code)

        trial = SingleStepTrial(trial_config, environment_factory=lambda: environment,
                                hooks={name: [phase] for name in TrialEvent})
        try:
            result = await trial.run()
            summaries.append(result.model_dump(mode='json'))
        finally:
            await environment.stop()
            (output / 'summary.json').write_text(json.dumps(summaries, indent=2, default=str))
    if oracle:
        passed = len(summaries) == len(plan) and all(
            not result.get('exception_info') and
            (result.get('verifier_result') or {}).get('rewards', {}).get('reward') == 1
            for result in summaries)
        (output / 'oracle-gate.json').write_text(json.dumps(dict(
            status='passed' if passed else 'failed',
            krun_enomem_workaround=config.get('krun_enomem_workaround'),
            images={e['name']: p['image_digest'] for e, _, p in plan},
            task_results=str(output / 'summary.json')), indent=2))
        if not passed:
            raise RuntimeError('oracle validation failed; retained results must be inspected')
    return summaries


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--oracle', action='store_true', help='run reference patches without a model')
    parser.add_argument('--plan', action='store_true', help='validate only; no model/VM calls')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    config = load_config(args.config)
    if args.plan:
        print(json.dumps([dict(name=e['name'], task=t.name,
                               image=e.get('image', t.config.environment.docker_image),
                               source_image=t.config.environment.docker_image,
                               prepared=p is not None)
                          for e, t, p in load_plan(config)], indent=2))
        return
    if args.output is None:
        parser.error('--output must be a new directory under /data')
    if not args.output.resolve().is_relative_to('/data'):
        parser.error('build and experiment data must stay under /data')
    asyncio.run(evaluate(config, args.output.resolve(), oracle=args.oracle))


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception as exc:
        print(json.dumps(dict(event='failed', error_type=type(exc).__name__,
                              error=str(exc), unix_ns=time.time_ns())), file=sys.stderr)
        raise
