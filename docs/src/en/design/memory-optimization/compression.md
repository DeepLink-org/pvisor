# Memory compression

Compression can reduce the cost of retaining cold RAM when encoding savings outweigh
storage, CPU, and recovery costs. Choose local ownership first; add a pool only
when sharing across instances justifies coordination.

## Target and current status {#status}

[Per-instance compression](compression-local.md) is delivered experimentally on
Linux x86_64 through the default-off `vm.cold_ram_compression` /
`--vm-cold-ram-compression`: a runtime-owned userfaultfd pager with bounded local
storage, no FUSE and no external pool. It is distinct from `vm.ram_compression`
(FUSE file backing), and is eviction/refault probing rather than a read-access
heat detector. Neither it nor [pooled-server compression](compression-pool.md)
is a production-benefit claim. The macOS/HVF heap pool remains experimental and
can fail dependent VMs when its service is lost; client-owned immutable backing
is still a proposal.

## Ownership and data flow {#architecture}

Each instance owns checkpoint save, compression, and restore independently of
runtime optimization. A [complete environment snapshot](../environment-snapshot.md)
includes machine state and durability; runtime cold objects retain only current
RAM fragments. A compressed checkpoint does not automatically reclaim live RAM.

The VM runtime owns cold-page selection and mapping changes; the codec transforms
bytes, and backing carries them. `memfd` does not compress automatically. The
target restores writable pages into instance-private RAM, even with shared objects.

## Choices and tradeoffs {#tradeoffs}

Local compression avoids service coordination but cannot share payloads across
instances. A pool adds optional [deduplication](deduplication.md), shared storage,
and host-wide budgets, at the cost of IPC, contention, and broader failure risks.
Both need temporary-memory and recovery headroom, CPU budgets, and acceptable tails.

## Direction and evidence {#direction}

The Linux local pager publishes and validates objects before reclaiming RAM,
then validates restoration through `UFFD_COPY`. Kernel-fault userfaultfd authority
is required; compiled support does not grant permission and missing authority
fails startup. It rejects dedup, file/FUSE backing, snapshot capture/restore,
whole-VM [offload](offload.md), and external memory pools; see the
[local contract](compression-local.md#direction). Linux sealed `memfd` pool
delivery remains proposed; a macOS sealed equivalent is not delivered. Start from the
[optimization architecture](index.md); [memory evidence](proof-of-concept.md#memory-evidence)
establishes no known production net gains or verified net physical-memory savings.
