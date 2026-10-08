# Snapshot subsystem: consistency and state storage

Native VM and storage SDKs provide complete-state sealing, RAM encodings, base references and restoration. The CLI exposes native full execution capture and restore through ordinary Jobs, not a standalone `pvisor snapshot` workflow; existing stores are not automatically converted into Job checkpoints.

## The consistent cut: work remaining after CPUs park {#consistent-cut}

![Freezing, capturing and restoring CPU, device queues, RAM and files](assets/checkpoint-cut.svg)

| State | Capture owner | Relationship required for restore |
| --- | --- | --- |
| CPU and devices | VM runtime | CPU state, queue progress, interrupts and device topology share a freeze window |
| RAM | VM mapping layer and host storage | Address layout, byte identity, backing and leases agree |
| Exported filesystems | File service and host coordinator | Profile-specific file copies, lower references and inode/handle bindings remain recoverable |
| Durable objects and Job head | Snapshot store and Job service | Validate complete objects and compatibility before publishing references for future restore |

A normal pause acknowledges parked vCPUs. Virtio devices may still hold guest RAM buffers and write I/O results into them. Copying RAM before saving queues can restore a used ring indicating completion alongside a buffer missing the result.

A full freeze parks CPUs, waits for device workers to stop and return queue ownership, drains device RAM leases, and closes the access gate. Waiting releases the VMM lock so completion paths can progress. CPU, RAM and devices captured within that full window form compatible machine state; filesystems must also be retained according to the capture profile.

The VM runtime supplies the window. Storage ownership covers required capture, references, compatibility and durable publication. Restore remains paused while validated RAM, CPU, devices and file copies are installed, then releases execution. A freeze timeout cannot let unfinished workers race with resumed execution; the caller must terminate the failed runner.

Network peers do not rewind with a local snapshot, so the current native Job execution profile excludes network devices. Workspace checkpoints and Agent trajectories also do not contain this full machine cut.

## Storage and internal lifecycle {#storage-contract}

Sealed objects retain compatibility bindings, CPU/device state and RAM, plus filesystems actually captured by their profile. Immutable bases and compressed content support reuse while restored writes remain private. Ordinary boot does not automatically share guest anonymous RAM.

The `SnapshotStore` SDK retains validation, references and GC. `open_for_restore`/`ram_reader` provide leased demand reads. `open_owned_stage_for_restore` and `materialize_owned_stage` copy stages authenticated by local publication, retaining compatibility, base pins, topology checks and publication leases; externally modified payloads are unsupported. Full audits continue through `open`/`open_for_restore`.

The RAM server/watchdog use private native-runner modes with EOF drain and cleanup. They do not require a public `snapshot` command or change `run` arguments and default execution.

### Host-only snapshot RAM mounts {#ram-runtime}

Snapshot data and transient RAM mounts have separate lifetimes. Native restore and node sharing place RAM mounts in a validated private host runtime directory, never under the snapshot store or node state. On Linux, selection tries `/run/user/<effective-UID>` with verified ownership and `0700` permissions, then literal `/tmp` with verified root ownership and `1777` permissions; macOS uses `/private/tmp`. Each mount directory is `0700`. Selection rejects symlink ancestors and overlap with known workspace, HOME/XDG and state roots; it does not use `TMPDIR` or `XDG_RUNTIME_DIR` to choose the mount location. If no safe location is available, preparation fails rather than falling back into durable state.

`SnapshotRamMount::new` and `PublishedEnvironment::ram_mount` treat their `directory` argument as an excluded state root, not as the parent for mountpoints. Callers must not discover mounts by enumerating that directory. Keep the mount owner alive until all RAM files and VM mappings have been released. External pager specifications use separate private runtime directories with `0600` files, removed after readiness or startup failure. Owner EOF triggers cleanup; helper waits are bounded and abnormal helper exits invoke cleanup for that owner's private mount only. Persistent snapshot data is not removed by mount cleanup, and existing legacy mounts are not automatically adopted or unmounted.

Linux rootless host staging still does not support nested mounts within a projected state root. It fails before Agent execution rather than exposing writable submounts or silently hiding their contents. Diagnostics identify the state root, covering mount and nested mounts, and preserve the underlying mount error. The private runtime directory keeps pVisor's transient RAM mounts outside projected state roots; this separation does not establish support for arbitrary user mount layouts.

## Current user entries {#current-entry}

Ordinary Job checkpoints, suspension, continuation and branches use `checkpoint`, `suspend`, `resume` and `fork`. Stopped-Job workspace checkpoints and VM execution checkpoints on eligible native profiles are supported.

```bash
pvisor checkpoint create last --request-id before-refactor --json
pvisor checkpoint list last --json
pvisor fork last --state workspace --stage ./stage/branch -- codex
```

The former save maps to suspend; resume continues the current head, while fork --state execution --checkpoint ID restores history or creates branches. Former run uses ordinary run --executor vm; list/delete/gc and base import/verification belong to Job checkpoint commands. Native VMs with no network devices, private RAM and owned complete rootfs support capture-and-continue and full restoration; ordinary run configuration is not changed automatically. See the [CLI reference](../reference/cli.md#full-vm-execution-checkpoints) for entries and limits, and [Job checkpoint design](job-checkpoint-cli.md#10-当前实现与验收边界) for handoff.

The [single-node daemon](daemon/index.md) uses native VM execution, but does not implement capture/restore APIs. Its pause/resume is acknowledged live vCPU control on the same Attempt, not cgroup freeze or a snapshot. Snapshot, checkpoint/fork, stage/apply and offload APIs remain absent. Do not use retired Cluster controls as snapshot entries.

Native node backing ownership remains separate from daemon lifecycle; see [responsibility convergence](daemon/responsibility-convergence.md).

## Historical evidence {#historical-evidence}

Standalone snapshot correctness/latency records from 2026-10-03/04 belong to their recorded source/artifacts. Historical harnesses require an explicit archived binary that still supports the retired command; the current binary cannot reproduce them. They are not evidence of delivered ordinary Job execution profiles. Raw data remains in [VM memory experiments](../benchmarks/vm-memory/index.md).

## System connections and source map {#integration}

Complete snapshots connect VM, memory and file services: the VM supplies quiescence, storage pins recoverable bytes, and the Job service manages selectable checkpoints and execution identity. A successful Journal event cannot replace the machine payload; an existing storage object alone cannot prove that the Job head advanced. Recovery reconciles each contract separately.

Source entries: `crates/pvisor-vm/src/handle.rs` and `devices/snapshot.rs` own machine capture and device contracts. `crates/pvisor/src/environment_snapshot/store.rs` and `filesystems.rs` own storage and file copies. `crates/pvisor/src/executor/vm/restore_ram.rs` attaches restored RAM in the runner. See [Job checkpoint design](job-checkpoint-cli.md) for identity and retries, and [Journal](journal.md) for fact commits.
