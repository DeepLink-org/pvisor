#!/usr/bin/env python3
"""B-FS-TOOLS: private rootless Docker overlay2 reference adapter.

Image import and container creation are preparation. Ready starts immediately
before exec of ``docker start --attach`` and ends at the common static payload
Ready marker. Output is written to the container's overlay2 layer, never a host
bind or tmpfs, and copied out after the payload completes for the independent
host oracle. Only containers labelled with this exact trial ownership token can
be removed. The caller owns the whole-parent budget and process timeout.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tarfile
import time
import uuid
from pathlib import Path

LABEL = "org.pvisor.benchmark.user-filesystem-trial"
RUNTIME_MARKER = "USER_WORKLOADS_DOCKER_RUNTIME "


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _docker(configuration, *arguments):
    return [
        configuration.get("docker_binary", "/usr/bin/docker"),
        "--host",
        configuration["docker_host"],
        *map(str, arguments),
    ]


def _checked(configuration, *arguments, timeout=120):
    result = subprocess.run(
        _docker(configuration, *arguments), text=True, capture_output=True, timeout=timeout
    )
    if result.returncode:
        raise RuntimeError("Docker adapter failed: " + result.stderr[-4000:])
    return result.stdout


def _configuration(protocol):
    return protocol["reference_configuration"]["docker"]


def prepare_assets(batch, setup_path):
    """Import the frozen toolroot into an existing *new owned* private daemon.

    The setup receipt must come from ``prepare_reference_budget.py`` and prove
    the requested two-core/2 GiB/zero-swap whole-parent budget. This function
    never creates or reconfigures daemons. Returns a JSON-safe configuration to
    place at ``protocol['reference_configuration']['docker']``. The caller must
    freeze this module and its launcher before invoking it.
    """
    batch, setup_path = Path(batch).resolve(), Path(setup_path).resolve(strict=True)
    setup = json.loads(setup_path.read_text())
    if (
        setup.get("state") != "ready-for-image-import-and-independent-runtime-capability-probes"
        or setup.get("memory_bytes") != 2 << 30
        or setup.get("swap_bytes") != 0
        or setup.get("cpu_ids") != [0, 1]
        or setup.get("cpu_quota") != 2
    ):
        raise ValueError("private Docker setup does not match the registered whole-parent budget")
    info = setup.get("docker_info", {})
    if info.get("Driver") != "overlay2" or info.get("CgroupDriver") != "systemd":
        raise ValueError("private Docker must use overlay2 and systemd cgroups")
    configuration = dict(
        docker_binary=setup["tools"]["docker"],
        docker_host=setup["docker_host"],
        budget_slice_unit=setup["slice_unit"],
        budget_cgroup=setup["cgroup"],
        setup_receipt=str(setup_path),
        setup_sha256=sha(setup_path),
        storage_driver="overlay2",
        cpu_placement=setup["cpu_placement"],
        output="container writable overlay2 layer; copied out after timing",
        input="same frozen fixture, read-only bind at workspace/input",
        ready="host pre-exec marker before docker start --attach; create/import excluded",
        limitation="payload and daemon affinity 0,1; no delegated cpuset, so early runc CPU placement is not proven",
    )
    budget = Path(configuration["budget_cgroup"])
    actual_budget = {
        name: (budget / name).read_text().strip()
        for name in ("cpu.max", "memory.max", "memory.swap.max")
    }
    if actual_budget != {
        "cpu.max": "200000 100000",
        "memory.max": str(2 << 30),
        "memory.swap.max": "0",
    }:
        raise ValueError("actual private parent cgroup resource limits changed")
    configuration["actual_parent_limits"] = actual_budget
    configuration["runtime_cgroup_observation"] = (
        "host cgroup namespace; payload reports actual parent path once before exec"
    )
    destination = batch / "docker-assets"
    destination.mkdir(exist_ok=False)
    source = Path(__file__).with_name("user_workloads_docker_launch.rs")
    frozen_source = batch / "harness" / source.name
    if frozen_source.resolve() != source.resolve():
        frozen_source.write_bytes(source.read_bytes())
    runtime = destination / "docker-runtime"
    compile_command = [
        "rustc",
        "--edition=2024",
        "-O",
        "--target",
        "x86_64-unknown-linux-musl",
        str(frozen_source),
        "-o",
        str(runtime),
    ]
    compiled = subprocess.run(compile_command, capture_output=True, text=True)
    (destination / "compile.stdout").write_text(compiled.stdout)
    (destination / "compile.stderr").write_text(compiled.stderr)
    if compiled.returncode:
        raise RuntimeError("Docker runtime launcher compilation failed")
    toolroot = batch / "toolroot"
    command = _docker(
        configuration,
        "import",
        "--change",
        "WORKDIR /bench-trial/work",
        "-",
        "pvisor-fs-user:" + uuid.uuid4().hex,
    )
    with (
        (destination / "import.stdout").open("w") as out,
        (destination / "import.stderr").open("w") as err,
    ):
        imported = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=out, stderr=err)
        try:
            with tarfile.open(fileobj=imported.stdin, mode="w|") as archive:
                archive.add(toolroot, arcname=".", recursive=True)
                archive.add(runtime, arcname="bench-artifacts/docker-runtime")
                for name in (
                    "bench-trial",
                    "bench-trial/work",
                    "bench-trial/work/input",
                    "bench-trial/work/output",
                ):
                    member = tarfile.TarInfo(name)
                    member.type, member.mode = tarfile.DIRTYPE, 0o755
                    archive.addfile(member)
        finally:
            imported.stdin.close()
        if imported.wait(timeout=300):
            raise RuntimeError("Docker image import failed; retained docker-assets/import.stderr")
    image_id = (destination / "import.stdout").read_text().strip()
    if not image_id.startswith("sha256:") or len(image_id) != 71:
        raise ValueError("Docker image import did not return an immutable image ID")
    inspection = json.loads(_checked(configuration, "image", "inspect", image_id))
    save(destination / "image-inspect.json", inspection)
    configuration.update(
        image_id=image_id,
        runtime_sha256=sha(runtime),
        runtime_source_sha256=sha(frozen_source),
        image_inspection_sha256=sha(destination / "image-inspect.json"),
    )
    save(
        destination / "receipt.json",
        dict(
            configuration=configuration,
            compile_command=compile_command,
            import_command=command,
            toolroot=str(toolroot),
        ),
    )
    return configuration


def build_command(batch, protocol, folder, case):
    """Create a fresh overlay2 container and return its observed launch argv/env."""
    batch, folder = Path(batch).resolve(), Path(folder).resolve(strict=True)
    configuration = _configuration(protocol)
    config_path = folder / "docker-launch.json"
    if config_path.exists():
        raise FileExistsError("refuse to reuse Docker trial")
    token = uuid.uuid4().hex
    name = "pvisor-fs-user-" + token
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("PVISOR_", "WORKLOADS_V2_", "DOCKER_"))
        and key.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")
    }
    environment.update(PATH="/usr/bin:/bin", LC_ALL="C")
    payload_env = dict(
        PATH="/usr/bin:/bin",
        HOME="/tmp",
        LC_ALL="C",
        GIT_CONFIG_NOSYSTEM="1",
        GIT_CONFIG_GLOBAL="/dev/null",
        PYTHONDONTWRITEBYTECODE="1",
        PYTHONPYCACHEPREFIX="/__pvisor_reference_no_pyc__",
    )
    argv = [
        "create",
        "--pull",
        "never",
        "--name",
        name,
        "--label",
        LABEL + "=" + token,
        "--network",
        "none",
        "--cgroupns",
        "host",
        "--cgroup-parent",
        configuration["budget_slice_unit"],
        "--workdir",
        "/bench-trial/work",
        "--mount",
        "type=bind,src="
        + str(batch / "toolroot/fs-v2-fixture")
        + ",dst=/bench-trial/work/input,readonly",
        "--entrypoint",
        "/bench-artifacts/docker-runtime",
    ]
    for key, value in payload_env.items():
        argv += ["--env", key + "=" + value]
    argv += [
        configuration["image_id"],
        "/bench-artifacts/payload",
        case,
        "output",
        "input",
        str(protocol.get("rounds", 3)),
        str(protocol.get("ram_mib", 512)),
    ]
    config = dict(
        configuration=configuration,
        name=name,
        token=token,
        create_command=_docker(configuration, *argv),
        backend="docker",
        case_id=case,
        folder=str(folder),
        payload_environment=payload_env,
    )
    save(config_path, config)  # Cleanup owns this unique name even if create errors.
    created = subprocess.run(
        config["create_command"], capture_output=True, text=True, timeout=120, env=environment
    )
    (folder / "docker-create.stdout").write_text(created.stdout)
    (folder / "docker-create.stderr").write_text(created.stderr)
    if created.returncode:
        raise RuntimeError("Docker create failed; retained docker-create.stderr")
    config["container_id"] = created.stdout.strip()
    save(config_path, config)
    return [sys.executable, str(Path(__file__).resolve()), "--start", str(config_path)], environment


def _inspect_owned(configuration, config):
    inspection = json.loads(_checked(configuration, "inspect", config["name"]))
    if (
        len(inspection) != 1
        or inspection[0].get("Config", {}).get("Labels", {}).get(LABEL) != config["token"]
    ):
        raise ValueError("Docker ownership label mismatch")
    return inspection[0]


def finalize_trial(batch, protocol, folder, case=None, backend="docker"):
    """Retain exit/storage/runtime evidence, copy completed output, return its path.

    This runs after timing. The caller subsequently executes the shared static
    host oracle and invokes ``cleanup_trial`` from its own finally block.
    """
    if backend != "docker":
        raise ValueError("Docker adapter received another backend")
    folder = Path(folder)
    configuration = _configuration(protocol)
    config = json.loads((folder / "docker-launch.json").read_text())
    inspection = _inspect_owned(configuration, config)
    save(folder / "docker-inspect.json", inspection)
    if (
        inspection["State"]["Running"]
        or inspection["State"]["ExitCode"] != 0
        or inspection["State"].get("OOMKilled")
    ):
        raise ValueError("Docker container did not complete successfully")
    if (
        inspection.get("GraphDriver", {}).get("Name") != "overlay2"
        or inspection.get("HostConfig", {}).get("CgroupParent")
        != configuration["budget_slice_unit"]
        or any(
            m["Destination"] in ("/bench-trial/work", "/bench-trial/work/output")
            for m in inspection.get("Mounts", [])
        )
    ):
        raise ValueError("Docker output storage or parent budget changed")
    lines = (folder / "stdout.log").read_text().splitlines()
    markers = [
        json.loads(line[len(RUNTIME_MARKER) :]) for line in lines if line.startswith(RUNTIME_MARKER)
    ]
    if (
        len(markers) != 1
        or markers[0].get("affinity") != [0, 1]
        or markers[0].get("root_filesystem") != "overlay"
    ):
        raise ValueError("Docker payload affinity/storage receipt missing")
    relative_budget = str(Path(configuration["budget_cgroup"]).relative_to("/sys/fs/cgroup"))
    actual_cgroup = markers[0].get("cgroup_namespace_path", "")
    if not actual_cgroup.startswith("0::/" + relative_budget + "/"):
        raise ValueError("Docker payload is outside the registered whole-parent budget")
    output = folder / "docker-output"
    output.mkdir(exist_ok=False)
    _checked(
        configuration,
        "cp",
        config["name"] + ":/bench-trial/work/output/.",
        str(output),
        timeout=300,
    )
    save(folder / "docker-runtime.json", markers[0])
    return output


def cleanup_trial(batch, protocol, folder, case=None, backend="docker"):
    """Remove only this adapter's uniquely labelled container; preserve evidence."""
    config_path = Path(folder) / "docker-launch.json"
    if backend != "docker" or not config_path.exists():
        return
    configuration = _configuration(protocol)
    config = json.loads(config_path.read_text())
    existing = subprocess.run(
        _docker(configuration, "inspect", config["name"]), capture_output=True, text=True
    )
    if existing.returncode:
        save(
            Path(folder) / "docker-cleanup.json", dict(already_absent=True, stderr=existing.stderr)
        )
        return
    inspection = _inspect_owned(configuration, config)
    save(Path(folder) / "docker-final-inspect.json", inspection)
    snapshot = dict(
        scope="failed or unverified trial only; partial output is not a successful sample"
    )
    try:
        if not (Path(folder) / "docker-output").exists():
            # The observation timer kills its client process group. A detached
            # Docker task still belongs to our uniquely labelled container;
            # stop it before retaining a partial failure snapshot.
            if inspection["State"].get("Running"):
                stopped = subprocess.run(
                    _docker(configuration, "kill", config["name"]),
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                snapshot["kill"] = dict(
                    returncode=stopped.returncode, stdout=stopped.stdout, stderr=stopped.stderr
                )
            output = Path(folder) / "docker-failed-output"
            output.mkdir(exist_ok=False)
            copied = subprocess.run(
                _docker(
                    configuration, "cp", config["name"] + ":/bench-trial/work/output/.", str(output)
                ),
                capture_output=True,
                text=True,
                timeout=30,
            )
            snapshot.update(
                returncode=copied.returncode, stdout=copied.stdout, stderr=copied.stderr
            )
    except Exception as error:
        snapshot["error"] = repr(error)
        if isinstance(error, subprocess.TimeoutExpired):
            snapshot.update(stdout=str(error.stdout or ""), stderr=str(error.stderr or ""))
    finally:
        save(Path(folder) / "docker-failure-snapshot.json", snapshot)
        result = subprocess.run(
            _docker(configuration, "rm", "--force", config["name"]),
            capture_output=True,
            text=True,
            timeout=60,
        )
        save(
            Path(folder) / "docker-cleanup.json",
            dict(returncode=result.returncode, stdout=result.stdout, stderr=result.stderr),
        )
    if result.returncode:
        raise RuntimeError("owned Docker container cleanup failed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--start", type=Path, required=True)
    args = parser.parse_args()
    config = json.loads(args.start.read_text())
    print(
        "USER_WORKLOADS_LAUNCH "
        + json.dumps(dict(monotonic_ns=time.monotonic_ns(), backend="docker")),
        flush=True,
    )
    command = _docker(config["configuration"], "start", "--attach", config["name"])
    os.execvpe(command[0], command, os.environ.copy())


if __name__ == "__main__":
    main()
