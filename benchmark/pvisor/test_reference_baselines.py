"""Reject fast failure and markers that did not complete an Agent tool loop."""

import pytest
from reference_baselines import validate_direct_filesystem, validate_guest_output, validate_staged_filesystem
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
