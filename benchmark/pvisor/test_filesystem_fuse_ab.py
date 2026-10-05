"""A purported FUSE control must prove both mounting and actual data service."""
import json
import pytest
from reference_baselines import validate_passthrough_output


def evidence(stats=None, mount=" - fuse.pvisor-bench-native pvisor-bench-native rw"):
    values = {"lookup": 2048, "getattr": 2048, "read": 1024, "write": 256,
              "read_bytes": 64 * 1024 * 1024, "write_bytes": 256 * 64 * 1024}
    values.update(stats or {})
    return "PASSTHROUGH_MOUNT 123 1 0:1 / /mount rw" + mount + "\nPASSTHROUGH_STATS " + json.dumps(values)


def test_real_fuse_request_evidence_is_required():
    assert validate_passthrough_output(evidence())["lookup"] == 2048


@pytest.mark.parametrize("fault", ["missing-mount", "not-fuse", "missing-stats", "duplicate-stats",
                                   "no-lookup", "no-read", "no-write", "short-read", "short-write"])
def test_bypass_or_partial_data_service_is_rejected(fault):
    value = evidence()
    if fault == "missing-mount":
        value = value.split("\n", 1)[1]
    elif fault == "not-fuse":
        value = evidence(mount=" - ext4 /dev/loop0 rw")
    elif fault == "missing-stats":
        value = value.split("\n", 1)[0]
    elif fault == "duplicate-stats":
        value += "\n" + value.split("\n", 1)[1]
    else:
        fields = {"no-lookup": "lookup", "no-read": "read", "no-write": "write",
                  "short-read": "read_bytes", "short-write": "write_bytes"}
        value = evidence({fields[fault]: 0})
    with pytest.raises(ValueError):
        validate_passthrough_output(value)
