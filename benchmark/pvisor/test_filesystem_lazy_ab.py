import hashlib
import json

import pytest
from filesystem_lazy_ab import IMAGE_DIGEST, SMALL, TIMINGS, ImageFixture, validate_trial


def test_cache_fixture_preserves_stat_and_verified_slice_contract(tmp_path):
    source = tmp_path / "source"
    fixture = tmp_path / "fixture"
    source.mkdir()
    fixture.mkdir()
    (fixture / "small").write_bytes(SMALL)
    image = ImageFixture(source, fixture)
    path = list(b"lazy-bench/small")
    response, body = image.request(dict(op="stat", digest=IMAGE_DIGEST, path=path))
    assert response["size"] == len(SMALL) and response["kind"] == "file"
    assert body == b"" and image.snapshot()["read"] == 0
    response, body = image.request(
        dict(op="read", digest=IMAGE_DIGEST, path=path, offset=7, length=19)
    )
    assert body == SMALL[7:26]
    assert response["sha256"] == "sha256:" + hashlib.sha256(body).hexdigest()
    response, _ = image.request(dict(op="stat", digest=IMAGE_DIGEST, path=list(b"../source")))
    assert response["status"] == "error"
    response, _ = image.request(dict(op="read", digest="other", path=path, offset=0, length=1))
    assert response["status"] == "error"


def test_guest_success_does_not_bypass_isolation_staging_and_source_checks(tmp_path):
    fixture = tmp_path / "fixture"
    upper = tmp_path / "upper"
    (fixture / "tree").mkdir(parents=True)
    upper.mkdir()
    source = fixture / "tree/file.txt"
    source.write_bytes(SMALL)
    proof = upper / "workspace-proof"
    proof.write_bytes(b"changed in upper\n")
    bundle = {
        "run": {"state": "completed", "exit_code": 0, "executor": {"isolation": "virtual_machine"}},
        "safety": {"filesystem_changes_staged": True},
    }
    result = dict(correctness="passed", timings_ms={key: 1.0 for key in TIMINGS})
    output = "LAZY_RESULT " + json.dumps(result)
    validate_trial(output, bundle, upper, fixture)
    source.write_bytes(b"changed in lower")
    with pytest.raises(ValueError, match="source was modified"):
        validate_trial(output, bundle, upper, fixture)
    source.write_bytes(SMALL)
    proof.write_bytes(b"incorrect")
    with pytest.raises(ValueError, match="not staged"):
        validate_trial(output, bundle, upper, fixture)
    proof.write_bytes(b"changed in upper\n")
    bundle["run"]["executor"]["isolation"] = "host_process"
    with pytest.raises(AssertionError):
        validate_trial(output, bundle, upper, fixture)
    result["timings_ms"].pop("metadata")
    with pytest.raises(ValueError, match="timing matrix"):
        validate_trial("LAZY_RESULT " + json.dumps(result), bundle, upper, fixture)
