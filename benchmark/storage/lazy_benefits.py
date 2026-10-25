#!/usr/bin/env python3
"""Benchmark: B-LAZY-STARTUP, user-facing (REGISTRY.md#b-lazy-startup).

Question: what do compressed local storage and remote on-demand images save,
and what do they cost versus native Docker for small and full-read workloads?
Design: independent storage/runtime/remote cohorts, verified OCI digests, fresh
private state, semspec clocks and correctness gates; no global cache changes.
Remote is loopback, not WAN; VM guest RAM differs from a container hard limit.
"""

import argparse
import datetime
import hashlib
import http.server
import json
import os
import random
import select
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tarfile
import threading
import time
from pathlib import Path

import native_docker as native

sys.path.insert(0, str(native.REPO / "benchmark/support"))
import lazy_image_v2 as wire  # noqa: E402

SAVE = native.RUNTIME.save
PYTHON_TREE = "/usr/local/lib/python3.12"
PYTHON_PROBE = """import sys,json,hashlib,sqlite3,ssl,zlib,pathlib,decimal
assert sys.version_info[:2] == (3,12)
assert hashlib.sha256(b'pvisor').hexdigest() == EXPECTED_HASH
assert json.loads(json.dumps(list(range(32)))) == list(range(32))
assert zlib.decompress(zlib.compress(b'image'*100)) == b'image'*100
db=sqlite3.connect(':memory:')
assert db.execute('select sum(value) from json_each(?)',('[1,2,3]',)).fetchone()[0] == 6
assert str(decimal.Decimal('0.1')+decimal.Decimal('0.2')) == '0.3'
print('REFERENCE_READY',flush=True)
""".replace("EXPECTED_HASH", repr(hashlib.sha256(b"pvisor").hexdigest()))
MMAP_PROBE = """import os,sys,stat,mmap,hashlib
assert sys.version_info[:2] == (3,12)
root='/usr/local/lib/python3.12'; files=[]; total=0; aggregate=hashlib.sha256()
for parent,dirs,names in os.walk(root,followlinks=False):
 for name in names:
  path=os.path.join(parent,name)
  if stat.S_ISREG(os.lstat(path).st_mode): files.append(path)
for path in sorted(files):
 size=os.stat(path).st_size; total+=size
 with open(path,'rb') as stream:
  if size:
   with mmap.mmap(stream.fileno(),0,access=mmap.ACCESS_READ) as data: digest=hashlib.sha256(data).hexdigest()
  else: digest=hashlib.sha256(b'').hexdigest()
 aggregate.update(path.encode()+b'\\0'+digest.encode()+b'\\n')
assert (len(files),total,aggregate.hexdigest()) == EXPECTED_ORACLE, (len(files),total,aggregate.hexdigest())
print('REFERENCE_READY',flush=True)
"""


def tree_oracle(image):
    """Derive the exact regular-file mmap oracle from verified OCI layer bytes."""
    files = {}
    prefix = PYTHON_TREE.lstrip("/") + "/"
    for layer in image["manifest"]["layers"]:
        with tarfile.open(Path(image["source"]) / layer["digest"][7:], "r:*") as archive:
            members = archive.getmembers()
            for member in members:
                path = member.name.removeprefix("./").lstrip("/")
                parent, _, name = path.rpartition("/")
                if name.startswith(".wh."):
                    victim = parent + "/" + name[4:]
                    if name == ".wh..wh..opq":
                        victim = parent
                    files = {
                        k: v
                        for k, v in files.items()
                        if k != victim and not k.startswith(victim + "/")
                    }
            for member in members:
                path = member.name.removeprefix("./").lstrip("/")
                if not path.startswith(prefix) or Path(path).name.startswith(".wh."):
                    continue
                if member.isfile():
                    with archive.extractfile(member) as stream:
                        digest = hashlib.file_digest(stream, "sha256").hexdigest()
                    files[path] = (member.size, digest)
                elif member.islnk():
                    target = member.linkname.removeprefix("./").lstrip("/")
                    files[path] = files[target]
                else:
                    files.pop(path, None)
    aggregate = hashlib.sha256()
    for path, (_, digest) in sorted(files.items()):
        aggregate.update(b"/" + path.encode() + b"\0" + digest.encode() + b"\n")
    assert files, "empty OCI file oracle"
    return len(files), sum(size for size, _ in files.values()), aggregate.hexdigest()


class Registry(http.server.BaseHTTPRequestHandler):
    """Read-only OCI endpoint, exact body accounting; never calls a user daemon."""

    def log_message(self, *_):
        pass

    def do_HEAD(self):
        self.reply(False)

    def do_GET(self):
        self.reply(True)

    def reply(self, body):
        status = 200
        digest = self.path.rsplit("/", 1)[-1]
        descriptor = self.server.objects.get(digest)
        if self.path in ("/v2", "/v2/"):
            payload, media = b"{}", "application/json"
        elif descriptor and ("/manifests/" in self.path or "/blobs/" in self.path):
            path, media = descriptor
            payload = path.read_bytes()
        else:
            status = 404
            payload = b'{"errors":[{"code":"BLOB_UNKNOWN"}]}'
            media = "application/json"
        self.send_response(status)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Content-Type", media)
        self.send_header("Docker-Distribution-Api-Version", "registry/2.0")
        if descriptor:
            self.send_header("Docker-Content-Digest", digest)
        self.end_headers()
        if body:
            self.wfile.write(payload)
        with self.server.guard:
            row = dict(
                path=self.path,
                method=self.command,
                status=status,
                response_bytes=len(payload) if body else 0,
                finished_ns=time.perf_counter_ns(),
                body_sha256=hashlib.sha256(payload).hexdigest() if body else None,
            )
            with self.server.rows_path.open("a") as stream:
                stream.write(json.dumps(row) + "\n")


def clean_env():
    return {k: v for k, v in os.environ.items() if not k.startswith("PVISOR_")}


def worker(batch):
    """Private user/mount/PID namespaces and pivot_root isolate Host authority."""
    subprocess.run(
        ["mount", "-t", "tmpfs", "-o", "mode=1777,nosuid,nodev", "tmpfs", "/tmp"], check=True
    )
    wire.enter_private_chroot(batch, batch)
    os.sched_setaffinity(0, {2, 3})
    config = json.loads((batch / "remote-config.json").read_text())
    token = config["token"]
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env = clean_env() | {"PVISOR_CACHE_TOKEN": token}
    log = (batch / "cache-service.log").open("wb")
    service = subprocess.Popen(
        [
            "/tmp/lazy-binaries/pvisor-daemon",
            "serve",
            "--cache",
            "--cache-listen",
            f"tcp://127.0.0.1:{port}",
            "--cache-image-store",
            str(batch / "service"),
        ],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    proxy = wire.TCPServer(("127.0.0.1", 0), wire.CacheProxy)
    proxy.upstream, proxy.counts = port, wire.Counts()
    thread = threading.Thread(target=proxy.serve_forever, daemon=True)
    thread.start()
    try:
        deadline = time.monotonic() + 20
        while True:
            if service.poll() is not None:
                raise RuntimeError("cache service exited")
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.1)
        print(
            json.dumps(
                {
                    "ready": True,
                    "namespaces": {
                        key: os.readlink("/proc/self/ns/" + key) for key in ["user", "mnt", "pid"]
                    },
                }
            ),
            flush=True,
        )
        for line in sys.stdin:
            request = json.loads(line)
            if request.get("stop"):
                return
            directory = Path(request["directory"])
            cache = Path(request["cache"])
            environment = env | {
                "PVISOR_CACHE_SERVER": f"tcp://127.0.0.1:{proxy.server_address[1]}",
                "PVISOR_RUN_HOME": str(directory / "runs"),
                "PVISOR_IMAGE_STORE": str(directory / "unused-local-store"),
                "XDG_CACHE_HOME": str(cache),
                "XDG_CONFIG_HOME": str(directory / "config"),
                "PVISOR_STARTUP_TIMING": "0",
            }
            argv = [
                "taskset",
                "-c",
                "0,1",
                "/tmp/lazy-binaries/pvisor",
                "spawn",
                "--vm",
                "--rootfs",
                "image=" + request["image"],
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--gateway-mode",
                "off",
                "--stdio",
                "inherit",
                "--stage",
                str(directory / "stage"),
                "--cpu",
                "2",
                "--memory",
                "512MiB",
                "--",
                *request["payload"],
            ]
            before = proxy.counts.snapshot()
            (directory / "workspace").mkdir()
            result = native.RUNTIME.capture(
                argv, directory / "workspace", environment, directory / "stdout", timeout=120
            )
            after = proxy.counts.snapshot()
            rows = after["rows"][len(before["rows"]) :]
            errors = after["errors"][len(before["errors"]) :]
            SAVE(directory / "requests.json", {"rows": rows, "errors": errors})
            assert not errors, errors
            assert (directory / "stdout").read_text().splitlines().count("REFERENCE_READY") == 1
            bundles = list((directory / "stage").rglob("run-bundle.json"))
            assert len(bundles) == 1, bundles
            bundle = json.loads(bundles[0].read_text())
            assert bundle["run"]["state"] == "completed" and bundle["run"]["exit_code"] == 0
            assert bundle["run"]["executor"]["isolation"] == "virtual_machine"
            prepared = [r["response"] for r in rows if r["op"] == "prepare"]
            assert prepared and all(r.get("digest") == request["digest"] for r in prepared), (
                prepared
            )
            metrics = dict(
                ready_ns=result["ready_duration_ns"],
                exit_ns=result["exit_duration_ns"],
                content_bytes=sum(r["content_bytes"] for r in rows),
                metadata_bytes=sum(r["metadata_bytes"] for r in rows),
                response_bytes=sum(r["response_bytes"] for r in rows),
                requests=len(rows),
            )
            assert (metrics["content_bytes"] > 0) == (request["state"] == "cold"), metrics
            SAVE(directory / "metrics.json", metrics)
            print(json.dumps(metrics), flush=True)
    finally:
        proxy.shutdown()
        proxy.server_close()
        service.terminate()
        try:
            service.wait(timeout=10)
        except subprocess.TimeoutExpired:
            service.kill()
            service.wait()
        log.close()


def exchange(process, request=None, timeout=150):
    if request is not None:
        process.stdin.write(json.dumps(request) + "\n")
        process.stdin.flush()
    readable, _, _ = select.select([process.stdout], [], [], timeout)
    if not readable:
        raise TimeoutError("private VM worker did not respond")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError("private worker exited; see worker.stderr")
    return json.loads(line)


def remote(e, args):
    images = {i["name"]: i for i in e.inputs}
    ubuntu, python = images["ubuntu-24.04"], images["python-3.12-slim-bookworm"]
    oracle = tree_oracle(python)
    workloads = {
        "shell": (
            ubuntu,
            [
                "/bin/sh",
                "-ec",
                '. /etc/os-release; test "$ID" = ubuntu; test "$VERSION_ID" = 24.04; printf "REFERENCE_READY\\n"',
            ],
        ),
        "python": (python, ["/usr/local/bin/python", "-B", "-u", "-c", PYTHON_PROBE]),
        "mmap-scan": (
            python,
            [
                "/usr/local/bin/python",
                "-B",
                "-u",
                "-c",
                MMAP_PROBE.replace("EXPECTED_ORACLE", repr(oracle)),
            ],
        ),
    }
    workloads = {
        name: (
            image,
            [
                "/bin/sh",
                "-ec",
                'test ! -e /tmp/pv-private; printf check >/tmp/pv-private; test "$(cat /tmp/pv-private)" = check; exec "$@"',
                "probe",
                *payload,
            ],
        )
        for name, (image, payload) in workloads.items()
    }
    SAVE(
        e.root / "workloads.json",
        {k: {"image": i["pinned"], "payload": p} for k, (i, p) in workloads.items()},
    )
    SAVE(e.root / "mmap-oci-oracle.json", oracle)
    (e.root / "frozen/bin").mkdir(parents=True)
    shutil.copy2(e.binary, e.root / "frozen/bin/pvisor")
    shutil.copy2(args.daemon, e.root / "frozen/bin/pvisor-daemon")
    e.command(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            e.root / "registry-key.pem",
            "-out",
            e.root / "registry-cert.pem",
            "-subj",
            "/CN=127.0.0.1",
            "-days",
            "1",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
        ]
    )
    source = e.root / "staging/blobs/sha256"
    source.mkdir(parents=True)
    for image in [ubuntu, python]:
        for blob in [image["manifest"]["config"], *image["manifest"]["layers"]]:
            path = Path(image["source"]) / blob["digest"][7:]
            if not (source / path.name).exists():
                shutil.copy2(path, source / path.name)
    preparation = []
    for image in [ubuntu, python]:
        start = time.perf_counter_ns()
        result = json.loads(
            e.command(
                [
                    e.binary,
                    "cache",
                    "publish",
                    "--backend",
                    "filesystem",
                    "--location",
                    e.root / "service/cache-v2",
                    "--image-store",
                    e.root / "staging",
                    image["pinned"],
                ]
            )
        )
        assert result["digest"] == image["digest"], result
        preparation.append(
            dict(image=image["name"], duration_ns=time.perf_counter_ns() - start, receipt=result)
        )
    SAVE(e.root / "preparation.json", preparation)
    e.inventory(e.root / "service", e.root / "service-inventory.json")
    e.inventory(e.root / "staging", e.root / "staging-inventory.json")
    SAVE(e.root / "remote-config.json", {"token": os.urandom(24).hex()})
    (e.root / "remote-config.json").chmod(0o600)
    os.sched_setaffinity(0, {2, 3})
    process = subprocess.Popen(
        [
            "unshare",
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child=KILL",
            "--mount-proc",
            "--propagation",
            "private",
            sys.executable,
            str(Path(__file__).resolve()),
            "--worker",
            str(e.root),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=(e.root / "worker.stderr").open("w"),
        text=True,
        start_new_session=True,
    )
    try:
        containment = exchange(process, timeout=30)
        assert containment["ready"]
        assert all(
            containment["namespaces"][k] != os.readlink("/proc/self/ns/" + k)
            for k in ["user", "mnt", "pid"]
        )
        SAVE(e.root / "containment.json", containment)
        e.save(
            daemon_sha256=native.sha(args.daemon),
            runs=args.runs,
            warmups=3,
            budget="2 CPU/512 MiB container vs 2 vCPU/512 MiB guest; clients 0,1, services 2,3; helpers not an equal full-product budget",
            cache="empty client then immediate warm client; host page caches retained; local loopback",
        )
        rng = random.Random(20261025)
        all_pairs = {}
        for workload, (image, payload) in workloads.items():

            def trial(phase, index):
                pair = {"phase": phase, "index": index}
                for backend in rng.sample(["docker", "pvisor"], 2):
                    directory = e.root / f"{workload}-{phase}-{index:03}-{backend}"
                    directory.mkdir()
                    with socket.socket() as reservation:
                        reservation.bind(("127.0.0.1", 0))
                        registry_port = reservation.getsockname()[1]
                    docker = (
                        native.Docker(
                            e, directory, insecure_registries=[f"127.0.0.1:{registry_port}"]
                        )
                        if backend == "docker"
                        else None
                    )
                    registry = None
                    try:
                        if docker:
                            registry_config = directory / "registry-config.json"
                            SAVE(
                                registry_config,
                                {
                                    "images": [image],
                                    "rows": str(directory / "registry.jsonl"),
                                    "port": registry_port,
                                    "cert": str(e.root / "registry-cert.pem"),
                                    "key": str(e.root / "registry-key.pem"),
                                },
                            )
                            registry = subprocess.Popen(
                                [
                                    "nsenter",
                                    "-t",
                                    str(docker.pid),
                                    "-U",
                                    "-n",
                                    "--",
                                    "taskset",
                                    "-c",
                                    "2,3",
                                    sys.executable,
                                    str(Path(__file__).resolve()),
                                    "--registry",
                                    str(registry_config),
                                ],
                                stdout=subprocess.PIPE,
                                stderr=(directory / "registry.stderr").open("w"),
                                text=True,
                            )
                            endpoint = exchange(registry, timeout=10)
                            assert endpoint["ready"]
                        for state in ["cold", "warm"]:
                            folder = directory / state
                            folder.mkdir()
                            if backend == "pvisor":
                                metrics = exchange(
                                    process,
                                    dict(
                                        directory=str(folder),
                                        cache=str(directory / "cache"),
                                        image=image["pinned"],
                                        digest=image["digest"],
                                        payload=payload,
                                        state=state,
                                    ),
                                )
                            else:
                                local_image = (
                                    f"127.0.0.1:{endpoint['port']}/bench/image@{image['digest']}"
                                )
                                rows_path = directory / "registry.jsonl"
                                before = (
                                    len(rows_path.read_text().splitlines())
                                    if rows_path.exists()
                                    else 0
                                )
                                command = [
                                    "taskset",
                                    "-c",
                                    "0,1",
                                    *docker.argv,
                                    "run",
                                    "--rm",
                                    "--network=none",
                                    "--cpus=2",
                                    "--memory=512m",
                                    local_image,
                                    "/usr/bin/taskset",
                                    "-c",
                                    "0,1",
                                    *payload,
                                ]
                                # docker run pulls a missing image and reuses a present one.
                                result = native.RUNTIME.capture(
                                    command,
                                    native.REPO,
                                    clean_env(),
                                    folder / "stdout",
                                    timeout=120,
                                )
                                rows = [
                                    json.loads(line) for line in rows_path.read_text().splitlines()
                                ][before:]
                                SAVE(folder / "requests.json", rows)
                                text = (folder / "stdout").read_text()
                                assert not (directory / "registry.stderr").read_text(), (
                                    "registry transfer error"
                                )
                                assert text.splitlines().count("REFERENCE_READY") == 1
                                assert not e.command([*docker.argv, "ps", "-aq"]).strip()
                                inspect = json.loads(
                                    e.command([*docker.argv, "image", "inspect", local_image])
                                )[0]
                                config = json.loads(
                                    (
                                        Path(image["source"])
                                        / image["manifest"]["config"]["digest"][7:]
                                    ).read_text()
                                )
                                assert inspect["Descriptor"]["digest"] == image["digest"]
                                assert inspect["RootFS"]["Layers"] == config["rootfs"]["diff_ids"]
                                metrics = dict(
                                    ready_ns=result["ready_duration_ns"],
                                    exit_ns=result["exit_duration_ns"],
                                    response_bytes=sum(r["response_bytes"] for r in rows),
                                    content_bytes=sum(
                                        r["response_bytes"] for r in rows if "/blobs/" in r["path"]
                                    ),
                                    metadata_bytes=sum(
                                        r["response_bytes"]
                                        for r in rows
                                        if "/manifests/" in r["path"]
                                    ),
                                    requests=len(rows),
                                )
                                assert (metrics["content_bytes"] > 0) == (state == "cold"), metrics
                                SAVE(folder / "metrics.json", metrics)
                            pair[backend + "-" + state] = metrics
                    finally:
                        if registry:
                            registry.terminate()
                            registry.wait(timeout=10)
                        if docker:
                            docker.close()
                SAVE(e.root / f"{workload}-{phase}-{index:03}.json", pair)
                print(
                    workload,
                    phase,
                    index,
                    {
                        k: round(v["ready_ns"] / 1e6, 2)
                        for k, v in pair.items()
                        if isinstance(v, dict)
                    },
                    flush=True,
                )
                return pair

            pairs = native.RUNTIME.repeat(3, args.runs, trial)
            all_pairs[workload] = pairs
            SAVE(e.root / "pairs.json", all_pairs)
        summary = {}
        for workload, pairs in all_pairs.items():
            summary[workload] = {
                state: {
                    key: native.paired(
                        [
                            {"docker": p["docker-" + state], "pvisor": p["pvisor-" + state]}
                            for p in pairs
                        ],
                        key,
                    )
                    for key in [
                        "ready_ns",
                        "exit_ns",
                        "content_bytes",
                        "metadata_bytes",
                        "response_bytes",
                        "requests",
                    ]
                }
                for state in ["cold", "warm"]
            }
        SAVE(e.root / "summary.json", summary)
    finally:
        if process.poll() is None:
            try:
                exchange_request = {"stop": True}
                process.stdin.write(json.dumps(exchange_request) + "\n")
                process.stdin.flush()
                process.wait(timeout=20)
                assert process.returncode == 0, "private worker cleanup failed"
            except (OSError, subprocess.TimeoutExpired):
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)


def serve_registry(config_path):
    config = json.loads(config_path.read_text())
    server = http.server.ThreadingHTTPServer(("127.0.0.1", config["port"]), Registry)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(config["cert"], config["key"])
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.guard, server.objects = threading.Lock(), {}
    server.rows_path = Path(config["rows"])
    for image in config["images"]:
        server.objects[image["digest"]] = (
            Path(image["manifest_path"]),
            image["manifest"]["mediaType"],
        )
        for d in [image["manifest"]["config"], *image["manifest"]["layers"]]:
            server.objects[d["digest"]] = (Path(image["source"]) / d["digest"][7:], d["mediaType"])
    print(json.dumps({"ready": True, "port": server.server_address[1]}), flush=True)
    server.serve_forever()


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--registry":
        serve_registry(Path(sys.argv[2]))
        return
    if len(sys.argv) == 3 and sys.argv[1] == "--worker":
        worker(Path(sys.argv[2]))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phase", choices=["storage", "runtime", "remote"], required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--daemon", type=Path)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--memory-rounds", type=int, default=5)
    args = parser.parse_args()
    if (
        min(args.repeats, args.memory_rounds) < 1
        or args.runs < 2
        or (args.phase == "remote" and args.daemon is None)
    ):
        parser.error("positive sample counts required; remote also requires --daemon")
    e = native.Evidence(args)
    sources = [
        Path(__file__),
        Path(native.__file__),
        native.RUNTIME_PATH,
        Path(wire.__file__),
        Path(wire.lazy_startup.__file__),
    ]
    frozen = {}
    for source in sources:
        shutil.copy2(source, e.root / source.name)
        frozen[str(source)] = native.sha(source)
    e.save(benchmark_id="B-LAZY-STARTUP", source_sha256=frozen)
    try:
        tools = {
            name: {"path": shutil.which(name), "sha256": native.sha(shutil.which(name))}
            for name in [
                "docker",
                "dockerd",
                "rootlesskit",
                "pasta",
                "runc",
                "containerd",
                "openssl",
            ]
        }
        e.save(tools=tools)
        (e.root / "processes-before.txt").write_text(
            e.command(["ps", "-eo", "pid,ppid,psr,pcpu,etime,args"])
        )
        {"storage": native.storage, "runtime": native.runtime, "remote": remote}[args.phase](
            e, args
        )
        assert native.sha(e.binary) == e.manifest["binary_sha256"]
        assert all(native.sha(path) == digest for path, digest in frozen.items())
        (e.root / "processes-after.txt").write_text(
            e.command(["ps", "-eo", "pid,ppid,psr,pcpu,etime,args"])
        )
        e.save(
            status="complete", finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat()
        )
    except BaseException as error:
        e.save(status="failed", error=repr(error))
        raise


if __name__ == "__main__":
    main()
