# Memory compression

Compression can reduce the cost of retaining cold RAM when encoding savings outweigh
storage, CPU, and recovery costs. Choose local ownership first; add a pool only
when sharing across instances justifies coordination.

## Target and current status {#status}

The target separates [per-instance compression](compression-local.md) from
[pooled-server compression](compression-pool.md). Neither is a production-benefit
claim. The current macOS/HVF heap cold pool is experimental and can fail dependent
VMs when its service is lost; client-owned immutable backing is a proposal.

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

Prove independent recovery before reclaiming RAM. Linux sealed `memfd` delivery is
proposed and still requires pager validation; a macOS sealed equivalent is not
delivered. Keep the existing cold-pool incompatibility with whole-VM
[offload and FUSE compressed backing](offload.md). Start from the
[optimization architecture](index.md); [memory evidence](proof-of-concept.md#memory-evidence)
establishes no known production net gains or verified net physical-memory savings.
