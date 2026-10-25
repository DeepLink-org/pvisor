"""Example worker adapter over the pVisor CLI; not a public pVisor SDK."""

import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

EXAMPLE = Path(__file__).resolve().parent


class ExecutionError(RuntimeError):
    """Sandbox setup, runner or feedback-collection failure; not a tool verdict."""


def execute(task, candidate_patch):
    """Execute a fixed command batch in one VM and return feedback to the framework.

    Nonzero tool exits and tool timeouts are returned as observations. ExecutionError
    reports infrastructure/preparation failures; existing artifacts stay on disk.
    Successful batches discard staged changes after persisting feedback. Failed
    batches retain their stage. Placement, scoring and retries belong to the caller.
    """
    pvisor = shutil.which(os.environ.get("PVISOR_BIN", "pvisor"))
    if not pvisor:
        raise ExecutionError(
            "pvisor is not installed or is not on PATH; see the installation guide"
        )
    work = Path(os.environ.get("WORK_ROOT", EXAMPLE / ".work")).resolve()
    work.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="task-", dir=work))
    workspace, stage = root / "workspace", root / "stage"
    workspace.mkdir()
    (workspace / "task.json").write_text(json.dumps(task, indent=2))
    shutil.copy2(candidate_patch, workspace / "candidate.patch")
    shutil.copy2(EXAMPLE / "task.py", workspace / "task.py")
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
        os.environ.get("ROOTFS", f"image={task['image']}"),
        "--overlaynet",
        "off",
        "--cpu",
        "1",
        "--memory",
        "1GiB",
        "--timeout",
        "300s",
        "--stdio",
        "capture",
        "--stage",
        str(stage),
        "--mount",
        f"{workspace}:stage",
    ]
    if os.environ.get("IMAGE_STORE"):
        command += ["--image-store", str(Path(os.environ["IMAGE_STORE"]).resolve())]
    try:
        execution = subprocess.run(
            [*command, "--", "/usr/local/bin/python3", "task.py"],
            cwd=workspace,
            check=False,
        )
    except OSError as error:
        raise ExecutionError(f"Cannot launch pVisor; files retained at {root}") from error
    # Retain the record even when the guest exits unsuccessfully.
    try:
        bundle = json.loads(cli("status", "--review", "--json", str(stage)))
    except (subprocess.CalledProcessError, json.JSONDecodeError) as error:
        raise ExecutionError(
            f"VM setup failed; see CLI output above. Files retained at {root}"
        ) from error
    (root / "bundle.json").write_text(json.dumps(bundle, indent=2))
    if (
        execution.returncode != 0
        or bundle["run"]["state"] != "completed"
        or bundle["run"]["exit_code"] != 0
    ):
        raise ExecutionError(f"VM or task preparation failed; records retained at {root}")
    try:
        result = json.loads(cli("inspect", str(stage), "--", "cat", "result.json"))
        if len(result["steps"]) != len(task["steps"]) or not result["steps"]:
            raise ValueError("Missing tool feedback")
    except (subprocess.CalledProcessError, ValueError, KeyError, TypeError) as error:
        raise ExecutionError(f"Cannot collect tool feedback; records retained at {root}") from error
    sample = {
        "task_id": task["task_id"],
        "sample_id": root.name,
        "records_dir": str(root),
        "job_id": bundle["run"]["run_id"],
        "attempt_id": bundle["run"]["attempt_id"],
        "steps": result["steps"],
    }
    (root / "sample.json").write_text(json.dumps(sample, indent=2))
    passed = all(s["exit_code"] == 0 and not s["timed_out"] for s in sample["steps"])
    if passed:
        try:
            cli("drop", str(stage))
        except subprocess.CalledProcessError as error:
            raise ExecutionError(f"Cleanup failed; feedback already saved at {root}") from error
    return sample
