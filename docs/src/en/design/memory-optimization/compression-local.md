# Per-instance compression

Keep compressible cold content under one instance's control, without needing a
pool to save or recover it. This favors a narrow ownership and failure boundary
over cross-instance sharing.

## Target and current status {#status}

Independent checkpoint save/compress/restore is an architectural requirement,
not a consequence of enabling runtime compression. The local runtime path is a
design direction, not a newly delivered Linux/KVM pager. The experimental
macOS/HVF heap pool does not establish an independent local-compression backend.

## Ownership and data flow {#architecture}

The instance coordinator owns [complete checkpoints](../environment-snapshot.md),
including CPU/device state, compatibility, and persistent storage. Checkpoint
encoding may reuse codec code without adopting runtime cold-object lifetimes or
durability. Saving a compressed checkpoint alone does not shrink running RAM.

For runtime compression, the VM selects cold content, captures a stable copy,
and retains a validated local encoded object before discarding original pages.
The codec owns encoding and bounded decoding; backing is only the carrier.
`memfd` would not provide automatic compression or a pager.

CPU access or device preparation restores current bytes into private writable
RAM. The runtime owns mappings and device access; guest applications need not
participate. Cold objects are not persistent recovery after a host crash.

## Choices and tradeoffs {#tradeoffs}

Local storage removes publication IPC and service availability from recovery,
but each instance keeps its own encoded payload and pays its own codec cost.
[Deduplication](deduplication.md) is optional, not required for correctness.

Long-lived, compressible cold content is the useful target. Hot or incompressible
content can cost more than resident RAM once object overhead and repeated decoding
are included. Prefer retaining original RAM when expected savings are insufficient.

Reserve temporary memory for stable copies and encoding, and recovery headroom
for decoded pages and scratch. Background work must yield to restoration;
compression ratio alone says nothing about CPU cost or application tail latency.

## Direction and constraints {#direction}

Start with non-destructive local storage and recovery, then integrate reclamation
with CPU/device access on real platforms. Linux pager support needs validation;
macOS has no delivered sealed equivalent. Reuse the separation of capture,
publication, and commit from [two-phase publication](proof-of-concept.md#two-phase-publication),
not the old pool's service-dependent recovery contract.

Keep the existing cold-pool exclusion with whole-VM [offload/FUSE backing](offload.md);
local ownership alone does not prove those mechanisms can safely compose.

## Evidence boundary {#evidence}

[Memory evidence](proof-of-concept.md#memory-evidence) does not establish known
production net gains. Compare total host occupancy, temporary peaks, CPU cost,
and application tails, not encoded size alone. See the [overview](compression.md)
and [pooled alternative](compression-pool.md) for the ownership choice.
