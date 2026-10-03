"""Reject fast failure and markers that did not complete an Agent tool loop."""

import pytest
from reference_baselines import validate_guest_output
from reference_workload import grade_returned


def guest(mode="tools", exit_code=0, correctness="passed"):
    return f'REFERENCE_READY\r\nREFERENCE_RESULT {{"mode":"{mode}","correctness":"{correctness}"}}\r\nREFERENCE_EXIT {exit_code}\r\n'


def test_guest_accepts_successful_task_and_shutdown():
    validate_guest_output(guest(), "tools")


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
