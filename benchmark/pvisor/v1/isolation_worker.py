import json
import os
import socket
import sys
from pathlib import Path

outside, lower, socket_path, affinity_text = sys.argv[1:5]
affinity = set(map(int, affinity_text.split(',')))
os.sched_setaffinity(0, affinity)
assert os.sched_getaffinity(0) == affinity
result = {}
result['inside-read'] = Path('inside').read_text() == 'benchmark-inside'
first, second = socket.socketpair()
with first, second:
    first.sendall(b'allowed-inside-socket')
    result['inside-socket'] = second.recv(64) == b'allowed-inside-socket'
for name, path in [
    ("absolute", outside + "/secret"),
    ("symlink", "escape/secret"),
    ("proc-root", "/proc/self/root" + outside + "/secret"),
    ("traversal", "../outside/secret"),
]:
    try:
        result[name + "-read"] = Path(path).read_text() == "benchmark-secret"
    except OSError:
        result[name + "-read"] = False
for name, path in [
    ("absolute", outside + "/written"),
    ("symlink", "escape/symlink-written"),
    ("lower-alias", lower + "/alias-written"),
]:
    try:
        Path(path).write_text("benchmark-write")
        result[name + "-write"] = True
    except OSError:
        result[name + "-write"] = False
try:
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(1)
        stream.connect(socket_path)
        result["unix-socket"] = stream.recv(32) == b"benchmark-socket"
except OSError:
    result["unix-socket"] = False
Path("staged-marker").write_text("benchmark-write")
result['inside-write'] = Path('staged-marker').read_text() == 'benchmark-write'
result['cpu_affinity'] = sorted(affinity)
print(json.dumps(result))
