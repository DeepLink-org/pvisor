"""Proxy and VM network latency and throughput against native.

Benchmark: B-NETWORK (benchmark/README.md#b-network), role user-facing.
Motivation: Agents make many small requests and bulk downloads; users need
the latency and throughput cost of network policy and VM networking.
Conclusion sought: ms added per small request by the proxy, VM bulk
throughput as a fraction of native, and whether either matters next to model
response time.
Design: local HTTP server, small-request latency and ~32 MiB transfers for
native, host proxy and VM; no public network; not model API latency.
"""

import json
import random
import shutil
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from .common import checked


class Origin(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        stream = self.path == "/stream"
        data = b"data: token\n\n" if stream else b"x" * 1024
        count = 10 if stream else (32768 if self.path == "/bulk" else 1)
        self.send_response(200)
        self.send_header("Content-Length", str(len(data) * count))
        self.end_headers()
        for _ in range(count):
            self.wfile.write(data)
            if stream:
                self.wfile.flush()
                time.sleep(0.002)

    def log_message(self, *args):
        pass


def run(ctx):
    addresses = json.loads(checked(["ip", "-4", "-json", "addr", "show"]).stdout)
    host = next(
        a["local"]
        for item in addresses
        for a in item["addr_info"]
        if a["scope"] == "global" and not a["local"].startswith("198.18.")
    )
    ThreadingHTTPServer.request_queue_size = 128
    server = ThreadingHTTPServer((host, 0), Origin)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    target = f"{host}:{server.server_port}"
    ctx.metadata["network_origin"] = dict(
        address=target, location="same host; no Internet", bytes=32 * 1024 * 1024
    )
    rng = random.Random(20261004)
    try:
        for mode in ctx.args.network_modes.split(","):
            backends = ctx.args.network_backends.split(",") if mode != "deny" else ["host", "vm"]
            for i in range(-ctx.args.warmups, ctx.args.samples):
                rng.shuffle(backends)
                for backend in backends:
                    root = ctx.fresh(f"net-{mode}-{backend}")
                    work = root / "workspace"
                    work.mkdir()
                    shutil.copy2(Path(__file__).with_name("network_worker.py"), work / "worker.py")
                    stage = root / "stage"
                    runs = root / "runs"
                    options = (
                        ["--overlaynet-deny-all"]
                        if mode == "deny"
                        else ["--overlaynet-allow", target]
                    )
                    command = ctx.command(
                        backend,
                        work,
                        stage,
                        ["/usr/bin/python3", "worker.py", mode, "http://" + target],
                        network=("auto" if backend == "vm" else "proxy", *options),
                    )
                    wall, stdout, _ = ctx.run(
                        command,
                        cwd=work,
                        env={"PVISOR_RUN_HOME": str(runs), "XDG_CONFIG_HOME": str(root / "config")},
                    )
                    bundle = ctx.validate_bundle(
                        backend,
                        runs,
                        stage,
                        expected_isolation="rootless_process"
                        if backend == "host" and mode == "deny"
                        else None,
                    )
                    if bundle:
                        stdout = bundle["run"]["output"]["stdout"]
                    value = json.loads(stdout.strip().splitlines()[-1])
                    row = dict(
                        suite="network",
                        workload=mode,
                        backend=backend,
                        trial=i,
                        wall_ms=wall,
                        worker_ms=value["worker_ms"],
                        check=value["check"],
                        correctness="passed",
                        logs=str(root),
                    )
                    if bundle:
                        row["safety"] = bundle["safety"]
                        if mode == "deny":
                            assert bundle["safety"]["network_non_bypassable"]
                    if i >= 0:
                        ctx.record(row)
                if i >= 0 and (i + 1) % 5 == 0:
                    print(f"network {mode}: {i + 1}/{ctx.args.samples}", flush=True)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
