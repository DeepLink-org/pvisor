"""Opt-in E2B SDK check that API restart retains a native VM and stdin job.

Requires a native --e2b template, KVM and an empty delegated cgroup. The caller
supplies an unused state directory and OPEN_SANDBOX_API_KEY/E2B_API_KEY. Only this
script's sandbox is removed; the API process is always stopped on completion.
"""

import argparse
import json
import os
import re
import subprocess
import time
from pathlib import Path

import httpx
from e2b import Sandbox


def run(args):
    state = Path(args.state)
    state.mkdir(mode=0o700, parents=True, exist_ok=False)
    template = json.loads((Path(args.images_dir) / (args.template + ".json")).read_text())
    assert template["native_guest"] and template["e2b"]
    key = os.environ.get("OPEN_SANDBOX_API_KEY") or os.environ["E2B_API_KEY"]
    command = [
        args.daemon,
        "serve",
        "--sandbox",
        "--images-dir",
        args.images_dir,
        "--cgroup-root",
        args.cgroup_root,
        "--state",
        args.state,
        "--listen",
        "127.0.0.1:0",
    ]
    server = None
    sandbox = None
    domain = None
    log_path = state / "api-test.log"

    def start():
        nonlocal server, domain
        with log_path.open("w") as log:
            server = subprocess.Popen(command, stdout=log, stderr=log)
        for _ in range(500):
            if server.poll() is not None:
                raise RuntimeError(log_path.read_text())
            match = re.search(r"listening on (127\.0\.0\.1:\d+)", log_path.read_text())
            if match:
                domain = match.group(1)
                command[-1] = domain  # Rebind the same authority after restart.
                return
            time.sleep(0.01)
        raise TimeoutError("API listener did not start")

    try:
        start()
        options = {
            "api_url": "http://" + domain,
            "sandbox_url": "http://" + domain + "/e2b",
            "api_key": key,
        }
        sandbox = Sandbox.create(args.template, **options)
        assert (
            sandbox.commands.run(
                "test ! -e /opt/pvisor/execd && test ! -e /opt/pvisor/envd"
            ).exit_code
            == 0
        )
        payload = bytes(range(256)) * 1025
        sandbox.files.write("/tmp/restart-data", payload)
        pending = sandbox.commands.run(
            'read value; printf "retained:%s" "$value"', background=True, stdin=True
        )
        pid = pending.pid
        assert any(process.pid == pid for process in sandbox.commands.list())
        pending.disconnect()
        record_path = state / sandbox.sandbox_id / "run.json"
        identity = json.loads(record_path.read_text())
        server.terminate()
        server.wait(timeout=20)
        server = None
        start()
        connected = Sandbox.connect(sandbox.sandbox_id, **options)
        assert json.loads(record_path.read_text()) == identity, (
            "API restart replaced the VM Attempt"
        )
        assert connected.files.read("/tmp/restart-data", format="bytes") == payload
        assert any(process.pid == pid for process in connected.commands.list())
        handle = connected.commands.connect(pid)
        connected.commands.send_stdin(pid, "restart ✓\n")
        assert handle.wait().stdout == "retained:restart ✓"
        print(
            "PASS: same native VM/Attempt, binary file and live stdin process retained across API restart",
            flush=True,
        )
    finally:
        try:
            if sandbox is not None:
                sandbox.kill()
            if server is not None and domain:
                with httpx.Client(
                    base_url="http://" + domain,
                    headers={"OPEN-SANDBOX-API-KEY": key},
                    trust_env=False,
                ) as client:
                    assert client.get("/v1/sandboxes").json()["items"] == []
        finally:
            if server is not None:
                server.terminate()
                server.wait(timeout=20)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", required=True)
    parser.add_argument("--images-dir", required=True)
    parser.add_argument("--cgroup-root", required=True)
    parser.add_argument("--state", required=True)
    parser.add_argument("--template", default="native-e2b")
    run(parser.parse_args())
