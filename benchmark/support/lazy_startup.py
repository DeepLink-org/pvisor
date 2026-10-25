"""Shared lazy startup contracts for active JSON benchmarks.

Extracted from the retired runners; no historical campaign entry point.
"""

import hashlib
import json
import os
import stat
from pathlib import Path

MARKER = b"LAZY_READY ubuntu 26.04\n"


WORKLOAD = '. /etc/os-release; test "$ID" = ubuntu && test "$VERSION_ID" = 26.04 || exit 41; printf "LAZY_READY %s %s\\n" "$ID" "$VERSION_ID"'


TORCH_MARKER = b"LAZY_READY python 3.10.14 torch 2.0.1+cpu cpu_sum 1240\n"


TORCH_WORKLOAD = """import sys
import torch
assert sys.version_info[:3] == (3, 10, 14), sys.version
assert torch.__version__ == "2.0.1+cpu", torch.__version__
assert torch.version.cuda is None, torch.version.cuda
torch.set_num_threads(1)
torch.set_num_interop_threads(1)
x = torch.arange(16, dtype=torch.int64, device="cpu")
assert x.square().sum().item() == 1240
print("LAZY_READY python 3.10.14 torch 2.0.1+cpu cpu_sum 1240", flush=True)
"""


NUMPY_SOURCE = "docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159"


NUMPY_MARKER = b"LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680\n"


NUMPY_WORKLOAD = """import sys
import numpy as np
assert sys.version_info[:3] == (3, 13, 14), sys.version
assert np.__version__ == "2.5.2", np.__version__
x = np.arange(16, dtype=np.int64).reshape(4, 4)
assert int(np.square(x).sum()) == 1240
assert int((x @ x.T).sum()) == 3680
print("LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680", flush=True)
"""


def workload(name):
    if name == "ubuntu-shell":
        return ["/bin/sh", "-c", WORKLOAD], MARKER
    if name == "torch-import":
        # Explicit environment and interpreter: do not depend on image ENV support.
        return [
            "/usr/bin/env",
            "OMP_NUM_THREADS=1",
            "MKL_NUM_THREADS=1",
            "OPENBLAS_NUM_THREADS=1",
            "PYTHONHASHSEED=0",
            "/opt/conda/bin/python",
            "-B",
            "-u",
            "-c",
            TORCH_WORKLOAD,
        ], TORCH_MARKER
    if name == "numpy-script":
        return [
            "/usr/bin/env",
            "OMP_NUM_THREADS=1",
            "MKL_NUM_THREADS=1",
            "OPENBLAS_NUM_THREADS=1",
            "PYTHONHASHSEED=0",
            "/usr/local/bin/python",
            "-B",
            "-u",
            "-c",
            NUMPY_WORKLOAD,
        ], NUMPY_MARKER
    raise ValueError(f"unknown workload: {name}")


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def docker_bind_plan(base):
    plan = dict(binds=list(base["binds"]), symlinks=list(base["symlinks"]))
    for name in ("/run", "/var", "/sys"):
        path = Path(name)
        canonical = path.resolve(strict=True)
        if not canonical.is_dir() or canonical == Path("/") or canonical.is_relative_to("/tmp"):
            raise RuntimeError(f"unsafe Docker bind source: {path}")
        if not any(canonical.is_relative_to(Path(parent)) for parent in plan["binds"]):
            plan["binds"].append(str(canonical))
        if path.is_symlink():
            plan["symlinks"].append(dict(path=name, target=os.readlink(path)))
    return plan


def validate_prepared_record(prepared, digest):
    record = json.loads(prepared)
    if (
        record.get("status") != "prepared"
        or record.get("digest") != digest
        or record.get("architecture") != "amd64"
        or not record.get("image_handle")
    ):
        raise RuntimeError("supported prepare CLI returned a mismatching digest/platform/handle")
    return record


def digest_hex(digest):
    if (
        not isinstance(digest, str)
        or not digest.startswith("sha256:")
        or len(digest) != 71
        or any(c not in "0123456789abcdef" for c in digest[7:])
    ):
        raise ValueError(f"expected canonical SHA-256 digest: {digest!r}")
    return digest[7:]


def verify_oci_blob(path, digest, size):
    expected = digest_hex(digest)
    if type(size) is not int or size < 0:
        raise ValueError("invalid OCI descriptor size")
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(f"OCI blob must be a regular file: {path}")
    hasher = hashlib.sha256()
    length = 0
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            length += len(block)
            hasher.update(block)
    if length != size or hasher.hexdigest() != expected:
        raise ValueError(f"OCI blob length/hash mismatch: {path}")
