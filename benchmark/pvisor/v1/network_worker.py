import concurrent.futures
import hashlib
import http.client
import json
import os
import socket
import sys
import time
from urllib.parse import urlsplit


def request(url, path, stream=False):
    target = urlsplit(url)
    proxy = urlsplit(os.environ.get("http_proxy") or os.environ.get("HTTP_PROXY") or "")
    start = time.perf_counter_ns()
    connection = http.client.HTTPConnection(
        proxy.hostname or target.hostname, proxy.port or target.port, timeout=10
    )
    connection.connect()
    connect_ms = (time.perf_counter_ns() - start) / 1e6
    connection.request("GET", url + path if proxy.hostname else path)
    response = connection.getresponse()
    assert response.status == 200, response.status
    first = response.read(1)
    first_ms = (time.perf_counter_ns() - start) / 1e6
    content = first + response.read()
    connection.close()
    return {
        "elapsed_ms": (time.perf_counter_ns() - start) / 1e6,
        "connect_ms": connect_ms,
        "first_byte_ms": first_ms,
        "bytes": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }


def main():
    mode, url = sys.argv[1:3]
    start = time.perf_counter_ns()
    if mode == "deny":
        target = urlsplit(url)
        try:
            with socket.create_connection((target.hostname, target.port), timeout=1):
                raise AssertionError("direct connection bypassed deny-all")
        except (OSError, TimeoutError):
            result = {"direct_socket_blocked": True}
    elif mode == "small":
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            values = list(pool.map(lambda _: request(url, "/small"), range(256)))
        assert all(
            v["bytes"] == 1024 and v["sha256"] == hashlib.sha256(b"x" * 1024).hexdigest()
            for v in values
        )
        result = {"requests": values, "concurrency": 8}
    elif mode == "bulk":
        result = request(url, "/bulk")
        assert (
            result["bytes"] == 32 * 1024 * 1024
            and result["sha256"] == hashlib.sha256(b"x" * (32 * 1024 * 1024)).hexdigest()
        )
    elif mode == "stream":
        result = request(url, "/stream", True)
        assert result["bytes"] == 10 * len(b"data: token\n\n")
    else:
        raise ValueError(mode)
    print(
        json.dumps(
            {"mode": mode, "worker_ms": (time.perf_counter_ns() - start) / 1e6, "check": result}
        ),
        flush=True,
    )


if __name__ == "__main__":
    main()
