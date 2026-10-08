# Pooled-server compression

Share encoded cold content across authorized instances while keeping recovery
under each client's control. Pooling is worthwhile only when shared storage and
encoding work save more than coordination and service overhead cost.

## Target and current status {#status}

The target gives clients their own immutable content handles and local decoding.
It is not the current macOS/HVF experimental heap pool: that service retains the
payload in its heap, and service loss can fail dependent VMs. Linux sealed
`memfd` delivery is proposed; no equivalent macOS sealed backing is delivered.

## Ownership and data flow {#architecture}

The server may index content, encode, coordinate publication, manage budgets,
and cache shared objects. It does not own guest addresses or machine snapshots.
Each instance independently owns checkpoint save/compress/restore through the
[environment-snapshot contract](../environment-snapshot.md).

The VM captures stable cold content; the server produces or reuses an immutable
encoded object. The client validates and pins its own handle before the runtime
reclaims RAM. Backing carries bytes; the codec compresses them. `memfd` provides
neither automatic compression nor deduplication.

A client decodes locally on CPU access or device preparation and restores private
writable pages. Shared encoded bytes must not connect independent writable RAM.
After delivery, server exit affects new publication, not access to pinned content;
before delivery, failure retains original RAM. These are proposed guarantees.

## Choices and tradeoffs {#tradeoffs}

Compared with [local compression](compression-local.md), a pool can avoid duplicate
payloads and redundant encoding. [Deduplication](deduplication.md) remains optional
and restricted to authorized trust domains; content identity is not authorization.

Pooling adds IPC, queueing, contention, metadata, and shared operational costs.
Client-owned objects narrow the availability boundary, but corruption or failed
restoration still requires stopping the affected runner rather than substituting
zeros or old checkpoint bytes. Runtime objects provide no host-crash durability.

Count server memory and shared-object occupancy as well as client RAM. Reserve
temporary-copy and decoding headroom, prioritize recovery over background work,
and evaluate CPU cost and application tail latency alongside compression savings.

## Direction and constraints {#direction}

Prove immutable delivery and recovery after service exit without reclaiming RAM
first. Then validate the Linux pager and CPU/device mapping integration before
enabling destructive reclamation. Keep expensive encoding and publication outside
freeze windows; [two-phase publication](proof-of-concept.md#two-phase-publication)
provides the mechanism lesson, not a proof that restoration contention is solved.

Keep the existing cold-pool incompatibility with whole-VM [offload/FUSE backing](offload.md),
and do not combine KSM and cold compression on the same RAM region.

## Evidence boundary {#evidence}

[Memory evidence](proof-of-concept.md#memory-evidence) establishes no known
production net gains or verified net physical-memory savings. Historical pool
results do not validate client-owned backing. The [architecture index](index.md)
and [compression overview](compression.md) keep those capability boundaries explicit.
