# VM pause, resume and file-backed RAM cases

These DOC specifications extend [the existing cases](cases.md) on Linux/KVM and
Apple Silicon macOS/HVF. These specifications still await human review;
execution success is not approval. Review the SDK driver at
`crates/pvisor/examples/vm_control_case.rs` together with this specification.

`mise run cases` discovers the linked native specifications. The four SDK cases
explicitly report SKIP and fail the gate when the SDK driver is unavailable.
`mise run cases -- --suite vm` builds the driver, signs it on macOS,
and runs all six cases on this page. To use existing binaries:

```sh
mise run cases -- --suite vm --subject-bin target/release/pvisor
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

Execution and assertions: [Scrut S-DOC-057](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-057.md).

### S-DOC-058: N02 Startup cannot overwrite an existing RAM file


**Semantics**: Startup rejects an existing backing file, preserves its contents, and does not execute the guest command.

**Violation example**: The existing file is truncated or the guest executes after rejection.

Execution and assertions: [Scrut S-DOC-058](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-058.md).

### S-DOC-059: N03 Idempotent pause/resume and startup backing reclamation


**Semantics**: Repeated pause stops guest progress; repeated resume continues execution. Two offloads retaining the startup backing preserve guest memory, suspended/running states, and completion observations.

**Violation example**: Heartbeat progresses while paused, resume fails to advance execution, or repeated reclamation corrupts guest memory.

Execution and assertions: [Scrut S-DOC-059](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-059.md).

### S-DOC-060: N04 Offload publishes a new name for the live inode


**Semantics**: Offload selects a new path on the same filesystem with the same inode as the startup backing. The reported path and RAM range are correct, guest memory survives resume, and repeated offload keeps the selected file.

**Violation example**: Publication copies into another inode, reports the wrong path, corrupts guest RAM, or unexpectedly reverts to the original filename.

Execution and assertions: [Scrut S-DOC-060](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-060.md).

### S-DOC-061: N05 Rejected overwrite leaves the VM running


**Semantics**: Offload rejects an existing destination without modifying it, suspending or cancelling the VM. Guest progress continues and subsequent pause/resume still completes.

**Violation example**: Rejection truncates the destination, pauses or cancels the VM, or desynchronizes later acknowledgements.

Execution and assertions: [Scrut S-DOC-061](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-061.md).

### S-DOC-062: N06 Seekable incremental backing reclaims and restores RAM


**Semantics**: Compression selected at startup produces a valid PVZRAM v2 manifest and immutable base/delta bundle after offload. Logical RAM is complete, and allocated disk space for the entire bundle remains smaller than RAM for this workload after every offload. Writes after each resume produce a distinct generation whose chain reads original bytes. Ten offload/resume cycles exercise compaction from eight layers to one; unreachable generations are collected, and at most eight layers remain. The guest SHA-256 and continuously modified 1 MiB of page contents survive all cycles, with matching control observations and states. A generation does not claim a device-consistent atomic VM snapshot.

Linux requires FUSE; macOS requires the macFUSE kernel backend.

**Violation example**: The file is raw RAM, a missing parent is read as zero, no new head is committed, the Seekable index is corrupt, page faults return compressed bytes, mutable guest pages are lost after reclamation, or old layers accumulate after compaction.

Execution and assertions: [Scrut S-DOC-062](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-062.md).

## VM check preparation

This preparation declares hardware prerequisites and preserves SDK failure diagnostics.

Preparation and assertion source: [fixtures/vm.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/vm.bash).
