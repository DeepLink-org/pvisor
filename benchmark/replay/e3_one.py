"""Run one frozen E3 trajectory against an OpenSandbox deployment."""

import argparse
import base64
import hashlib
import json
import shlex
import time
from pathlib import Path

from opensandbox_backend import OpenSandboxBackend

LOCALHOST_INIT = "set -eu; if [ ! -s /etc/hosts ]; then printf '127.0.0.1 localhost\\n::1 localhost ip6-localhost ip6-loopback\\n' > /etc/hosts; fi"
LOCALHOST_CHECK = "import socket; socket.getaddrinfo('localhost',0); s=socket.socket(); s.bind(('localhost',0)); s.close()"


def inputs(archive, catalog):
    manifest = json.loads((archive / "manifest.json").read_bytes())

    def verified(name):
        data = (archive / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != manifest[name]:
            raise ValueError(f"Archive checksum mismatch for {name}")
        return data

    wrapper = base64.b64encode(verified("replay_tool.py")).decode()
    plans = json.loads(verified("plans.json"))
    bindings = json.loads(verified("bindings.json"))
    images = {task: image for image in json.loads(catalog.read_bytes()) for task in image["tasks"]}
    for plan in plans:
        task = plan["task_id"]
        binding, image = bindings[task], images[task]
        if (
            plan["image_digest"] != binding["digest"]
            or binding["rootfs"] != image["source_rootfs"]
            or binding["base_commit"] != image["base_commit"]
        ):
            raise ValueError(f"Image binding mismatch for {task}")
    return plans, bindings, images, wrapper


def run(backend, plan, binding, wrapper, output):
    env = backend.config["env"]
    mini = "/root/.local/share/uv/tools/mini-swe-agent"
    inline = f'import base64;exec(compile(base64.b64decode({wrapper!r}),"<replay-tool>","exec"))'

    def execute(command, source=True):
        payload = base64.b64encode(json.dumps(dict(command=command, timeout=120)).encode()).decode()
        args = [binding["python"], "-I", "-S", "-c", inline, payload]
        if source:
            args = [
                "env",
                "PATH=" + mini + "/bin:" + env["PATH"],
                "PYTHONPATH=" + mini + "/lib/python3.12/site-packages",
                *args,
            ]
        transport = backend.exec(shlex.join(args))
        if transport["return_code"]:
            raise RuntimeError("Guest helper failed")
        row = json.loads(transport["stdout"])
        if row["timed_out"] and (not row["cleanup_verified"] or row["live_process_group_members"]):
            raise RuntimeError("Guest timeout cleanup failed")
        return row

    output.mkdir(parents=True, exist_ok=False)
    start = time.monotonic()
    result = dict(
        trajectory=plan["trajectory_id"], image=backend.config["image"], replay_completed=False
    )
    try:
        backend.start()
        result["pod"] = backend.pod
        checks = [
            LOCALHOST_INIT,
            shlex.join([binding["python"], "-c", LOCALHOST_CHECK]),
            'test "$(git rev-parse HEAD)" = ' + shlex.quote(binding["base_commit"]),
            shlex.join(
                [
                    mini + "/bin/python",
                    "-c",
                    "import sys,yaml,importlib.metadata as m; "
                    "assert sys.version_info[:3]==(3,12,11) and yaml.__version__=='6.0.3' "
                    "and m.version('mini-swe-agent')=='2.4.6'",
                ]
            ),
        ]
        for check in checks:
            if execute(check, False)["return_code"]:
                raise RuntimeError("Initial guest check failed: " + check)
        result["ready_s"] = time.monotonic() - start
        with (output / "steps.jsonl").open("x") as log:
            for i, step in enumerate(plan["steps"]):
                time.sleep(step["wait_before_s"])
                row = {
                    "step": i,
                    "skipped": step["skip"],
                    "source_returncode": step["source_returncode"],
                }
                if not step["skip"]:
                    row.update(execute(step["command"]))
                log.write(json.dumps(row) + "\n")
                log.flush()
        patch = execute("git diff --binary " + shlex.quote(binding["base_commit"]))
        if patch["return_code"]:
            raise RuntimeError("Could not collect worktree diff")
        (output / "agent.patch").write_text(patch["stdout"])
        result["replay_completed"] = True
        result["replay_finished_s"] = time.monotonic() - start
    except BaseException as error:
        result["failure"] = type(error).__name__
        raise
    finally:
        try:
            result["cleanup"] = backend.stop()
        except Exception as error:
            result["cleanup_failure"] = type(error).__name__
            raise
        finally:
            result["finished_s"] = time.monotonic() - start
            (output / "result.json").write_text(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--images", required=True, type=Path)
    parser.add_argument("--trajectory", required=True)
    parser.add_argument("--backend", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args()
    plans, bindings, images, wrapper = inputs(args.archive, args.images)
    matches = [plan for plan in plans if plan["trajectory_id"] == args.trajectory]
    if len(matches) != 1:
        parser.error("Trajectory must identify one archived plan")
    plan = matches[0]
    binding, image = bindings[plan["task_id"]], images[plan["task_id"]]
    reference = image["registry_name"] + "@" + image["descriptor"]["digest"]
    print(
        json.dumps(
            dict(
                plans=len(plans),
                tasks=len(bindings),
                trajectory=args.trajectory,
                steps=len(plan["steps"]),
                prepared_image=reference,
                recorded_wait_s=sum(s["wait_before_s"] for s in plan["steps"]),
            )
        ),
        flush=True,
    )
    if args.execute:
        if not args.backend or not args.output:
            parser.error("Execution requires --backend and --output")
        config = json.loads(args.backend.read_bytes())
        if config.get("kind") != "opensandbox" or config.get("mode", "kata-fc") != "kata-fc":
            parser.error("Expected an OpenSandbox Kata Firecracker deployment")
        config.update(
            image=config["registry"].rstrip("/") + "/" + reference,
            cwd="/app",
            vcpus=2,
            memory="8Gi",
            env={**binding["image_environment"], **plan["environment"]},
        )
        run(OpenSandboxBackend(config), plan, binding, wrapper, args.output)


if __name__ == "__main__":
    main()
