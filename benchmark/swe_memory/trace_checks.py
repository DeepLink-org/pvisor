"""Exact expected timeout matching, without allowing any new timeout position."""


def timeout_signature(events):
    starts = [e["host_monotonic_ns"] for e in events if e["event"] == "trial.agent_start"]
    ends = [e["host_monotonic_ns"] for e in events if e["event"] == "trial.agent_end"]
    assert len(starts) == len(ends) == 1 and starts[0] < ends[0]
    inside = []
    outside = []
    for e in events:
        if e["event"] != "tool_end":
            continue
        flag = bool(e.get("timed_out"))
        if starts[0] <= e["host_monotonic_ns"] <= ends[0]:
            inside.append(flag)
        elif flag:
            outside.append(e)
    return inside, outside


def compare_timeouts(events, source_events):
    actual, outside = timeout_signature(events)
    expected, _ = timeout_signature(source_events)
    return dict(
        expected_timeout_indices=[i for i, x in enumerate(expected) if x],
        actual_timeout_indices=[i for i, x in enumerate(actual) if x],
        source_tool_count=len(expected),
        replay_tool_count=len(actual),
        flags_match=actual == expected,
        outside_agent_timeouts=outside,
        passed=actual == expected and not outside,
    )
