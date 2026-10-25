# Pause, offload and resume a VM {#cli-example}

For submitting a task and collecting its result from a caller, start with the [infrastructure quick start](../../start/infrastructure.md). This example separately verifies live VM controls and memory continuity.

## 1. Prepare the server and image {#prerequisites}

Open two terminals on the same Linux x86_64 server. Complete [installation](../../start/installation.md), ensure your account can read and write `/dev/kvm`, and confirm that the system allows pVisor to create user namespaces. Check the CLI options:

```bash
pvisor --version
pvisor suspend --help
pvisor resume --help
```

Help should include `--vm-pause`, `--vm-offload`, and `--vm-load`. For `setgroups` or `uid_map` permission errors, ask the administrator to check user namespace and AppArmor policies; KVM group membership does not satisfy those requirements. AppArmor policies must match the actual pVisor executable path.

Prepare an independent **Linux amd64 rootfs** containing `/usr/local/bin/python3`. The demo uses a filesystem unpacked from `python:3.12-slim`; pin its digest for reproducibility. If the server cannot reach the registry, prepare a rootfs for the same architecture on a connected machine and transfer it while preserving permissions and symbolic links. Do not use the host `/`. See the [VM executor](../executors/vm.md) for preparation.

Work in a private directory with enough space, replacing `ROOTFS` with its actual absolute path:

```bash
mkdir -p pvisor-vm-demo/workspace
cd pvisor-vm-demo/workspace
export ROOTFS=/absolute/path/to/linux-rootfs
```

Use a new demo directory for each run, with the rootfs outside it. The configuration uses one vCPU and 256 MiB of guest RAM; the Host incurs additional memory overhead.

## 2. Start the task {#start-task}

In the first terminal, create a script that retains 8 MiB of data for integrity checks and prints a counter. Once the caller creates `finish`, it checks memory and writes the result. The file is only a completion signal for this demo.

```bash
cat > task.py <<'PY'
import hashlib
import json
import pathlib
import time

memory = bytearray(bytes(range(256)) * 32768)
expected = hashlib.sha256(memory).hexdigest()
counter = 0
print('ready', flush=True)
while not pathlib.Path('finish').exists():
    assert hashlib.sha256(memory).hexdigest() == expected
    counter += 1
    print(f'progress {counter}', flush=True)
    time.sleep(1)
assert hashlib.sha256(memory).hexdigest() == expected
result = {'memory_ok': True, 'steps': counter}
pathlib.Path('result.json').write_text(json.dumps(result) + '\n')
print(json.dumps(result), flush=True)
PY
```

Start the VM and keep the first terminal running:

```bash
pvisor spawn --executor vm --rootfs "$ROOTFS" \
  --overlaynet off --memory 256MiB --cpu 1 --timeout 5m \
  --vm-ram-backing "$PWD/../guest.ram" \
  --stage ../stage --mount "$PWD:stage" \
  -- /usr/local/bin/python3 -u task.py
```

**Expected result:** `ready` followed by increasing `progress N` lines. `task.py` uses a workspace-relative path without assuming a fixed guest working directory. `--overlaynet off` disables guest networking; do not add `--safe`, which requires network controls.

## 3. Pause and continue {#pause-resume}

In the second terminal, enter the same `pvisor-vm-demo/workspace` and use the same pVisor executable. Save the execution identity, then pause:

```bash
pvisor status ../stage --json > ../before.json
pvisor suspend ../stage --vm-pause > ../pause.json
```

**Expected result:** `value.vm.state` in `pause.json` is `paused`. After queued output drains, the counter stops. Pause stops vCPUs; memory usually remains resident.

```bash
pvisor resume ../stage --vm-load > ../pause-resume.json
```

**Expected result:** `value.vm.state` in `pause-resume.json` is `running`, and the counter continues from where it paused.

## 4. Offload memory and resume {#offload-resume}

```bash
pvisor suspend ../stage --vm-offload > ../offload.json
```

**Expected result:** `value.vm.state` in `offload.json` is `offloaded`. `value.vm.memory` contains the backing path and resident-page observations. Offload establishes a CPU/device quiescence boundary, synchronizes RAM to its backing, and requests resident-page reclamation.

```bash
pvisor resume ../stage --vm-load > ../offload-resume.json
touch finish
```

The counter continues after resume. `finish` tells the task to check memory, write `result.json`, and exit. Wait for the command in the first terminal to finish before collecting results.

These operations continue the **same VM Attempt**, requiring the original owner/VMM to remain alive. Plain `suspend` instead saves an execution checkpoint; see [checkpoint semantics](../../concepts/lifecycle.md#checkpoint-boundary). This demo uses ordinary file-backed RAM without compression, deduplication, or a shared pool. Resident-page observations are not total service memory usage and do not guarantee a reclamation amount.

## 5. Collect results {#collect-results}

Run in the second terminal:

```bash
pvisor status --review --json ../stage > ../result.bundle.json
pvisor inspect ../stage -- cat result.json > ../result.json
python3 - <<'PY'
import json
from pathlib import Path

root = Path('..')
before = json.loads((root / 'before.json').read_text())
bundle = json.loads((root / 'result.bundle.json').read_text())
result = json.loads((root / 'result.json').read_text())
assert bundle['run']['state'] == 'completed'
assert bundle['run']['exit_code'] == 0
assert bundle['run']['attempt_id'] == before['run']['attempt_id']
assert result['memory_ok'] is True and result['steps'] > 0
print('PASS: same Attempt, memory intact, task completed')
PY
```

**Expected result:** `PASS: same Attempt, memory intact, task completed`. `memory_ok` is `true`; `steps` varies with the timing of your operations. The caller can associate its task ID with the Bundle's Job/Attempt IDs before scoring or scheduling.

The VM exits when the task finishes. Retain the records and results you need, then discard unapplied candidates:

```bash
pvisor drop ../stage
```

The explicitly selected `guest.ram` is a caller-owned backing file; remove it once the VM has exited and it is no longer needed. To stop early, use `pvisor kill ../stage`. The five-minute task timeout continues during pauses.

This workflow was verified on 2026-10-24 with Linux x86_64/KVM and CLI `0.3.0+g35.b318715`: the counter stopped after pause/offload, continued after resume, retained its Attempt identity, passed memory checks, and exited with code 0. This verifies single-VM functionality, not concurrency density or automatic model-wait management.
