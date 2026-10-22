"""Join host monotonic events and cgroup samples; no performance claims."""
import argparse
import csv
import json
from pathlib import Path


def analyze(root):
    samples = [json.loads(line) for line in (root / 'resources.jsonl').read_text().splitlines()]
    events = sorted((json.loads(line) for line in (root / 'events.jsonl').read_text().splitlines()),
                    key=lambda e: e['host_monotonic_ns'])
    cursor = 0
    stage, model, tools = 'startup', False, set()
    rows = []
    for first, last in zip(samples, samples[1:]):
        midpoint = (first['host_monotonic_ns'] + last['host_monotonic_ns']) // 2
        while cursor < len(events) and events[cursor]['host_monotonic_ns'] <= midpoint:
            e = events[cursor]
            cursor += 1
            name = e['event']
            if name.startswith('trial.'):
                stage = name[6:]
            elif name == 'model_request_start':
                model = True
            elif name in ('model_request_end', 'model_request_error'):
                model = False
            elif name == 'tool_start':
                tools.add(e['execution_id'])
            elif name in ('tool_end', 'tool_transport_error'):
                tools.discard(e['execution_id'])
        seconds = (last['host_monotonic_ns'] - first['host_monotonic_ns']) / 1e9
        cpu_seconds = (last['cpu']['usage_usec'] - first['cpu']['usage_usec']) / 1e6
        if seconds <= 0 or cpu_seconds < 0:
            raise ValueError('resource timestamps/counters must be monotonic')
        phase = 'model_wait' if model else 'tool' if tools else stage
        mean_memory = (first['memory_current'] + last['memory_current']) / 2
        rows.append(dict(host_monotonic_ns=midpoint, phase=phase, trial_stage=stage, duration_s=seconds,
                         cpu_core_seconds=cpu_seconds, cpu_cores=cpu_seconds / seconds,
                         memory_mib=mean_memory / 1024**2,
                         memory_gib_seconds=mean_memory / 1024**3 * seconds))
    with (root / 'timeline.csv').open('w') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]) if rows else ['phase'])
        writer.writeheader()
        writer.writerows(rows)
    phases = {}
    for row in rows:
        total = phases.setdefault(row['phase'], dict(duration_s=0, cpu_core_seconds=0,
                                                     memory_gib_seconds=0))
        for key in total:
            total[key] += row[key]
    for total in phases.values():
        total['mean_cpu_cores'] = total['cpu_core_seconds'] / total['duration_s']
        total['mean_memory_mib'] = total['memory_gib_seconds'] * 1024 / total['duration_s']
    result = dict(phases=phases, sample_count=len(samples),
                  interval_assignment='midpoint; phase boundary uncertainty up to sample interval',
                  memory_scope='VM service cgroup incl. VMM/helpers, excluding host Agent/API server',
                  wait_scope='API request incl. queue, inference and network; not pure reasoning')
    (root / 'phase-summary.json').write_text(json.dumps(result, indent=2))
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('vm_directory', type=Path)
    print(json.dumps(analyze(parser.parse_args().vm_directory), indent=2))
