"""Reject fast failure and markers that did not complete an Agent tool loop."""

import pytest
from reference_baselines import (
    validate_bundle_execution,
    validate_direct_filesystem,
    validate_guest_output,
    validate_staged_filesystem,
)
from reference_workload import grade_returned


def guest(mode="tools", exit_code=0, correctness="passed"):
    return f'REFERENCE_READY\r\nREFERENCE_RESULT {{"mode":"{mode}","correctness":"{correctness}"}}\r\nREFERENCE_EXIT {exit_code}\r\n'


def test_guest_accepts_successful_task_and_shutdown():
    validate_guest_output(guest(), "tools")


@pytest.mark.parametrize('output', [
    guest() + 'REFERENCE_EXIT 0\n',
    guest() + 'REFERENCE_EXIT 1\n',
    guest().replace('REFERENCE_READY', 'REFERENCE_READY invalid'),
    guest().replace('REFERENCE_READY\r\n', ''),
    guest() + 'REFERENCE_READY\n',
    guest() + 'REFERENCE_RESULT invalid\n',
    guest() + 'kernel panic - late failure\n',
    guest().replace('REFERENCE_READY\r\n', '') + 'REFERENCE_READY\n',
])
def test_guest_requires_unique_ordered_markers(output):
    with pytest.raises(ValueError):
        validate_guest_output(output, 'tools')


@pytest.mark.parametrize("fault", [None, "missing-file", "truncated-file", "wrong-content", "symlink"])
def test_direct_control_requires_writes_in_workspace(tmp_path, fault):
    written = tmp_path / "_fs/written"
    written.mkdir(parents=True)
    for i in range(256):
        pattern = b"pvisor-workload\n"
        (written / f"{i:04d}").write_bytes((pattern * 4370)[:64 * 1024])
    if fault == "missing-file":
        (written / "0000").unlink()
    elif fault == "truncated-file":
        (written / "0000").write_bytes(b"incomplete")
    elif fault == "wrong-content":
        (written / "0000").write_bytes(b"x" * (64 * 1024))
    elif fault == "symlink":
        (written / "0000").unlink()
        (written / "0000").symlink_to("0001")
    if fault:
        with pytest.raises(ValueError):
            validate_direct_filesystem(tmp_path, 256 * 64 * 1024)
    else:
        validate_direct_filesystem(tmp_path, 256 * 64 * 1024)


@pytest.mark.parametrize("isolation,staged", [
    ("rootless_process", False), ("host_process", False), ("rootless_process", True),
])
def test_nonstaged_host_requires_declared_boundary_and_direct_writes(isolation, staged):
    bundle = {"run": {"state": "completed", "exit_code": 0,
                      "executor": {"isolation": isolation}},
              "safety": {"filesystem_changes_staged": staged}}
    if isolation == "rootless_process" and not staged:
        validate_bundle_execution(bundle, "pvisor-host", host_isolation="rootless_process")
    else:
        with pytest.raises(AssertionError):
            validate_bundle_execution(bundle, "pvisor-host", host_isolation="rootless_process")


@pytest.mark.parametrize(
    "output",
    [
        guest() + "Kernel panic - not syncing: Attempted to kill init!\n",
        guest(exit_code=1),
        guest(mode="ready"),
        guest(correctness="failed"),
        guest() + guest(),
        guest().replace("REFERENCE_EXIT 0", "REFERENCE_EXIT 127"),
    ],
)
def test_vmm_zero_exit_cannot_hide_guest_failure(output):
    with pytest.raises(ValueError):
        validate_guest_output(output, "tools")


def test_grade_marker_in_prompt_is_not_a_tool_result():
    requests = [
        {
            "body": {
                "input": [{"type": "message", "role": "user", "content": "REFERENCE_GRADE_PASS"}]
            }
        }
    ]
    assert not grade_returned(requests)


@pytest.mark.parametrize("code", [1, 127])
def test_codex_grade_requires_tool_exit_zero(code):
    assert not grade_returned(
        [
            {
                "body": {
                    "input": [
                        {
                            "type": "function_call_output",
                            "output": f"Process exited with code {code}\nREFERENCE_GRADE_PASS",
                        }
                    ]
                }
            }
        ]
    )


def test_codex_tool_result_returns_grade_to_model():
    assert grade_returned(
        [
            {
                "body": {
                    "input": [
                        {
                            "type": "function_call_output",
                            "output": "Process exited with code 0\nREFERENCE_GRADE_PASS",
                        }
                    ]
                }
            }
        ]
    )


@pytest.mark.parametrize("error", [True, False])
def test_claude_tool_result_must_succeed(error):
    assert (
        grade_returned(
            [
                {
                    "body": {
                        "messages": [
                            {
                                "role": "user",
                                "content": [
                                    {
                                        "type": "tool_result",
                                        "is_error": error,
                                        "content": "REFERENCE_GRADE_PASS",
                                    }
                                ],
                            }
                        ]
                    }
                }
            ]
        )
        is not error
    )


@pytest.mark.parametrize("fault", [None, "lower-write", "missing-upper", "truncated-upper", "wrong-content"])
@pytest.mark.parametrize("file_kib", [60, 64])
def test_successful_workload_still_requires_staged_writes(tmp_path, fault, file_kib):
    work, stage = tmp_path / "work", tmp_path / "stage"
    written = stage / "upper/_fs/written"
    written.mkdir(parents=True)
    for i in range(256):
        pattern = b"pvisor-workload\n"
        (written / f"{i:04d}").write_bytes((pattern * 4370)[:file_kib * 1024])
    if fault == "lower-write":
        (work / "_fs/written").mkdir(parents=True)
    elif fault == "missing-upper":
        (written / "0000").unlink()
    elif fault == "truncated-upper":
        (written / "0000").write_bytes(b"incomplete")
    elif fault == "wrong-content":
        (written / "0000").write_bytes(b"x" * (file_kib * 1024))
    if fault or file_kib != 64:
        with pytest.raises(ValueError):
            validate_staged_filesystem(work, stage, file_kib * 1024 * 256)
    else:
        validate_staged_filesystem(work, stage, file_kib * 1024 * 256)


def test_one_registered_benchmark_per_invocation():
    from reference_baselines import benchmark_for_modes
    assert benchmark_for_modes('ready') == 'B-STARTUP'
    assert benchmark_for_modes('env,tools,claude,codex') == 'B-AGENT-TASK'
    with pytest.raises(ValueError,match='one benchmark ID'):
        benchmark_for_modes('ready,filesystem')
    with pytest.raises(ValueError,match='unknown'):
        benchmark_for_modes('typo')


def test_build_receipt_cannot_label_an_unrelated_binary(tmp_path):
    import hashlib
    import json
    from reference_baselines import verified_build_receipt

    binary = tmp_path / 'pvisor'
    binary.write_bytes(b'current binary')
    manifest = tmp_path / 'source-manifest.json'
    manifest.write_text('[]\n')
    receipt = tmp_path / 'build-receipt.json'
    record = dict(pvisor_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  source_manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest())
    receipt.write_text(json.dumps(record))
    assert verified_build_receipt(receipt, binary) == record
    binary.write_bytes(b'older binary')
    with pytest.raises(ValueError, match='measured pvisor binary'):
        verified_build_receipt(receipt, binary)
    binary.write_bytes(b'current binary')
    manifest.write_text('["different source"]')
    with pytest.raises(ValueError, match='source manifest'):
        verified_build_receipt(receipt, binary)


def test_budget_oom_rejects_successful_native_command_and_retains_scene(tmp_path, monkeypatch):
    import json
    from types import SimpleNamespace
    import reference_baselines as runner

    assets = tmp_path / 'assets'
    work = assets / 'rootfs/work'
    work.mkdir(parents=True)
    (work / 'retained-input').write_text('required original evidence')
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets,
                           resource_budget=tmp_path / 'private.slice',
                           cpu_affinity='', docker_root_pid=None)

    class OomBudget:
        reads = 0

        def read(self):
            self.reads += 1
            return dict(cpu_stat={'usage_usec': self.reads * 100},
                        memory_events={'oom': int(self.reads > 1), 'oom_kill': 0})

        def processes(self, pids):
            return {'witnesses': [{'pid': pid, 'start_ticks': 1,
                                  'cgroup': '/controlled-test'} for pid in pids]}

        def witness_all_members(self, pids):
            return self.processes(pids)

    monkeypatch.setattr(runner, 'reference_budget', lambda _: OomBudget())
    with pytest.raises(RuntimeError, match='resource-budget OOM'):
        runner.run_trial(args, {'assets': {'docker_image': 'unused-native-control'}},
                         'native', 'ready', 0)
    trial = args.output / 'trials/ready-native-000'
    assert json.loads((trial / 'command.json').read_text())['exit'] == 0
    evidence = json.loads((trial / 'resource-budget.json').read_text())
    assert evidence['memory_events_delta']['oom'] == 1
    assert (trial / 'workspace/retained-input').read_text() == 'required original evidence'


def test_unobserved_timing_does_not_scan_processes_or_report_zero_rss(tmp_path, monkeypatch):
    from types import SimpleNamespace
    import reference_baselines as runner

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets,
                           resource_budget=None, cpu_affinity='', docker_root_pid=None,
                           resource_observation='off')

    def reject_scan(*args, **kwargs):
        raise AssertionError('periodic process scan perturbed the timing')

    monkeypatch.setattr(runner, 'snapshot', reject_scan)
    row = runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, 'native', 'ready', 0)
    assert row['correctness'] == 'passed'
    assert row['resource_observation'] == 'off'
    assert row['peak_tree_rss_kib'] is None
    assert 'unknown' in row['memory_scope']


def test_budget_requires_explicit_cpu_placement_before_launch(tmp_path):
    from types import SimpleNamespace
    from reference_baselines import reference_budget

    args = SimpleNamespace(resource_budget=tmp_path / 'private.slice', cpu_affinity='')
    with pytest.raises(ValueError, match='explicit CPU affinity'):
        reference_budget(args)


def test_diagnostic_preserves_large_nonblocking_stderr_without_pipe_failure(tmp_path, monkeypatch):
    import sys
    from types import SimpleNamespace
    import reference_baselines as runner

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets,
                           resource_budget=None, cpu_affinity='', docker_root_pid=None,
                           resource_observation='off', diagnostic_timing=True,
                           diagnostic_stderr_file=True)
    payload = b'profile bytes\x00\xff\n' * 65536
    original = runner.subprocess.Popen

    def launch(argv, **kwargs):
        if argv[0] == 'cp':
            return original(argv, **kwargs)
        assert kwargs['stderr'] != runner.subprocess.PIPE
        script = (
            "import os;os.set_blocking(2,False);"
            f"assert os.write(2,{payload[:16]!r}*65536)=={len(payload)};"
            "print('REFERENCE_READY',flush=True);"
            "print('REFERENCE_RESULT {\"mode\":\"ready\",\"correctness\":\"passed\"}',flush=True)"
        )
        return original([sys.executable, '-c', script], **kwargs)

    monkeypatch.setattr(runner.subprocess, 'Popen', launch)
    row = runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, 'native', 'ready', 0)
    assert row['correctness'] == 'passed'
    assert row['stderr_capture'] == 'regular-file diagnostic'
    assert (args.output / 'trials/ready-native-000/stderr.log').read_bytes() == payload


def test_regular_file_stderr_cannot_change_formal_timing(tmp_path):
    from types import SimpleNamespace
    import reference_baselines as runner

    args = SimpleNamespace(output=tmp_path / 'output', diagnostic_stderr_file=True)
    with pytest.raises(ValueError, match='diagnostic only'):
        runner.run_trial(args, {}, 'native', 'ready', 0)
    assert not args.output.exists()


def test_observed_budget_violation_cannot_be_published_as_unknown(tmp_path, monkeypatch):
    import json
    from types import SimpleNamespace
    import reference_baselines as runner
    from resource_budget import BudgetViolation

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets,
                           resource_budget=tmp_path / 'private.slice',
                           cpu_affinity='', docker_root_pid=None)

    class EscapedBudget:
        def read(self):
            return dict(cpu_stat={'usage_usec': 100}, memory_events={'oom': 0})

        def processes(self, pids):
            return {'witnesses': [{'pid': pid} for pid in pids]}

        def witness_all_members(self, pids):
            raise BudgetViolation('controlled negative: shim escaped parent',
                                  evidence={'pid': 123, 'tid': 124, 'cpus': [0, 1, 2]})

    monkeypatch.setattr(runner, 'reference_budget', lambda _: EscapedBudget())
    original = runner.subprocess.Popen

    def launch(argv, *args, **kwargs):
        # Keep the real successful child alive long enough for this observation;
        # this test is a correctness control, not a benchmark timing sample.
        if argv[:2] == ['/bin/sh', '-c']:
            argv = [*argv[:-1], argv[-1] + '; sleep 0.08']
        return original(argv, *args, **kwargs)

    monkeypatch.setattr(runner.subprocess, 'Popen', launch)
    with pytest.raises(RuntimeError, match='resource-budget violation'):
        runner.run_trial(args, {'assets': {'docker_image': 'unused-native-control'}},
                         'native', 'ready', 0)
    trial = args.output / 'trials/ready-native-000'
    record = json.loads((trial / 'resource-budget.json').read_text())
    assert record['violations']
    assert record['violations'][0]['evidence'] == {'pid': 123, 'tid': 124, 'cpus': [0, 1, 2]}
    assert record['unknown_observations'] == []
    assert json.loads((trial / 'command.json').read_text())['exit'] == 0
    assert (trial / 'workspace').exists()


@pytest.mark.parametrize('backend', ['fc-system', 'fc-reference', 'firecracker'])
@pytest.mark.parametrize('policy', ['normal', 'ready-only'])
def test_fc_policy_keeps_generic_exit_gate_and_same_launch_config(tmp_path, monkeypatch, backend, policy):
    import json
    import sys
    from types import SimpleNamespace
    import reference_baselines as runner

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    (assets / 'agent-env.ext4').write_bytes(b'same prepared disk')
    (assets / 'vmlinux').write_bytes(b'legacy kernel')
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets, cpu_affinity='',
                           docker_root_pid=None, resource_budget=None, resource_observation='off',
                           fc_ready_policy=policy)
    identity = {'paths': {'kernel': '/frozen/kernel', 'initrd': '/frozen/initrd'},
                'record': {'variant': backend}}
    monkeypatch.setattr(runner, 'fc_kernel', lambda a, b: None if b == 'firecracker' else identity)
    original = runner.subprocess.Popen

    def launch(argv, **kwargs):
        if argv[0] == 'cp':
            return original(argv, **kwargs)
        assert argv[:3] == ['firecracker', '--enable-pci', '--no-api']
        config = json.loads((args.output / f'trials/ready-{backend}-000/firecracker.json').read_text())
        assert config['machine-config'] == {'vcpu_count': 2, 'mem_size_mib': 128}
        assert config['boot-source']['boot_args'] == 'console=ttyS0 reboot=k panic=1 root=/dev/vda rw init=/bench/init quiet pvbench.mode=ready pvbench.scratch=executor'
        assert config['logger']['level'] == 'Warning'
        assert (assets / 'agent-env.ext4').read_bytes() == b'same prepared disk'
        script = f"import os,time;os.write(1,{guest(mode='ready').encode()!r});"
        # A normal nonzero VMM exit is always rejected, even after valid markers.
        script += 'time.sleep(2)' if policy == 'ready-only' else 'raise SystemExit(7)'
        return original([sys.executable, '-c', script], **kwargs)

    monkeypatch.setattr(runner.subprocess, 'Popen', launch)
    if policy == 'normal':
        with pytest.raises(RuntimeError, match='exit 7'):
            runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, backend, 'ready', 0)
    else:
        row = runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, backend, 'ready', 0)
        assert row['completion_ms'] is None
        assert row['controlled_sigterm']
        assert row['ready_ms'] >= 0
        assert row['kernel_variant'] == ('legacy-reference/unknown' if backend == 'firecracker' else backend)


@pytest.mark.parametrize('output', [
    guest(mode='ready').replace('REFERENCE_EXIT 0', 'REFERENCE_EXIT 1'),
    guest(mode='ready').replace('REFERENCE_READY\r\n', ''),
    guest(mode='ready').replace('REFERENCE_READY', 'REFERENCE_READY\nREFERENCE_READY'),
    guest(mode='ready', correctness='failed'),
    'Kernel panic - failure\n' + guest(mode='ready'),
    guest(mode='ready').replace('{"mode":"ready","correctness":"passed"}', '[]'),
])
def test_ready_only_invalid_markers_never_authorize_sigterm(tmp_path, monkeypatch, output):
    import json
    import sys
    from types import SimpleNamespace
    import reference_baselines as runner

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    (assets / 'agent-env.ext4').write_bytes(b'disk')
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets, cpu_affinity='',
                           docker_root_pid=None, resource_budget=None, resource_observation='off',
                           fc_ready_policy='ready-only')
    original = runner.subprocess.Popen

    def launch(argv, **kwargs):
        if argv[0] == 'cp':
            return original(argv, **kwargs)
        return original([sys.executable, '-c', f'import os;os.write(1,{output.encode()!r})'], **kwargs)

    monkeypatch.setattr(runner.subprocess, 'Popen', launch)
    with pytest.raises((ValueError, RuntimeError, AssertionError)):
        runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, 'firecracker', 'ready', 0)
    command = json.loads((args.output / 'trials/ready-firecracker-000/command.json').read_text())
    assert not command['controlled_sigterm']


@pytest.mark.parametrize('backend', ['native', 'firecracker', 'fc-system', 'fc-reference'])
def test_ready_only_never_applies_to_other_workloads(tmp_path, backend):
    from types import SimpleNamespace
    from reference_baselines import run_trial
    with pytest.raises(ValueError, match='restricted to ready'):
        run_trial(SimpleNamespace(output=tmp_path, fc_ready_policy='ready-only'), {}, backend, 'tools', 0)


def test_public_cli_exposes_explicit_fc_variants_and_policy():
    import subprocess
    import sys
    from pathlib import Path
    help_text = subprocess.check_output([sys.executable, str(Path(__file__).with_name('reference_baselines.py')), '--help'], text=True)
    for option in ('--fc-system-receipt', '--fc-reference-receipt', '--fc-ready-policy', '--qemu-system-receipt'):
        assert option in help_text


@pytest.mark.parametrize('backend', ['qemu', 'qemu-microvm'])
@pytest.mark.parametrize('changed', [False, True])
def test_qemu_boots_stock_vmlinuz_and_initrd_and_rechecks_receipt(tmp_path, monkeypatch, backend, changed):
    import json
    import sys
    from types import SimpleNamespace
    import reference_baselines as runner

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    (assets / 'agent-env.ext4').write_bytes(b'same prepared disk')
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets, cpu_affinity='',
                           resource_budget=None, resource_observation='off',
                           qemu_system_receipt=tmp_path / 'receipt.json')
    identity = {'paths': {'vmlinuz': '/frozen/stock-vmlinuz', 'initrd': '/frozen/stock-initrd'},
                'record': {'variant': 'fc-system'}}
    calls = []

    def verify(path, kind):
        assert path == args.qemu_system_receipt and kind == 'fc-system'
        calls.append(path)
        if changed and len(calls) > 1:
            raise ValueError('kernel artifact hash mismatch: vmlinuz')
        return identity

    monkeypatch.setattr(runner, 'verify_kernel_receipt', verify)
    original = runner.subprocess.Popen

    def launch(argv, **kwargs):
        if argv[0] == 'cp':
            return original(argv, **kwargs)
        assert argv[0] == 'qemu-system-x86_64'
        assert argv[argv.index('-kernel') + 1] == '/frozen/stock-vmlinuz'
        assert argv[argv.index('-initrd') + 1] == '/frozen/stock-initrd'
        if backend == 'qemu-microvm':
            assert argv[argv.index('-machine') + 1] == 'microvm,acpi=off,x-option-roms=off,pit=off,pic=off,rtc=on'
        assert argv[argv.index('-smp') + 1] == '2'
        assert argv[argv.index('-m') + 1] == '128'
        return original([sys.executable, '-c', f'import os;os.write(1,{guest(mode="ready").encode()!r})'], **kwargs)

    monkeypatch.setattr(runner.subprocess, 'Popen', launch)
    if changed:
        with pytest.raises(ValueError, match='hash mismatch'):
            runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, backend, 'ready', 0)
    else:
        row = runner.run_trial(args, {'assets': {'docker_image': 'unused'}}, backend, 'ready', 0)
        assert row['kernel_variant'] == 'qemu-system'
        assert row['kernel_provenance'] == identity
        assert row['completion_ms'] is not None
    assert len(calls) == 2


def test_same_pid_later_thread_affinity_violation_is_rejected(tmp_path, monkeypatch):
    import json
    from types import SimpleNamespace
    import reference_baselines as runner
    from resource_budget import BudgetViolation

    assets = tmp_path / 'assets'
    (assets / 'rootfs/work').mkdir(parents=True)
    args = SimpleNamespace(output=tmp_path / 'output', assets=assets,
                           resource_budget=tmp_path / 'private.slice',
                           cpu_affinity='', docker_root_pid=None)

    class ChangedAffinityBudget:
        observations = 0

        def read(self):
            return dict(cpu_stat={'usage_usec': 100}, memory_events={'oom': 0})

        def processes(self, pids):
            return {'witnesses': [{'pid': pid, 'start_ticks': 1,
                                  'cgroup': '/controlled-test'} for pid in pids]}

        def witness_all_members(self, pids):
            self.observations += 1
            if self.observations > 1:
                raise BudgetViolation('existing PID created an unrestricted thread')
            return self.processes(pids)

    budget = ChangedAffinityBudget()
    monkeypatch.setattr(runner, 'reference_budget', lambda _: budget)
    original = runner.subprocess.Popen

    def launch(argv, *args, **kwargs):
        if argv[:2] == ['/bin/sh', '-c']:
            argv = [*argv[:-1], argv[-1] + '; sleep 0.12']
        return original(argv, *args, **kwargs)

    monkeypatch.setattr(runner.subprocess, 'Popen', launch)
    with pytest.raises(RuntimeError, match='resource-budget violation'):
        runner.run_trial(args, {'assets': {'docker_image': 'unused-native-control'}},
                         'native', 'ready', 0)
    trial = args.output / 'trials/ready-native-000'
    record = json.loads((trial / 'resource-budget.json').read_text())
    assert record['live_observations']
    assert record['violations']
    assert record['unknown_observations'] == []
    assert json.loads((trial / 'command.json').read_text())['exit'] == 0
