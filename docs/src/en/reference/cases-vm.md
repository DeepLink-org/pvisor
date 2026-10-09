# VM pause, resume and file-backed RAM cases

These DOC specifications extend [the existing cases](cases.md) on Linux/KVM and
Apple Silicon macOS/HVF. New specifications are UNREVIEWED and have not been
compiled or executed. Review the SDK driver at
`crates/pvisor/examples/vm_control_case.rs` together with this specification.

`just cases` discovers this page. The four SDK cases explicitly SKIP when the
SDK driver is unavailable. `just cases --suite vm` builds the driver, signs it on macOS,
and runs all six cases on this page. To use existing binaries:

```sh
just semspec run docs/src/zh/reference/cases-vm.md --subject-bin target/release/pvisor
```

macOS requires `PVISOR_CASE_ROOTFS` pointing to a Linux guest rootfs; Linux defaults
to `/`. SDK cases require guest `/usr/bin/python3`, configurable through
`PVISOR_CASE_VM_PYTHON`. Use `PVISOR_CASE_VM_DRIVER` and
`PVISOR_CASE_VM_LIBRARY_DIR` for the driver and firmware directory.

The guest uses multiple vCPUs and checks 8 MiB of fixed data plus 1 MiB of
continuously modified page contents. Heartbeat/release files
in the dedicated stage upper coordinate the SDK driver and guest. This is a test
fixture, not a product control plane. Checks require guest progress to stop when
paused, continue after resume, and preserve SHA-256 after repeated offloads.
They do not require resident memory to reach zero.

### S-DOC-057: N01 Startup RAM file persists after exit


**Semantics**: A new startup RAM file permits successful guest execution, has permissions 0600 and nonzero size, and remains after normal exit.

**Violation example**: The file is absent, accessible by other users, or deleted at normal exit.

<!-- semspec: case id=S-DOC-057 -->
```bash
require_vm_case
vm_case_setup
pvisor run --no-agent-defaults --gateway-mode off --rootfs "$PVISOR_CASE_ROOTFS" \
  --vm-ram-backing "$CASE_ROOT/startup.ram" --stdio capture -- /bin/sh -c 'printf ram-startup-ok'
bundle_expect run.state completed
bundle_expect run.exit_code 0
bundle_contains run.output.stdout ram-startup-ok
python3 - "$CASE_ROOT/startup.ram" <<'PY'
import pathlib, stat, sys
p = pathlib.Path(sys.argv[1])
assert p.is_file() and p.stat().st_size > 0
assert stat.S_IMODE(p.stat().st_mode) == 0o600
PY
```

### S-DOC-058: N02 Startup cannot overwrite an existing RAM file


**Semantics**: Startup rejects an existing backing file, preserves its contents, and does not execute the guest command.

**Violation example**: The existing file is truncated or the guest executes after rejection.

<!-- semspec: case id=S-DOC-058 -->
```bash
require_vm_case
vm_case_setup
printf 'keep-existing-ram' > "$CASE_ROOT/startup.ram"
expect_refused pvisor run --no-agent-defaults --gateway-mode off --rootfs "$PVISOR_CASE_ROOTFS" \
  --vm-ram-backing "$CASE_ROOT/startup.ram" --stdio capture -- /bin/sh -c 'printf unexpected-guest'
assert_content "$CASE_ROOT/startup.ram" 'keep-existing-ram'
bundle_expect run.state failed
python3 - "$PVISOR_CASE_RECORDS" <<'PY'
import json, pathlib, sys
bundles = list(pathlib.Path(sys.argv[1]).rglob('run-bundle.json'))
assert bundles
bundle = json.loads(max(bundles, key=lambda p: p.stat().st_mtime).read_text())
assert bundle['run']['state'] == 'failed'
assert bundle['run']['output'].get('stdout') is None
PY
```

### S-DOC-059: N03 Idempotent pause/resume and startup backing reclamation


**Semantics**: Repeated pause stops guest progress; repeated resume continues execution. Two offloads retaining the startup backing preserve guest memory, suspended/running states, and completion observations.

**Violation example**: Heartbeat progresses while paused, resume fails to advance execution, or repeated reclamation corrupts guest memory.

<!-- semspec: case id=S-DOC-059 -->
```bash
require_vm_sdk
vm_case_setup
vm_run_sdk pause
grep -Fq 'vm-control-case-ok pause' "$CASE_ROOT/sdk.log"
test -s "$CASE_ROOT/startup.ram"
```

### S-DOC-060: N04 Offload publishes a new name for the live inode


**Semantics**: Offload selects a new path on the same filesystem with the same inode as the startup backing. The reported path and RAM range are correct, guest memory survives resume, and repeated offload keeps the selected file.

**Violation example**: Publication copies into another inode, reports the wrong path, corrupts guest RAM, or unexpectedly reverts to the original filename.

<!-- semspec: case id=S-DOC-060 -->
```bash
require_vm_sdk
vm_case_setup
vm_run_sdk offload
grep -Fq 'vm-control-case-ok offload' "$CASE_ROOT/sdk.log"
python3 - "$CASE_ROOT/startup.ram" "$CASE_ROOT/offloaded.ram" <<'PY'
import os, stat, sys
assert os.path.samefile(sys.argv[1], sys.argv[2])
assert stat.S_IMODE(os.stat(sys.argv[2]).st_mode) == 0o600
PY
```

### S-DOC-061: N05 Rejected overwrite leaves the VM running


**Semantics**: Offload rejects an existing destination without modifying it, suspending or cancelling the VM. Guest progress continues and subsequent pause/resume still completes.

**Violation example**: Rejection truncates the destination, pauses or cancels the VM, or desynchronizes later acknowledgements.

<!-- semspec: case id=S-DOC-061 -->
```bash
require_vm_sdk
vm_case_setup
vm_run_sdk reject
grep -Fq 'vm-control-case-ok reject' "$CASE_ROOT/sdk.log"
assert_content "$CASE_ROOT/occupied.ram" keep
```

### S-DOC-062: N06 Seekable incremental backing reclaims and restores RAM


**Semantics**: Compression selected at startup produces a valid PVZRAM v2 manifest and immutable base/delta bundle after offload. Logical RAM is complete, and allocated disk space for the entire bundle remains smaller than RAM for this workload after every offload. Writes after each resume produce a distinct generation whose chain reads original bytes. Ten offload/resume cycles exercise compaction from eight layers to one; unreachable generations are collected, and at most eight layers remain. The guest SHA-256 and continuously modified 1 MiB of page contents survive all cycles, with matching control observations and states. Linux requires FUSE; macOS requires the macFUSE kernel backend. A generation does not claim a device-consistent atomic VM snapshot.

**Violation example**: The file is raw RAM, a missing parent is read as zero, no new head is committed, the Seekable index is corrupt, page faults return compressed bytes, mutable guest pages are lost after reclamation, or old layers accumulate after compaction.

<!-- semspec: case id=S-DOC-062 -->
```bash
require_vm_compression
vm_case_setup
vm_run_sdk compressed
grep -Fq 'vm-control-case-ok compressed' "$CASE_ROOT/sdk.log"
grep -Fq 'compressed-cycle=9 depth=1 layers=1 ' "$CASE_ROOT/sdk.log"
grep -Fq 'compressed-cycle=10 depth=2 layers=2 ' "$CASE_ROOT/sdk.log"
python3 - "$CASE_ROOT/startup.ram" <<'PY'
import hashlib, json, pathlib, struct, sys
manifest = pathlib.Path(sys.argv[1]).read_bytes()
assert manifest[:8] == b'PVZRAM\0\0'
n = struct.unpack_from('<I', manifest, 8)[0]
descriptor = json.loads(manifest[12:12+n])
assert descriptor['version'] == 2
assert hashlib.sha256(manifest[12:12+n]).digest() == manifest[12+n:44+n]
records = manifest[44+n:]
assert len(records) >= 800 and len(records) % 80 == 0
heads = []
for i in range(0, len(records), 80):
    record = records[i:i+80]
    assert record[:8] == b'PVHEAD2\0'
    assert hashlib.sha256(record[:48]).digest() == record[48:]
    heads.append(record[16:48])
assert len(set(heads)) >= 10
root = pathlib.Path(descriptor['directory'])
head, seen = heads[-1], set()
while head is not None:
    assert head not in seen and len(seen) < 8
    seen.add(head)
    with (root / (head.hex() + '.pvdelta')).open('rb') as f:
        f.seek(-64, 2)
        footer = f.read(64)
        assert footer[:8] == b'PVSNAP2\0' and footer[32:] == head
        offset, size, seek_size = struct.unpack_from('<QQQ', footer, 8)
        assert offset + size + 64 == f.seek(0, 2)
        f.seek(offset)
        encoded = f.read(size)
        assert hashlib.sha256(encoded).digest() == head
        metadata = json.loads(encoded)
        assert metadata['version'] == 2 and metadata['block_bytes'] == 65536
        f.seek(offset - 9)
        count, flags, magic = struct.unpack('<IBI', f.read(9))
        assert flags == 0 and magic == 0x8F92EAB1 and seek_size == 17 + count * 8
        f.seek(offset - seek_size)
        magic, frame_size = struct.unpack('<II', f.read(8))
        assert magic == 0x184D2A5E and frame_size == seek_size - 8
        head = None if metadata['parent'] is None else bytes(metadata['parent'])
assert {p.name for p in root.glob('*.pvdelta')} == {h.hex() + '.pvdelta' for h in seen}
PY
```

## VM check preparation

This preparation declares hardware prerequisites and preserves SDK failure diagnostics.

<!-- semspec: setup -->
````bash
# VM case prerequisites and SDK driver selection; no product failure becomes SKIP.
require_vm_case() {
  require_python3
  case "$(uname -s)" in
    Linux)
      [ -r /dev/kvm ] && [ -w /dev/kvm ] || skip '/dev/kvm unavailable'
      export PVISOR_CASE_ROOTFS="${PVISOR_CASE_ROOTFS:-/}"
      ;;
    Darwin)
      [ "$(uname -m)" = arm64 ] || skip 'Apple Silicon required'
      [ -n "${PVISOR_CASE_ROOTFS:-}" ] || skip 'explicit Linux guest rootfs required'
      ;;
    *) skip 'VM unsupported on this OS' ;;
  esac
  [ -d "$PVISOR_CASE_ROOTFS" ] || skip 'guest rootfs unavailable'
}

vm_case_setup() {
  case_setup
  export XDG_CACHE_HOME="$CASE_ROOT/cache"
  mkdir -p "$XDG_CACHE_HOME"
}

require_vm_sdk() {
  require_vm_case
  export VM_CASE_DRIVER="${PVISOR_CASE_VM_DRIVER:-$(dirname "$SUBJECT_BIN")/examples/vm_control_case}"
  [ -x "$VM_CASE_DRIVER" ] || skip 'SDK driver unavailable; prepare with just cases --suite vm'
  export PVISOR_CASE_VM_PYTHON="${PVISOR_CASE_VM_PYTHON:-/usr/bin/python3}"
  [ -x "$PVISOR_CASE_ROOTFS/$PVISOR_CASE_VM_PYTHON" ] || skip 'guest Python unavailable'
}

require_vm_compression() {
  require_vm_sdk
  case "$(uname -s)" in
    Linux) [ -r /dev/fuse ] && [ -w /dev/fuse ] || skip '/dev/fuse unavailable' ;;
    Darwin) [ -d /Library/Filesystems/macfuse.fs ] || skip 'macFUSE kernel backend unavailable' ;;
  esac
}

# Preserve driver diagnostics even when errexit aborts a failing DOC case.
vm_run_sdk() {
  local status=0
  "$VM_CASE_DRIVER" "$1" > "$CASE_ROOT/sdk.log" 2>&1 || status=$?
  cat "$CASE_ROOT/sdk.log"
  [ "$status" -eq 0 ] || fail "VM SDK driver failed: exit=$status"
}
````
