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


@pytest.mark.parametrize("fault", [None, "missing-file", "truncated-file"])
def test_direct_control_requires_writes_in_workspace(tmp_path, fault):
    written = tmp_path / "_fs/written"
    written.mkdir(parents=True)
    for i in range(256):
        with (written / f"{i:04d}").open("wb") as file:
            file.truncate(64 * 1024)
    if fault == "missing-file":
        (written / "0000").unlink()
    elif fault == "truncated-file":
        (written / "0000").write_bytes(b"incomplete")
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


@pytest.mark.parametrize("fault", [None, "lower-write", "missing-upper", "truncated-upper"])
@pytest.mark.parametrize("file_kib", [60, 64])
def test_successful_workload_still_requires_staged_writes(tmp_path, fault, file_kib):
    work, stage = tmp_path / "work", tmp_path / "stage"
    written = stage / "upper/_fs/written"
    written.mkdir(parents=True)
    for i in range(256):
        with (written / f"{i:04d}").open("wb") as file:
            file.truncate(file_kib * 1024)
    if fault == "lower-write":
        (work / "_fs/written").mkdir(parents=True)
    elif fault == "missing-upper":
        (written / "0000").unlink()
    elif fault == "truncated-upper":
        (written / "0000").write_bytes(b"incomplete")
    if fault:
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
            return {'witnesses': [{'pid': pid} for pid in pids]}

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


def test_budget_requires_explicit_cpu_placement_before_launch(tmp_path):
    from types import SimpleNamespace
    from reference_baselines import reference_budget

    args = SimpleNamespace(resource_budget=tmp_path / 'private.slice', cpu_affinity='')
    with pytest.raises(ValueError, match='explicit CPU affinity'):
        reference_budget(args)


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
            raise BudgetViolation('controlled negative: shim escaped parent')

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
    assert record['unknown_observations'] == []
    assert json.loads((trial / 'command.json').read_text())['exit'] == 0
    assert (trial / 'workspace').exists()
