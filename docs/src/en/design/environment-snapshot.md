# Full environment snapshots and CLI migration

The standalone `pvisor snapshot` frontend has been removed. VM/Cluster and storage SDKs retain complete-state sealing, RAM encodings, base references and restoration, without exposing another independent instance workflow. Ordinary Jobs now integrate native full execution capture and restore; existing stores are not automatically converted into Job checkpoints.

## Current user entries {#current-entry}

Ordinary Job checkpoints, suspension, continuation and branches use `checkpoint`, `suspend`, `resume` and `fork`. Stopped-Job workspace checkpoints and VM execution checkpoints on eligible native profiles are supported.

```bash
pvisor checkpoint create last --request-id before-refactor --json
pvisor checkpoint list last --json
pvisor fork last --state workspace --stage ./stage/branch -- codex
```

The former save maps to suspend; resume continues the current head, while fork --state execution --checkpoint ID restores history or creates branches. Former run uses ordinary run --executor vm; list/delete/gc and base import/verification belong to Job checkpoint commands. Native VMs with no network devices, private RAM and owned complete rootfs support capture-and-continue and full restoration; ordinary run configuration is not changed automatically. See the [CLI reference](../reference/cli.md#full-vm-execution-checkpoints) for entries and limits, and [Job checkpoint design](job-checkpoint-cli.md#10-当前实现与验收边界) for handoff.

Cluster tasks and VM controls use the service Cluster entry, retaining Task/Lease/control-revision identities and Worker reconciliation:

```bash
pvisor service cluster --help
```

For capped capture, branching and restoration, see the [VM and Gateway guide](../guides/cluster/vm-and-gateway.md). For node backing ownership, see the [unified service guide](../guides/cluster/service.md).

## Storage and internal lifecycle {#storage-contract}

Sealed objects retain compatibility bindings, CPU/device state and RAM, plus filesystems actually captured by their profile. Immutable bases and compressed content support reuse while restored writes remain private. Ordinary boot does not automatically share guest anonymous RAM.

The `SnapshotStore` SDK retains validation, references and GC. `open_for_restore`/`ram_reader` provide leased demand reads. `open_owned_stage_for_restore` and `materialize_owned_stage` copy stages authenticated by local publication, retaining compatibility, base pins, topology checks and publication leases; externally modified payloads are unsupported. Full audits continue through `open`/`open_for_restore`.

The former CLI RAM server/watchdog now enter private native-runner modes, retaining EOF drain/cleanup without a public `snapshot` command. `run` arguments and default execution keep their existing contracts.

## Historical evidence {#historical-evidence}

Standalone snapshot correctness/latency records from 2026-10-03/04 belong to their recorded source/artifacts. Historical harnesses require an explicit archived binary that still supports the retired command; the current binary cannot reproduce them. They are not evidence of delivered ordinary Job execution profiles. Raw data remains in [VM memory experiments](../benchmarks/vm-memory/index.md).
