"""Do not publish partial matrices or drop slow tool samples."""
import pytest
from filesystem_stage_ab import WORKLOADS, summarize


def row(backend, elapsed):
    return {"backend": backend, "completion_ms": elapsed, "ready_ms": elapsed / 10,
            "result": {"filesystem": {key: {"worker_ms": elapsed / 2} for key in WORKLOADS}}}


def test_summary_preserves_every_sample_and_tool():
    result = summarize([row("direct", 10), row("staged", 30), row("direct", 20),
                        row("staged", 50)], ["direct", "staged"], 2)
    assert result["direct"]["timings_ms"]["completion_ms"]["p50"] == 15
    assert result["staged"]["timings_ms"]["completion_ms"]["p50"] == 40
    assert result["direct"]["n"] == result["staged"]["n"] == 2
    assert all(result["staged"]["timings_ms"][key]["p50"] == 20 for key in WORKLOADS)


def test_summary_rejects_incomplete_matrix():
    with pytest.raises(ValueError, match="incomplete matrix"):
        summarize([row("direct", 10)], ["direct", "staged"], 1)
