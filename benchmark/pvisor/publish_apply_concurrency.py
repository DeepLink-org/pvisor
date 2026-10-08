#!/usr/bin/env python3
"""Publish B-APPLY concurrent-write correctness, separately from latency."""
import argparse
import json
from pathlib import Path
import re

from publication import write_csv
from publish_density import verify_harness
from reference_baselines import digest, verified_build_receipt


def planned_attempts(report):
    if report.get('benchmark_id') != 'B-APPLY':
        raise ValueError('not an apply report')
    protocol = report['concurrent_apply_protocol']
    count, repetitions = protocol['files'], protocol['repetitions']
    if type(count) is not int or count < 1000 or type(repetitions) is not int or repetitions != 3:
        raise ValueError('publication requires three probes with at least1000files')
    rows = report['rows'] + report.get('capabilities', {}).get('apply/concurrent-conflicts', {}).get('failures', [])
    keys = [row['trial'] for row in rows]
    if any(type(key) is not int for key in keys) or len(keys) != len(set(keys)) or set(keys) != set(range(repetitions)):
        raise ValueError('missing or duplicate concurrent-write attempts')
    if any(row['files'] != count or row['workload'] != 'conflict-during-target-writes'
           or row['backend'] != 'staged' for row in rows):
        raise ValueError('wrong concurrent-write condition')
    return count, rows


def publish(path, output):
    path = path.resolve()
    report = json.loads(path.read_text())
    count, rows = planned_attempts(report)
    receipt = verified_build_receipt(path.parent / 'build-receipt.json', path.parent / 'bin/pvisor')
    if receipt != report['binary_build'] or receipt['pvisor_sha256'] != report['binary_sha256']:
        raise ValueError('report disagrees with retained build')
    verify_harness(report, path.parent)
    valid = preserved = detected = overwritten = unknown = 0
    evidence = []
    for row in rows:
        root = Path(row['logs']).resolve()
        if not root.is_relative_to(path.parent / 'trials'):
            raise ValueError('trial artifacts outside retained cohort')
        if json.loads((root / 'result.json').read_text()) != row:
            raise ValueError('retained attempt disagrees with report')
        injection_path = root / 'injection.json'
        if not injection_path.exists():
            if row.get('correctness') == 'passed':
                raise ValueError('successful probe has no injection')
            unknown += 1
            continue
        injection = json.loads(injection_path.read_text())
        for key in ('files', 'trial', 'state_at_injection', 'injected_path', 'injected_content',
                    'already_applied_before_injection'):
            if injection[key] != row[key]:
                raise ValueError('injection evidence disagrees with outcome')
        match = re.fullmatch(r'files/f(\d{6,})', row['injected_path'])
        if (not match or int(match[1]) >= count or row['state_at_injection'] != 'prepared'
                or type(row['already_applied_before_injection']) is not int
                or not 0 < row['already_applied_before_injection'] < count):
            raise ValueError('invalid actual-write injection evidence')
        workspace = root / 'workspace'
        final = (workspace / row['injected_path']).read_text()
        ledger_path = root / 'stage/apply-ledger.json'
        ledger = json.loads(ledger_path.read_text())['records'][-1]['state']
        stderr = (root / 'apply.stderr').read_text()
        if final != row['final_injected_content'] or ledger != row['ledger_final']:
            raise ValueError('retained target or ledger disagrees with outcome')
        keep = final == row['injected_content']
        refusal = (row.get('exit_code') not in (None, 0)
                   and 'target changed after staging at' in stderr and Path(row['injected_path']).name in stderr)
        if row.get('host_edit_preserved') != keep or row.get('detected_conflict') != refusal:
            raise ValueError('incorrect preservation or conflict classification')
        if row.get('correctness') == 'passed' and not (keep and refusal and ledger == 'prepared'):
            raise ValueError('incorrectly successful concurrent-write probe')
        for index in range(count):
            target = workspace / 'files' / f'f{index:06d}'
            if target.is_symlink() or not target.resolve().is_relative_to(workspace.resolve()):
                raise ValueError('target evidence escapes retained workspace')
            allowed = (f'old-{index}\n', f'new-{index}\n')
            if index == int(match[1]):
                allowed += (row['injected_content'],)
            if target.read_text() not in allowed:
                raise ValueError('unexpected partial-apply content')
        if ledger == 'prepared':
            upper = root / 'stage/upper/files'
            if len(list(upper.iterdir())) != count:
                raise ValueError('incomplete retained upper after refusal')
            for index in range(count):
                item = upper / f'f{index:06d}'
                if item.is_symlink() or item.read_text() != f'new-{index}\n':
                    raise ValueError('retained upper changed after refusal')
        valid += 1
        preserved += keep
        detected += refusal
        overwritten += not keep and row.get('exit_code') == 0 and ledger == 'committed'
        unknown += row.get('exit_code') is None
        evidence.append(dict(trial=row['trial'], injection_sha256=digest(injection_path),
                             result_sha256=digest(root / 'result.json'),
                             target_sha256=digest(workspace / row['injected_path']),
                             ledger_sha256=digest(ledger_path), stderr_sha256=digest(root / 'apply.stderr')))
    audit = dict(state='passed', product_result='passed' if detected == preserved == 3 else 'failed-or-untested',
                 report_sha256=digest(path), evidence=evidence,
                 scope='complete planned grid, retained injection/outcome/target/ledger, all target contents and Prepared upper contents; no exhaustive rename-race guarantee')
    audit_path = path.parent / 'concurrent-publication-audit.json'
    audit_path.write_text(json.dumps(audit, indent=2) + '\n')
    result = dict(benchmark_id='B-APPLY', cohort=path.parent.name, measured_at=report['recorded_at'],
                  files=count, planned_probes=3, valid_injections=valid, conflicts_detected=detected,
                  host_edits_preserved=preserved, silent_overwrites=overwritten, unknown_probes=unknown,
                  binary_sha256=report['binary_sha256'], source_manifest_sha256=receipt['source_manifest_sha256'],
                  report_sha256=digest(path), audit_sha256=digest(audit_path),
                  cpu_affinity=report['cli_arguments']['cpu_affinity'],
                  scope='actual target-write window,owned SIGSTOP,external fsynced edit,owned SIGCONT;correctness only;not exhaustive rename races')
    output.mkdir(parents=True, exist_ok=True)
    write_csv(output / 'apply-concurrent-conflicts.csv', [result])
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(publish(args.report, args.output), indent=2))
