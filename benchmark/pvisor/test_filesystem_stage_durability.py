import pytest
from filesystem_stage_durability import validate_completion


@pytest.mark.parametrize("fault", [None, "missing-seal", "wrong-policy", "invalid-seal"])
def test_measurements_require_the_requested_policy_and_completed_persistence(tmp_path, fault):
    journal = tmp_path / "preimages"
    journal.mkdir()
    (journal / "durability-v1").write_bytes(b"pvisor.stage.checkpoint/1\n")
    (journal / "sealed-v1").write_bytes(b"pvisor.stage.sealed/1\n")
    if fault == "missing-seal":
        (journal / "sealed-v1").unlink()
    elif fault == "wrong-policy":
        (journal / "durability-v1").write_bytes(b"pvisor.stage.strict/1\n")
    elif fault == "invalid-seal":
        (journal / "sealed-v1").write_bytes(b"partial")
    if fault:
        with pytest.raises((ValueError, FileNotFoundError)):
            validate_completion(tmp_path, "checkpoint")
    else:
        validate_completion(tmp_path, "checkpoint")
