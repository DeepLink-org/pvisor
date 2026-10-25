#!/usr/bin/env python3
"""B-FS-TOOLS: optional AgentENV adapter for the unchanged V2 static payload.

Template restore and fixture upload precede Ready. Only the common payload is
timed; export and the existing independent host oracle follow execution.
No extra workload rounds, full-root inventory scans or interference gates.
"""

import argparse
import json
import os
import re
import shlex
import sys
import tarfile
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

from user_workloads_backends import save, sha


def configure(config):
    url = urlsplit(config["endpoint"])
    if (
        url.scheme not in ("http", "https")
        or not url.hostname
        or url.username
        or url.password
        or url.query
        or url.fragment
        or (url.scheme == "http" and url.hostname not in ("localhost", "127.0.0.1", "::1"))
    ):
        raise ValueError("invalid AgentENV endpoint")
    os.environ.update(
        E2B_API_URL=config["endpoint"],
        E2B_SANDBOX_URL=config["endpoint"],
        E2B_API_KEY=Path(config["api_key_file"]).read_text().strip(),
    )
    from importlib.metadata import version

    if version("e2b") != "2.50.0":
        raise ValueError("AgentENV adapter requires e2b==2.50.0")
    from e2b import Sandbox

    return Sandbox


def prepare_assets(batch, setup_path):
    config = json.loads(Path(setup_path).read_text())
    if config["guest_cpus"] != 2 or config["guest_memory_mib"] != 512:
        raise ValueError("AgentENV requires a separately prepared 2-vCPU/512-MiB template")
    configure(config)
    destination = Path(batch) / "agentenv-assets"
    destination.mkdir()
    bundle = destination / "payload.tar"
    toolroot = Path(batch) / "toolroot"
    manifest = {}
    with tarfile.open(bundle, "w") as archive:
        for relative in ("bench-artifacts/payload", "bench-artifacts/workload", "fs-v2-fixture"):
            path = toolroot / relative
            archive.add(path, arcname=relative)
            for member in [path] if path.is_file() else path.rglob("*"):
                if member.is_file():
                    manifest[str(member.relative_to(toolroot))] = sha(member)
    save(destination / "manifest.json", manifest)
    config.update(
        bundle=str(bundle),
        bundle_sha256=sha(bundle),
        manifest=str(destination / "manifest.json"),
        manifest_sha256=sha(destination / "manifest.json"),
        setup_sha256=sha(setup_path),
        ready="command dispatch to streamed common payload READY; restore/upload excluded",
        output="guest writable filesystem; streamed export and common host oracle after timing",
    )
    return config


def service_state(config):
    root = Path(config["service_cgroup"])
    state = {
        key: (root / key).read_text().strip()
        for key in ("cpu.max", "memory.max", "memory.swap.max", "cpu.stat", "memory.events")
    }
    if any(state[key] != value for key, value in config["service_limits"].items()):
        raise ValueError("AgentENV service limits changed")
    if (root / "cpuset.cpus.effective").read_text().strip() != "0-1":
        raise ValueError("AgentENV service CPU placement differs")
    return state


def checked(sandbox, command, **kwargs):
    result = sandbox.commands.run(command, user="root", **kwargs)
    if result.exit_code != 0:
        raise ValueError("AgentENV preparation/export command failed")
    return result


def connect(folder):
    launch = json.loads((Path(folder) / "agentenv-launch.json").read_text())
    record = json.loads((Path(folder) / "agentenv.json").read_text())
    Sandbox = configure(launch["configuration"])
    return Sandbox.connect(record["sandbox_id"], request_timeout=30), launch, record


def build_command(batch, protocol, folder, case):
    config = protocol["reference_configuration"]["agentenv"]
    folder = Path(folder)
    before = service_state(config)
    record = dict(token=uuid.uuid4().hex, service_before=before, cleanup=False)
    launch = dict(configuration=config, case=case, timeout=protocol["trial_timeout_seconds"])
    save(folder / "agentenv-launch.json", launch)
    save(folder / "agentenv.json", record)
    Sandbox = configure(config)
    start = time.monotonic_ns()
    # Persist identity before any upload: caller's finally block owns teardown.
    sandbox = Sandbox.create(
        template=config["template"],
        timeout=900,
        metadata={"pvisor_benchmark": record["token"]},
        request_timeout=30,
    )
    record.update(sandbox_id=sandbox.sandbox_id, create_ns=time.monotonic_ns() - start)
    save(folder / "agentenv.json", record)
    if (
        sha(config["bundle"]) != config["bundle_sha256"]
        or sha(config["manifest"]) != config["manifest_sha256"]
    ):
        raise ValueError("frozen AgentENV payload changed")
    upload_started = time.monotonic_ns()
    with Path(config["bundle"]).open("rb") as stream:
        sandbox.files.write("/tmp/fs-user-payload.tar", stream, user="root")
    sandbox.files.write(
        "/tmp/fs-user-manifest.json", Path(config["manifest"]).read_bytes(), user="root"
    )
    record["upload_ns"] = time.monotonic_ns() - upload_started
    save(folder / "agentenv.json", record)
    # Only this disposable sandbox is changed. Read-only input matches native VM.
    script = (
        "import hashlib,json,os,subprocess,tarfile\n"
        "from pathlib import Path\n"
        "with tarfile.open('/tmp/fs-user-payload.tar') as f:f.extractall('/',filter='data')\n"
        "for name,digest in json.loads(Path('/tmp/fs-user-manifest.json').read_text()).items():\n"
        " assert hashlib.sha256(Path('/'+name).read_bytes()).hexdigest()==digest,name\n"
        f"for name,digest in {protocol['prepared_tools']!r}.items():\n"
        " assert hashlib.sha256(Path('/'+name).read_bytes()).hexdigest()==digest,name\n"
        "Path('/bench-trial/work/input').mkdir(parents=True)\n"
        "Path('/bench-trial/work/output').mkdir()\n"
        "subprocess.run(['mount','--bind','/fs-v2-fixture','/bench-trial/work/input'],check=True)\n"
        "subprocess.run(['mount','-o','remount,bind,ro','/bench-trial/work/input'],check=True)\n"
        "memory=int(next(s.split()[1] for s in Path('/proc/meminfo').read_text().splitlines() if s.startswith('MemTotal:')))\n"
        "assert os.cpu_count()==2 and 450000<memory<540000,(os.cpu_count(),memory)\n"
        "print(json.dumps(dict(cpus=os.cpu_count(),memory_kib=memory,mounts=Path('/proc/self/mountinfo').read_text())))\n"
    )
    setup_started = time.monotonic_ns()
    result = checked(sandbox, "/usr/bin/python3 -c " + shlex.quote(script), timeout=120)
    record["guest_setup_and_validation_ns"] = time.monotonic_ns() - setup_started
    save(folder / "agentenv-guest.json", json.loads(result.stdout))
    record["preparation_ns"] = time.monotonic_ns() - start
    save(folder / "agentenv.json", record)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PVISOR_", "E2B_"))}
    return [sys.executable, str(Path(__file__).resolve()), "--run", str(folder)], env


def run(folder):
    sandbox, launch, _ = connect(folder)
    case = launch["case"]
    if not re.fullmatch(r"[MRS][0-9]{2}", case):
        raise ValueError("invalid case")
    print("USER_WORKLOADS_LAUNCH " + json.dumps(dict(monotonic_ns=time.monotonic_ns())), flush=True)
    result = sandbox.commands.run(
        "/bench-artifacts/payload " + case + " output input 3 512",
        cwd="/bench-trial/work",
        user="root",
        envs={
            "PATH": "/usr/bin:/bin",
            "HOME": "/tmp",
            "LC_ALL": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONPYCACHEPREFIX": "/__pvisor_reference_no_pyc__",
        },
        timeout=launch["timeout"],
        on_stdout=lambda text: print(text, end="", flush=True),
        on_stderr=lambda text: print(text, end="", file=sys.stderr, flush=True),
    )
    if result.exit_code:
        raise ValueError("AgentENV payload failed")


def extract_output(archive, destination):
    """Reject links/devices/traversal before writing exported sandbox files."""
    with tarfile.open(archive) as stream:
        members = stream.getmembers()
        for member in members:
            path = Path(member.name)
            if path.is_absolute() or ".." in path.parts or not (member.isfile() or member.isdir()):
                raise ValueError("unsafe AgentENV output archive member")
        stream.extractall(destination, members=members, filter="data")


def finalize_trial(batch, protocol, folder, case, backend="agentenv"):
    sandbox, _, record = connect(folder)
    script = (
        "import tarfile\n"
        "with tarfile.open('/tmp/fs-user-output.tar','w') as out:\n"
        " out.add('/bench-trial/work/output',arcname='.')\n"
    )
    checked(sandbox, "/usr/bin/python3 -c " + shlex.quote(script), timeout=180)
    archive = Path(folder) / "agentenv-output.tar"
    with (
        sandbox.files.read(
            "/tmp/fs-user-output.tar", format="stream", user="root", request_timeout=180
        ) as stream,
        archive.open("wb") as out,
    ):
        for chunk in stream:
            out.write(chunk)
    destination = Path(folder) / "agentenv-output"
    destination.mkdir()
    extract_output(archive, destination)
    record["output_archive_sha256"] = sha(archive)
    save(Path(folder) / "agentenv.json", record)
    return destination


def cleanup_trial(batch, protocol, folder, case=None, backend="agentenv"):
    path = Path(folder) / "agentenv.json"
    if not path.exists():
        return
    record = json.loads(path.read_text())
    if "sandbox_id" not in record:
        raise ValueError("sandbox creation outcome unknown; reconcile retained metadata token")
    if record.get("cleanup"):
        return
    sandbox, launch, record = connect(folder)
    if sandbox.kill(request_timeout=30) is False:
        raise ValueError("sandbox deletion not acknowledged")
    record["cleanup"] = True
    record["service_after"] = service_state(launch["configuration"])
    save(path, record)
    a, b = [
        dict(line.split() for line in record[key]["memory.events"].splitlines())
        for key in ("service_before", "service_after")
    ]
    if any(a[k] != b[k] for k in ("oom", "oom_kill")):
        raise ValueError("AgentENV service OOM during trial")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=Path, required=True)
    run(parser.parse_args().run)
