# RAM offload

Offload trades an idle instance's resident RAM for storage-backed reads on its next interaction.
The original VMM stays alive: CPU, device, connection and clock state remain in that process.

## Place in the architecture {#architecture}

The unit is one instance, not a shared memory pool or a portable VM image.
Within the [memory optimization architecture](index.md), offload addresses waiting time;
[local compression](compression-local.md) addresses a different RAM/CPU tradeoff.

Same-process resume continues the original execution. A RAM backing file alone cannot restart
that execution after the VMM exits, restore a historical VM or migrate it to another node.
[Full environment snapshots](../environment-snapshot.md) have a separate capture and restore contract,
including CPU/device state, compatibility and the filesystem covered by the execution profile.

## Pause is not quiescence {#quiescence}

Ordinary pause stops vCPUs but leaves device memory access available; it neither establishes
an immutable RAM epoch nor commits compressed storage. Offload needs a stronger boundary:
stop CPU writes, close and drain the device memory gate, and synchronize dirty backing pages.

CPU and devices stay quiescent while the offload operation completes. A runner acknowledgement
is not the completion point: host-side compressed publication may still be outstanding.
Resume restores required hypervisor mappings, reopens the device gate and restarts the vCPUs.

## Backing identity and storage choice {#backing}

RAM uses a shared file mapping. Stable backing identity—not a particular pathname—lets the
same process fault pages back without replacing its host RAM addresses or copying a new image.
Current path publication uses a hard link to the original inode; a compressed alias links the
manifest, whose reference to the original sidecar directory remains unchanged.

A regular file on disk-backed storage makes clean pages eligible for eviction and later reads.
A file on tmpfs or a memfd is still memory-backed: changing the backing type is not disk eviction.
Such objects may be useful for sharing or mapping, but their reclamation depends on host policy
and possible swap; a file descriptor alone does not establish durable storage or RAM savings.

Current whole-instance reclaim rejects private COW RAM. Discarding modified private pages would
lose guest writes rather than write them into the shared base; a new complete capture is needed.
This offload path must not be applied indiscriminately to snapshot-restored private mappings.

## Raw and compressed paths {#storage-paths}

Raw backing lets the kernel write RAM bytes directly to the file and satisfy later page faults.
The compressed path exposes a logical RAM file through FUSE: writeback stages raw pages,
then offload commits changed blocks into a base/delta bundle; reads decode committed blocks.
[Offload file format](offload-format.md) explains the storage and publication model.

Compression can reduce stored bytes, but adds encoding, checksums, staging I/O and FUSE work.
Resuming adds block decoding to cold reads. Incompressible data, frequent rewrites and compaction
can erase the benefit or increase pause time; compare CPU, I/O, storage peak and resume latency.

## Current path and future goals {#status}

The existing path is: create backing → run on shared mappings → quiesce CPU and devices →
synchronize/request reclaim → finish compressed commit if enabled → resume the same instance.
Reclamation is a host request, not a promise that all RAM disappears; VMM/device state remains,
and compression itself reads and allocates memory after the runner's residency sample.

Current support is the same-process raw/FUSE-backed path described in the
[implementation appendix](../offload/index.md), with its platform requirements and known limits.
A future goal is predictable idle-instance density with bounded offload and cold-resume costs;
this does not claim new support for private COW eviction, durable VM recovery or migration.
