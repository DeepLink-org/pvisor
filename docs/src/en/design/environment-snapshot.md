# Full environment snapshots and CLI migration

The standalone `pvisor snapshot` frontend has been removed. VM/Cluster and storage SDKs retain complete-state sealing, RAM encodings, base references and restoration, without exposing another independent instance workflow. Existing stores are not automatically converted into ordinary Job checkpoints, and removing the entry adds no capture capability.

## Current user entries {#current-entry}

Ordinary Job checkpoints, suspension, continuation and branches use `checkpoint`, `suspend`, `resume` and `fork`. Stopped-Job workspace checkpoints are connected; execution support remains an explicit executor/profile capability decision.

```bash
pvisor checkpoint create last --request-id before-refactor --json
pvisor checkpoint list last --json
pvisor fork last --state workspace --stage ./stage/branch -- codex
```

`checkpoint --kind execution`, `suspend/resume` and execution forks are not unconditional replacements for the retired snapshot frontend. Complete execution handoff for ordinary VM Jobs still has implementation boundaries; see [Job checkpoint design](job-checkpoint-cli.md#10-当前实现与验收边界).

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
