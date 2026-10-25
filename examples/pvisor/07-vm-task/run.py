"""Host caller: submit one VM task, collect its result, then release its stage."""

import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

EXAMPLE = Path(__file__).resolve().parent
IMAGE = "docker.io/library/python@sha256:e8a38829209aea82774882e5157e1f7c3e8c8393ff53a697b3580896e57f345c"


def main():
    pvisor = shutil.which(os.environ.get("PVISOR_BIN", "pvisor"))
    if not pvisor:
        raise SystemExit("pvisor is not installed or is not on PATH; see the installation guide")
    work = Path(os.environ.get("WORK_ROOT", EXAMPLE / ".work")).resolve()
    work.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="task-", dir=work))
    workspace, stage = root / "workspace", root / "stage"
    workspace.mkdir()
    for name in ("input.json", "task.py"):
        shutil.copy2(EXAMPLE / name, workspace / name)
    print(f"Records: {root}", flush=True)

    def cli(*args):
        return subprocess.check_output([pvisor, *args], cwd=workspace, text=True)

    # ROOTFS is an optional offline override; normally pVisor prepares the OCI image.
    command = [
        pvisor,
        "spawn",
        "--executor",
        "vm",
        "--rootfs",
        os.environ.get("ROOTFS", f"image={IMAGE}"),
        "--overlaynet",
        "off",
        "--cpu",
        "1",
        "--memory",
        "256MiB",
        "--timeout",
        "60s",
        "--stdio",
        "capture",
        "--stage",
        str(stage),
        "--mount",
        f"{workspace}:stage",
    ]
    if os.environ.get("IMAGE_STORE"):
        command += ["--image-store", str(Path(os.environ["IMAGE_STORE"]).resolve())]
    execution = subprocess.run(
        [*command, "--", "/usr/local/bin/python3", "task.py"], cwd=workspace, check=False
    )
    # Retain the record even when the guest exits unsuccessfully.
    try:
        bundle = json.loads(cli("status", "--review", "--json", str(stage)))
    except subprocess.CalledProcessError as error:
        raise SystemExit(
            f"VM setup failed; see CLI output above. Files retained at {root}"
        ) from error
    (root / "bundle.json").write_text(json.dumps(bundle, indent=2))
    if (
        execution.returncode != 0
        or bundle["run"]["state"] != "completed"
        or bundle["run"]["exit_code"] != 0
    ):
        raise RuntimeError(f"Task failed; records and stage retained at {root}")
    result = json.loads(cli("inspect", str(stage), "--", "cat", "result.json"))
    sample = {
        "task_id": root.name,
        "job_id": bundle["run"]["run_id"],
        "attempt_id": bundle["run"]["attempt_id"],
        "result": result,
    }
    (root / "sample.json").write_text(json.dumps(sample, indent=2))
    cli("drop", str(stage))
    print(json.dumps(sample, indent=2))


if __name__ == "__main__":
    main()
