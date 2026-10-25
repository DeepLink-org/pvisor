"""Guest-side preparation and execution of two recorded Agent tool calls."""

import hashlib
import json
import os
import subprocess
from pathlib import Path


def main():
    workspace = Path.cwd()
    task = json.loads((workspace / "task.json").read_text())
    patch = workspace / "candidate.patch"
    if hashlib.sha256(patch.read_bytes()).hexdigest() != task["candidate_patch_sha256"]:
        raise RuntimeError("Candidate patch digest mismatch")
    git = ["git", "-c", f"safe.directory={task['workdir']}", "-C", task["workdir"]]
    revision = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    if revision != task["base_commit"]:
        subprocess.run([*git, "checkout", "--detach", task["base_commit"]], check=True)
    subprocess.run([*git, "apply", "--check", str(patch)], check=True)
    subprocess.run([*git, "apply", str(patch)], check=True)
    # OCI runtimes normally inject this file; an extracted rootfs may omit it.
    hosts = Path("/etc/hosts")
    content = hosts.read_text() if hosts.exists() else ""
    if not any("localhost" in line.split("#", 1)[0].split()[1:] for line in content.splitlines()):
        hosts.write_text(content.rstrip() + "\n127.0.0.1 localhost\n")
    steps = []
    for step in task["steps"]:
        try:
            result = subprocess.run(
                step["command"],
                check=False,
                env={**os.environ, **task.get("environment", {})},
                shell=True,
                executable="/bin/bash",
                cwd=task["workdir"],
                capture_output=True,
                text=True,
                timeout=180,
            )
            feedback = dict(
                exit_code=result.returncode,
                stdout=result.stdout,
                stderr=result.stderr,
                timed_out=False,
            )
        except subprocess.TimeoutExpired as error:

            def decode(value):
                return value.decode(errors="replace") if isinstance(value, bytes) else value or ""

            feedback = dict(
                exit_code=None,
                stdout=decode(error.stdout),
                stderr=decode(error.stderr),
                timed_out=True,
            )
        steps.append({**step, **feedback})
        (workspace / "result.json").write_text(json.dumps({"steps": steps}, indent=2))
    # Tool failures are observations for the caller, not missing execution results.


if __name__ == "__main__":
    main()
