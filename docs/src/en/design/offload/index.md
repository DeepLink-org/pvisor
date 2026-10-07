# VM RAM offload

> See [VM RAM offload architecture](../memory-optimization/offload.md) for the architecture; this document remains the implementation and evidence appendix.

## 1. Motivation {#motivation}

Agent environments often need to preserve process state between interactions. Stopping a VM frees resources but loses volatile state; pausing its vCPUs still retains RAM. Offload serves the waiting interval: keep the VMM and device state alive, synchronize RAM to a file, request residency reclamation, then continue the same VM on the next interaction.

Raw file backing grows as pages are written. Compression reduces storage by encoding zero, uniform and compressible blocks, at the cost of CPU, I/O and pause time. The design must preserve ordinary memory access, establish a stable commit point and restore small portions without decoding all RAM.

The current scope is offload/resume within one live VMM. CPU, devices, connections and clocks remain in that process. RAM files alone cannot restart the VM or migrate it between nodes.

## 2. Core design {#core-design}

### File mapping as live RAM

The VMM maps a file with `MAP_SHARED`; vCPUs and virtio devices access the same addresses. The kernel handles faults and writeback. Offload synchronizes the backing and requests page reclamation; resume uses the same file identity. Offload does not copy RAM into a new file or replace the host mapping when the pathname changes.

Ordinary mode uses one raw file for both logical and physical FDs. Compressed mode separates them: the physical FD points to the manifest, while the logical FD points to FUSE `mount-*/ram`. FUSE decodes committed blocks on read and stages pages on writeback.

| Object | Responsibility |
|---|---|
| `VmControl` / `RamBacking` | Control exchange, FD/path ownership and storage lifetime |
| VMM / device memory gate | CPU pause, device drain, mapping reclamation and restore |
| `CompressedMount` / `RamFs` | Expose compressed storage as a mappable logical file |
| `CompressedRam` | Manifest, staging, dirty masks and commit |
| `SnapshotChain` | Generation encoding, validation, inheritance, compaction and GC |

### Manifest, generations and staging

In compressed mode `vm.ram` stores the directory and head, not RAM payload. Immutable `.pvdelta` files hold committed content: the first is a full base; later generations store changed blocks and reference a parent. Uncommitted writes remain in kernel dirty pages or staging.

Immutable generations avoid overwriting data still being read. Publication makes the generation durable before appending and synchronizing the manifest head. Staging remains writable for small updates and does not retain history.

Each nonuniform 64 KiB block is independently compressed at zstd level 1 and located through a seek table. Uniform blocks, including zero, use fill entries. Reading one block does not decode all RAM, although the current decoder still processes a complete block.

[File formats](disk-layout-and-schema.md) specifies directories, sizes, schemas and byte diagrams. The [SVG atlas](../../../zh/design/offload/assets/overview.svg) expands manifest, generation, raw RAM, staging and pin bytes. Shared SVG labels are currently Chinese; field descriptions are provided here in English.

## 3. Detailed data and mechanisms {#detailed-design}

### File identity and directories {#files-and-references}

```text
/data/
├── vm.ram                    compressed manifest, or ordinary raw RAM
└── vm.ram.layers/            compressed mode only, permissions 0700
    ├── .tmp<staging>         writable raw pages
    ├── <id>.pvdelta          immutable base or delta
    ├── <id>.pvpin            optional empty retention root
    └── .tmp<capture>         generation under construction
<user-cache>/pvisor/ram/
└── mount-<random>/ram        logical FUSE RAM, compressed mode only
/exports/
└── idle.ram                  optional hard link to the original inode
```

Explicit backing is created with mode 0600 and must not exist. Otherwise a temporary file is created under `dirs::cache_dir()/pvisor/ram`. Compressed layers use the explicit path plus `.layers`, or a temporary cache `layers-*` directory.

`offload(Some(path))` publishes a hard link. The host checks source dev/ino against its FD, rejects replacement and cross-filesystem destinations, then updates selected path. Runner FD and mmap retain their identity. A compressed alias links only the manifest: its descriptor still references the original sidecar.

Publishing temporary compressed backing keeps its original layers directory. The original temporary manifest name is deleted at exit while aliases remain. Run Bundle, OverlayFS upper and OCI cache are separate resources.

### Storage capture and restore contract {#storage-contract}

The host storage layer's `SnapshotChain::capture` takes a `RamLayout`, optional parent chain, dirty-block set and block reader. The dirty set must include every changed block, including vCPU and device writes; the reader must observe one stable epoch throughout capture. Storage does not pause a VM itself, and omitted dirty blocks inherit old content. The parent must belong to the same directory with an identical layout; a layout change requires a new base with `parent=None`. Base and compaction captures read every block, so supplying only dirty-block content is insufficient.

`SnapshotChain::restore_to` sets the destination file length, writes all RAM blocks at compact logical offsets and synchronizes the file. The caller must quiesce every destination accessor throughout restore. Failure can leave a resized or partially written destination; there is no transactional rollback. This restores RAM bytes only, without starting a VM or restoring CPU/device state. Direct storage supports multiple regions; the current FUSE adapter uses a logical file region beginning at 0, which cannot reconstruct the actual guest physical layout.

### Page staging and block reads

Staging is a sparse raw file at compact logical RAM offsets. Each 64 KiB block has a `u16` validity mask for its sixteen 4 KiB pages. A first partial-page write fills untouched bytes from parent; a complete-page overwrite writes directly.

Reads obtain committed block content and overlay valid staging pages. A fully staged block can skip ancestor reads. Initial unwritten RAM reads as zero. Masks exist only in memory, so staging alone cannot reconstruct an uncommitted epoch.

Staging pages, compression blocks and host pages are different granularities. The VMM aligns offsets to host pages, also used by mincore; Apple Silicon commonly uses 16 KiB. Anonymous device windows are excluded from RAM backing.

### Offload and resume {#lifecycle}

Startup creates backing and optional FUSE mount, installs initial guest lower exclusions and duplicates the RAM FD into the internal runner. The VMM sets file length and maps RAM. The host Connection owns backing; the runner owns VMM and control thread.

Ordinary pause stops vCPUs without closing the device gate or committing a generation. Stable RAM capture additionally requires draining device accesses and kernel writeback.

| Stage | Action |
|---|---|
| Path preparation | Host optionally publishes alias before sending the request |
| CPU quiescence | Runner takes transition lock and pauses vCPUs |
| Device quiescence | Release VMM mutex before closing/draining gate, allowing I/O workers to finish |
| Reclamation | macOS unmaps HVF RAM; synchronize and advise reclaim on host file mappings |
| Runner reply | Return Offloaded and residency samples; CPU/gate remain stopped/closed |
| Host completion | Synchronize logical FD, commit compressed store, wait for worker and validate outcome |
| Resume | Restore HVF mappings, open gate, resume vCPUs; faults read backing on demand |

macOS uses `msync(MS_SYNC | MS_INVALIDATE)` and `madvise(MADV_DONTNEED)`; Linux uses `MS_SYNC` plus `posix_fadvise(DONTNEED)`. Host MAP_SHARED addresses remain valid. vCPU transitions have a shared 3-second deadline; device drain has 5 seconds. Host exchange budgets are 300 seconds for offload and 10 for pause/resume.

The runner acknowledgement precedes host storage completion. Callers must wait for the full outcome. Control frames use a 4-byte big-endian length plus JSON, bounded to 16 KiB. Dropping the caller future does not abandon acknowledgement consumption and desynchronize the next request.

### State, events and memory reports {#control-state-and-memory}

Host `RunHandle` / `RunControlHandle` pause, resume and offload operations use Core `OperationKind` and return `Value::Vm`. Successful pause/offload sets the outer `RunState` to `Suspended`; successful resume sets it to `Running`. The result's `VmState::{Running,Paused,Offloaded}` distinguishes VM control states. `Suspended` alone does not prove RAM was offloaded and is separate from durable checkpoint/suspend workflows. The lower-level interface is `pvisor_vm::api::VmControl`, implemented by `VmmHandle`, with offload returning `RamReclaim`.

Control transitions are serialized and record `vm.control_requested`, followed by `vm.control_completed` or `vm.control_failed`. Failure to publish the request event prevents control execution. After successful control, Run state is updated before publishing completion. If that event write fails, the call returns `VM control completed but observation failed`; neither the completed control nor its state is rolled back. Callers must not interpret this error as proof that the VM did not execute the operation.

Core `VmMemory` fields have these meanings:

| Field | Meaning |
|---|---|
| `backing_file` | Host-selected absolute live backing path; the host replaces the runner's reported path with its own owned path |
| `backed_bytes` | File-backed RAM range covered by reclamation, including the separate kernel region and excluding DAX/GPU shared windows |
| `resident_before_bytes` | Pre-reclamation mincore residency sample in host-page bytes; null if unavailable |
| `resident_after_bytes` | Equivalent sample after runner reclamation; null if unavailable |

On Linux, mincore includes file page-cache residency and is not process RSS. Samples do not guarantee zero residency, and later accesses can fault pages back in; host compression also follows runner sampling.

### Publication and validation {#format-and-publication}

Manifest size is `44 + N + 80k`: magic (8), descriptor length (4), JSON (N), checksum (32), then k head records (80 each). A generation contains frames, seek table, metadata and a 64-byte footer.

The ID hashes original metadata JSON, which includes decoded block checksums. Readers check structure, ancestry and content. JSON IDs are 32 integer bytes; filenames encode them as 64 hex characters. [Format definitions](disk-layout-and-schema.md#detailed-design) includes every field and schema.

| Order | Action |
|---|---|
| 1 | Synchronize logical FD, draining dirty kernel pages into staging |
| 2 | Combine staging/parent and write temporary generation |
| 3 | fsync generation, publish without replacement, fsync layers directory |
| 4 | Append head and fsync manifest |
| 5 | Install chain, clear dirty masks, truncate staging |
| 6 | Resolve current/pinned ancestors and remove unreachable generations |

FUSE fsync only flushes writes; it does not publish head. Offload coordinates stable generation commit. GC failure after durable head warns without reversing success. Generation or head write failure poisons the store rather than promising a safe retry.

### Inheritance, compaction and GC {#worked-example}

```text
H1(base)  = A1 B1 C1
H2(delta) = B2, parent H1
H3(delta) = A2, parent H2
Current RAM = A2 B2 C1
```

The resolved index identifies each block's last overriding entry. Dirty blocks matching parent checksum can be omitted; a delta with no changes can reuse head.

At parent depth 8, the next capture builds a complete base with null parent. Compaction reads all logical RAM, decodes old blocks and re-encodes content while the VM is paused. Its cost approaches a full save even when recent changes are small.

GC retains current head and ancestry reachable from `.pvpin` roots. Old manifest records do not retain history. Unpinned old chains can be deleted after compaction. Standard offload neither pins automatically nor rebuilds a VM from a historical head.

The directory owner must serialize `SnapshotChain::pin/unpin/capture/collect_unreachable`; these operations provide no interprocess locks. GC requires exclusive directory ownership and prior publication/fsync of the current head. It resolves all pin roots and ancestors before deleting anything; an invalid pin name or missing/corrupt chain stops collection. Direct capture does not implicitly delete history, and an unpinned historical ID has no durable retention promise. Pins and collection do not impose a capacity quota.

### Reference designs and tradeoffs {#design-references}

- [Zstd Seekable](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md): independent frames and a trailing seek table; incompressible blocks also use Zstd frames, without a separate RAW payload.
- [QEMU mapped-ram](https://www.qemu.org/docs/master/devel/migration/mapped-ram.html): RAMBlock, stable logical positions and write bitmaps, without copying fixed physical disk positions.
- [QEMU fast snapshot load](https://www.qemu.org/docs/master/devel/migration/fast-snapshot-load.html): faults and background restore must coordinate page ownership; FUSE backing does not implement that pager.
- [Firecracker memory](https://github.com/firecracker-microvm/firecracker/blob/main/src/vmm/src/vstate/memory.rs): region layouts and the boundary requiring both vCPU and userspace writes in incremental tracking.
- [zram](https://www.kernel.org/doc/html/latest/admin-guide/blockdev/zram.html) / [zswap](https://www.kernel.org/doc/html/latest/admin-guide/mm/zswap.html): zero/uniform omission and compressed-object organization, without copying kernel allocators.

Compressed objects are packed contiguously and may cross disk pages. There is no storage-page allocation bitmap, mutable extent, hole punching or online defragmentation, and small objects do not each require an extra 4 KiB extent. PVZRAM v2 replaces the old mutable-block container; the current reader does not read v1 files.

### Exit and failures {#ownership-and-cleanup}

Normal exit waits for the child; cancellation/deadline terminates its process tree before detach. Backing cleanup attempts unmount and removes staging/mount directories. Unpublished temporary backing/layers are deleted; explicit files, aliases and kept layers persist.

Exit does not perform another generation commit. Writes after the last resume can be discarded with staging; the manifest retains the last committed head. Inspect through read-only `CompressedRam::open` only after the writer stops.

Remaining defects concern pathname visibility, state reporting and cleanup timing: new aliases do not continuously install guest visibility constraints; pause after Offloaded may report Paused while resources stay offloaded; the 300-second Tokio timeout does not stop blocking commit, leaving worker/unmount/delete synchronization unresolved. Open rejects torn manifest tails. The 64 GiB layout allowance is also not fully reconciled with the 64 MiB metadata budget.

### Source locations {#source-map}

| Source | Content |
|---|---|
| [control.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/executor/vm/control.rs) | Ownership, publication, exchange and host commit |
| [supported.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/executor/vm/supported.rs) | Startup, FDs, exclusions and runner control |
| [ram_file.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/ram_file.rs) | FUSE file and callbacks |
| [ram_backing.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/ram_backing.rs) | Manifest, staging, masks and commit |
| [image.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/ram_backing/image.rs) | Encoding, inheritance, pins, compaction and GC |
| [VMM RAM](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/vmm/ram.rs) | Mapping, sync, reclamation and sampling |
| [libkrun](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/handle.rs) / [memory gate](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/devices/virtio/memory_gate.rs) | CPU/device quiescence |

## 4. Experimental evidence {#experiments}

The following evidence comes from targeted tests and inspection on 2026-10-03, covering the `e5359307` product path and `1b97a9ef` documentation helper. It does not validate subsequently added, uncommitted cold-pool experiments.

| Coverage | Result | What was exercised |
|---|---:|---|
| Backing, protocol, RAM store and Core contracts | 31 passed | Random partial writes, repeated commit/compaction, corruption, timeout and acknowledgements |
| Device gate | 4 passed | Access ownership, drain/reopen and independent VMs |
| Host RAM mapping/reclamation | 2 passed | Shared data retained, device windows excluded, invalid backing rejected |
| Publication side effect after rejection | 1 passed | Reproduced an alias surviving transition failure |

These 38 checks establish some storage/control invariants, not end-to-end guest correctness. Six VM document cases hit EPERM during socket setup; direct SDK execution was blocked at AgentCtl initialization before guest entry. Logs are in `review_project/06-evidence/offload-20261003/`. Those attempts cannot be counted as successful VM tests.

An existing 256 MiB compressed RAM sample has a 999-byte manifest, a ~21.45 MiB base and a ~1.85 MiB delta. Both generations allocate 23.30 MiB, excluding manifest/directory overhead. Ten historical heads remain while GC retained only one base and delta. [Detailed measurements](disk-layout-and-schema.md#experiments) gives exact sizes and artifact location.

This demonstrates consistent format accounting and one workload's storage result. It does not measure physical memory savings, first-fault latency, compaction pause or multi-VM density. There is insufficient evidence to select level 1 or a fixed eight-layer threshold as optimal.

Residency fields sample file-backed mappings through mincore; backed_bytes measures covered range. Host compression follows runner sampling and can allocate/cache data, so these fields cannot substitute for RSS, physical footprint or post-offload system measurements.

## 5. Usage recommendations {#usage}

Consider offload for same-node VM retention across waiting periods. Pause is simpler when only a brief CPU stop is needed. Current files do not support shutdown restore, migration or final-exit state archiving.

Validate ordinary backing control/restore before evaluating compression. Compression requires Linux FUSE or macFUSE kernel backend. Keep backing/aliases in host-private storage outside guest views and on one filesystem; retain the referenced sidecar. Wait for operation success before using a published path.

Measure delta and compaction rounds separately: pause time, CPU, disk peak and restore latency. Compaction retains old chain while building a new base; pins retain further history. Large RAM, incompressible content and frequent rewrites especially need measurement. A small sample is insufficient for capacity planning.

To retain final RAM data, explicitly complete offload before ending the writer; exit does not commit automatically. Inspect without an active writer. Manage cleanup through whole-backing ownership rather than deleting individual parents.

### Experimental cold-pool boundary {#experimental-integration-boundary}

The uncommitted `PVISOR_EXPERIMENTAL_MEMORY_POOL` path uses a Unix socket and in-memory compressed object pool to restore cold blocks. It is separate from disk-backed whole-VM offload. Integration currently rejects coexistence with FUSE compression and rejects whole-VM offload with experimental preparation/pager enabled. Inventory JSON is diagnostic only. These experiments are outside the evidence and recommendations above. See [memory deduplication and cold-block compression](../memory-optimization/proof-of-concept.md) for their mechanisms and evidence.
