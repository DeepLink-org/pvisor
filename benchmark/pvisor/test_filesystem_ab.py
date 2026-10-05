"""A skipped or failed backend must not yield a successful A/B summary."""

import pytest
from filesystem_ab import WORKLOADS, summarize, validate_binaries


def test_stale_build_or_wrong_manifest_cannot_enter_the_ab_run():
    with pytest.raises(ValueError, match="identical hashes"):
        validate_binaries({"baseline": "same", "candidate": "same"})
    hashes = {"baseline": "old", "candidate": "new"}
    validate_binaries(hashes, {"binary_sha256": hashes})
    with pytest.raises(ValueError, match="manifest does not match"):
        validate_binaries(hashes, {"binary_sha256": {"baseline": "old", "candidate": "other"}})


def test_incomplete_matrix_cannot_publish_percentiles():
    cells = [("baseline", "pvisor-vm"), ("candidate", "pvisor-vm")]
    rows = [{"variant": "baseline", "backend": "pvisor-vm"}]
    with pytest.raises(ValueError, match="incomplete sample matrix"):
        summarize(rows, cells, 1)


def test_warmed_samples_are_separate_from_completion_time():
    cells = [("baseline", "pvisor-vm"), ("candidate", "pvisor-vm")]
    rows = [
        {
            "variant": variant,
            "backend": backend,
            "completion_ms": 100 + duration,
            "result": {"filesystem": {mode: {"worker_ms": duration} for mode in WORKLOADS}},
        }
        for variant, backend in cells
        for duration in (10, 30)
    ]
    result = summarize(rows, cells, 2)
    assert result["candidate/pvisor-vm"]["n"] == 2
    assert result["baseline/pvisor-vm"]["timings_ms"]["git"]["p50"] == 20
    assert result["baseline/pvisor-vm"]["timings_ms"]["completion_ms"]["p50"] == 120
