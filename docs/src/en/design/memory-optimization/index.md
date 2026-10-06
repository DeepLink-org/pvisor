# Memory optimization

Multiple pVisor instances retain similar execution environments and working sets that are temporarily unused between interactions. Memory optimization treats these costs separately: cross-instance deduplication removes duplicate copies, per-instance offload releases resident memory, and compression reduces the volume of content that must be retained.

## Overall architecture and principles {#architecture}

Instances own checkpoint saving, compression, and restoration; the VM runtime owns mappings and consistent CPU/device access. Memory optimization is an optional policy. It does not take ownership of the only copy of instance state or treat a running service as the recovery guarantee for reclaimed content.

- **Remove duplication first, compress next, offload when needed.** This is a selection principle, not a mandatory pipeline for every page.
- **Independent instance state, optional sharing.** Instances can save and restore without deduplication or a pooled service.
- **Content ownership precedes reclamation.** Pin a reliable recovery source before discarding original RAM; runtime objects are not persistent checkpoints.
- **Measure benefits and costs together.** Include CPU, transient peaks, pauses, and restoration latency. Compression ratio is not a substitute for net host-memory benefit.

`backing` carries content, deduplication finds identical content, compression changes encoding, and offload changes residency. KSM, memfd, and FUSE are implementation options rather than interchangeable backends: KSM does not compress, memfd does not deduplicate automatically, and FUSE does not provide a complete machine snapshot.

## Three capabilities {#capabilities}

| Capability | Primary goal | Main tradeoff |
| --- | --- | --- |
| [Memory deduplication](deduplication.md) | Share identical content across instances | Scanning and COW costs, trust domains, and sharing boundaries |
| [RAM offload](offload.md) | Move one instance's RAM into recoverable storage | Write costs, storage capacity, and restoration latency |
| [Memory compression](compression.md) | Reduce the volume of retained content | Encoding/decoding CPU, transient memory, and access amplification |

See [Offload file format](offload-format.md) for raw files and compressed base/delta bundles. Compression has [per-instance](compression-local.md) and [pooled-server](compression-pool.md) architectures: the former is independent; the latter can share encoded objects but adds coordination costs.

Immutable baseline sharing can complement Linux KSM: file baselines share unmodified pages, while KSM handles eligible private anonymous pages. The first version does not combine KSM and cold-page reclamation on the same RAM region. The local cold pager and cold pool remain incompatible with FUSE/file backing, snapshot capture/restore and whole-VM offload; `vm.ram_dedup` is explicitly exclusive with `vm.cold_ram_compression`. Combinations are explicit architectural choices, not automatic runtime switching.

## Ownership and boundaries {#ownership}

| Component | Owns | Does not own |
| --- | --- | --- |
| Instance coordinator and snapshot storage | Complete state, compatibility, publication, and persistent references | Cross-instance physical page identities |
| VM runtime | Private writes, mapping transitions, CPU/device access, and restoration | Pool indexes or global compression policy |
| Shared/compressed storage | Immutable content, encoding, references, and cache reclamation | Guest pointers or independent publication of complete checkpoints |
| Host policy | Total capacity, global scanning, and sharing authorization | Instance-state correctness |

Sharing is limited by default to explicitly authorized trust domains; the same UID is not complete tenant isolation. Sharing latency can reveal content presence, so cross-domain deduplication cannot be selected solely by savings. Physical reclamation also depends on the host kernel and storage: API success does not mean all target bytes have been released.

Space saved by compression cannot be converted unconditionally into capacity for new instances: restoration still needs uncompressed RAM and working buffers. Capacity planning must reserve restoration headroom so background optimization does not prevent recovery from making progress. The Linux local store bounds encoded payload to half configured RAM and object count separately; these are not total host-memory limits. Host-wide capacity and scheduling still need further design.

## Implementation direction and evidence {#direction}

The architectural goals start from the 2026-10-06 design and do not imply that every capability has shipped. Reuse immutable baselines and private COW mappings first, then introduce region-level Linux KSM advice. Experimental Linux instance-local live compression is now delivered; pooled coordination remains a separate extension.

Existing file offload, formats, and the experimental macOS/HVF cold pool provide implementation foundations. Linux x86_64 now supports the default-off `vm.cold_ram_compression` / `--vm-cold-ram-compression` with private anonymous RAM, runtime-owned kernel-fault userfaultfd and bounded `LocalColdRamStore`. It is ongoing guest-independent eviction/refault probing, not ordinary pause or a true read-access heat detector. Permission is required and startup fails without it; see [local compression](compression-local.md) for admission and recovery. Client-owned sealed shared backing remains proposed. A uniform API does not imply equal platform capabilities or host authority.

[Experimental proof of concept](proof-of-concept.md) preserves low-level COW, cold restoration, reference lifetimes, and historical failures. It establishes mechanism feasibility and limitations, not production density, net physical savings, or restoration tail latency for the new architecture. See [Environment snapshots](../environment-snapshot.md) for complete-state boundaries.
