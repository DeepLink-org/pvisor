# Recorded SWE tool traces

The 30-task SWE-bench Pro corpus is distributed as a separate data package.
It contains 2,486 Agent tool commands from NodeBB, Vuls and qutebrowser,
relative timestamps and waits, expected output/patch hashes, pinned inputs,
source licenses, and historical validation records. It excludes model text,
credentials, raw outputs, full patches, rootfs and RAM. Full experimental logs
remain in the private experiment archive.

Download [swe-tool-traces-30-v1.tar.gz](https://github.com/zhaoyuuu0624/pvisor/releases/download/swe-tool-traces-30-v1/swe-tool-traces-30-v1.tar.gz) (425,707 bytes).
This versioned data asset is hosted on the contributor's fork, separately from
pVisor software releases. `data-package.json` pins its URL, size and SHA-256.

SHA-256: `8659e15a30f5dc808e312f650483539a4b369b7eb40b5b48fd3dd60b03340246`.

After obtaining the archive, verify it before extraction:

```bash
python3 - <<'PYCODE'
import hashlib, json
from pathlib import Path
manifest = json.loads(Path('benchmark/pvisor/sweeval/traces/data-package.json').read_text())
archive = Path(manifest['file'])
assert archive.stat().st_size == manifest['bytes']
assert hashlib.sha256(archive.read_bytes()).hexdigest() == manifest['sha256']
print('Archive verified')
PYCODE
```

Extract into a separate experiment directory. The archive contains its own
per-task manifest and four original license texts with a task mapping; keep
those files together. Preserve the original data and failed attempts.

The timing data supports omitting recorded waits or retaining inter-tool and
trailing waits; it does not add a replay implementation. API intervals include
queueing, inference and network, not isolated reasoning. The historical record
includes output differences and VM finalization failures; packaging those
records does not turn them into passing runs or demonstrate offload savings.
