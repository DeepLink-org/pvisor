"""Guest/native worker for B-DENSITY; hold readiness before useful tool work."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

payload_mib = int(sys.argv[1])
expected_cpus = set(map(int, sys.argv[2].split(',')))
os.sched_setaffinity(0, expected_cpus)
assert os.sched_getaffinity(0) == expected_cpus, 'worker CPU affinity differs'
# Touch every private byte, rather than reporting reserved virtual addresses.
data = bytearray(b'x' * (payload_mib * 1024 * 1024))
expected = hashlib.sha256(data).hexdigest()
nonce = str(time.time_ns())
print('PVISOR_DENSITY_READY ' + json.dumps(dict(token=nonce, bytes=len(data), checksum=expected)), flush=True)
assert sys.stdin.readline().strip() == 'GO', 'missing readiness barrier release'
started = time.perf_counter_ns()
changed = 0
if payload_mib:
    for i in range(4):
        path = Path('files') / f'f{i:03d}'
        assert path.read_text() == f'old-{i}\n'
        path.write_text(f'new-{i}\n')
    status = subprocess.check_output(['git', '-c', 'safe.directory=' + os.getcwd(), 'status', '--porcelain'], text=True)
    assert {line[3:] for line in status.splitlines()} == {f'files/f{i:03d}' for i in range(4)}
    for i in range(64):
        assert (Path('files') / f'f{i:03d}').read_text() == f'{"new" if i < 4 else "old"}-{i}\n'
    changed = 4
assert hashlib.sha256(data).hexdigest() == expected, 'private memory changed'
print('PVISOR_DENSITY_RESULT ' + json.dumps(dict(token=nonce, bytes=len(data), checksum=expected,
    changes=changed, integrity='passed', tool_ms=(time.perf_counter_ns()-started)/1e6)), flush=True)
