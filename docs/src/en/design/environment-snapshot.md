# Full environment snapshots and CLI migration

Native VM and storage SDKs provide complete-state sealing, RAM encodings, base references and restoration. The CLI exposes native full execution capture and restore through ordinary Jobs, not a standalone `pvisor snapshot` workflow; existing stores are not automatically converted into Job checkpoints.

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

## Storage and internal lifecycle {#storage-contract}

Sealed objects retain compatibility bindings, CPU/device state and RAM, plus filesystems actually captured by their profile. Immutable bases and compressed content support reuse while restored writes remain private. Ordinary boot does not automatically share guest anonymous RAM.

The `SnapshotStore` SDK retains validation, references and GC. `open_for_restore`/`ram_reader` provide leased demand reads. `open_owned_stage_for_restore` and `materialize_owned_stage` copy stages authenticated by local publication, retaining compatibility, base pins, topology checks and publication leases; externally modified payloads are unsupported. Full audits continue through `open`/`open_for_restore`.

The RAM server/watchdog use private native-runner modes with EOF drain and cleanup. They do not require a public `snapshot` command or change `run` arguments and default execution.

### Host-only snapshot RAM mounts {#ram-runtime}

Snapshot data and transient RAM mounts have separate lifetimes. Native restore and node sharing place RAM mounts in a validated private host runtime directory, never under the snapshot store or node state. On Linux, selection tries `/run/user/<effective-UID>` with verified ownership and `0700` permissions, then literal `/tmp` with verified root ownership and `1777` permissions; macOS uses `/private/tmp`. Each mount directory is `0700`. Selection rejects symlink ancestors and overlap with known workspace, HOME/XDG and state roots; it does not use `TMPDIR` or `XDG_RUNTIME_DIR` to choose the mount location. If no safe location is available, preparation fails rather than falling back into durable state.

`SnapshotRamMount::new` and `PublishedEnvironment::ram_mount` treat their `directory` argument as an excluded state root, not as the parent for mountpoints. Callers must not discover mounts by enumerating that directory. Keep the mount owner alive until all RAM files and VM mappings have been released. External pager specifications use separate private runtime directories with `0600` files, removed after readiness or startup failure. Owner EOF triggers cleanup; helper waits are bounded and abnormal helper exits invoke cleanup for that owner's private mount only. Persistent snapshot data is not removed by mount cleanup, and existing legacy mounts are not automatically adopted or unmounted.

Linux rootless host staging still does not support nested mounts within a projected state root. It fails before Agent execution rather than exposing writable submounts or silently hiding their contents. Diagnostics identify the state root, covering mount and nested mounts, and preserve the underlying mount error. Moving pVisor's own transient mounts avoids creating this conflict; it does not claim support for arbitrary user mount layouts.

## Historical evidence {#historical-evidence}

Standalone snapshot correctness/latency records from 2026-10-03/04 belong to their recorded source/artifacts. Historical harnesses require an explicit archived binary that still supports the retired command; the current binary cannot reproduce them. They are not evidence of delivered ordinary Job execution profiles. Raw data remains in [VM memory experiments](../benchmarks/vm-memory/index.md).
