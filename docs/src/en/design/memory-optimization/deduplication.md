# Memory deduplication

Share identical content across pVisor instances while preserving independent writes and exits. Deduplication removes duplicate copies; it neither compresses unique content nor saves complete machine state.

## Architectural goal {#architecture}

Share known-identical immutable RAM baselines first, then let Linux KSM discover duplicate private pages that arise during execution. Instances own checkpoint saving, compression, and restoration; sharing does not take ownership of the only copy of state. See [Memory optimization](index.md) for common boundaries.

| Approach | Shared content | Main tradeoff |
| --- | --- | --- |
| Immutable baseline + private COW | Unmodified pages restored from a common baseline | No scanning needed, but writes reduce sharing |
| Linux KSM | Identical private anonymous pages found by the kernel | No sharing service, but asynchronous scans consume CPU |
| Linux physical pool | Equal resident raw pages in different VMs | Scanning, slots/references, kernel COW and a separate process failure domain |
| Encoded-pool deduplication | Equal encoded cold objects | May combine with compression, requiring reclamation and restoration |

Encoded-pool deduplication is defined by [Pooled-server compression](compression-pool.md); the Linux physical pool shares raw resident pages and has a separate ownership contract.

## Immutable baseline sharing {#baseline}

Instances map corresponding ranges of the same backing object with `MAP_PRIVATE` for write isolation. Unmodified file pages can be shared; host COW makes writes private rather than directly connecting writable RAM between VMs.

Materialize compressed checkpoints into mappable immutable baselines, then reuse those baselines. Files with identical content but different inodes do not automatically share cache pages, and disk reflinks are not memory sharing. The baseline cache is a regenerable accelerator, not a replacement for persistent checkpoints.

Content validation and immutability are prerequisites for reuse. Instances hold their own backing references and leases; an instance exit or cache eviction must not invalidate another instance's mappings. Kernel-object/storage references should keep shared content alive rather than another instance's heap.

## Linux daemon physical pool {#physical-pool}

![Two VMs read the same slot; writes create private COW pages](../assets/memory-cow.svg)

Explicit `serve --memory-pool` enables raw-page sharing in the daemon-owned pool. Candidates retain hashes only; content enters a bounded memfd slot after appearing in different VM sessions. With CPUs/devices quiesced, the VM rechecks bytes and maps equal pages using a read-only descriptor and `MAP_PRIVATE`. This path needs no userfaultfd and does not encode or compress unique pages.

References pin slots until mappings are removed; disconnects retain them until pidfd confirms peer exit. The pool owns live shared pages, so its loss fails dependent VMs. It does not supply durable snapshots or process-restart recovery. See [Shared working sets](../daemon/shared-working-set.md) for budgets and failure scope.

## Linux KSM {#ksm}

KSM accepts `MADV_MERGEABLE` advice for suitable private anonymous RAM, with the kernel scanning, merging, and handling COW in the background. It requires neither discovery between pVisor instances nor a userspace service holding shared pages.

Successful registration is not evidence that pages have been scanned or merged, and provides no synchronous completion deadline. Instances register suitable regions only; an administrator or host coordinator owns global scanning budgets so instances do not overwrite each other's global settings.

KSM does not merge file page-cache pages. Adding advice to the current `MAP_SHARED` live backing does not enable deduplication. Anonymous COW pages from private mappings can be candidates, with eligibility and benefits validated on the actual kernel. macOS has no standard Linux KSM capability; UKSM is an algorithmic reference, not a deployment dependency.

## Key tradeoffs {#tradeoffs}

- **Explicit sharing versus background discovery:** Common baselines need no rediscovery; KSM handles dynamic duplicates at the cost of scanning delay and CPU.
- **Savings versus write costs:** Frequent writes break sharing, reduce benefits, and add COW costs. Initial sharing ratios alone cannot determine capacity.
- **Sharing versus isolation:** Restrict trust domains and assess content-presence side channels. KSM advice has no pVisor-defined domain parameter, so product labels alone cannot establish isolation.
- **Simplicity versus coverage:** The current physical pool uses bounded resident scanning. Arbitrary instant hot-page merging and P2P remain outside its scope; active shared slots and durable machine recovery are designed separately.

The first version does not combine KSM and userspace cold reclamation on the same region, avoiding redundant merging followed by copying and compression. See [Memory compression](compression.md) for unique but compressible cold content.

## Current integration and evidence {#direction}

`[vm].ram_dedup` is a default-off boolean; `--vm-ram-dedup` enables it and selects the VM executor. The runner explicitly calls `handle.advise_ram_dedup()` at VM startup and writes the best-effort installation report to stderr. Advice failures do not stop the VM. See the [CLI](../../reference/cli.md#vm-ram-dedup) and [configuration reference](../../reference/config.md#all-fields).

On Linux, ordinary private anonymous RAM and private COW mappings restored from snapshots are eligible for `MADV_MERGEABLE`. Live `MAP_SHARED` RAM is skipped, not converted to a private or anonymous mapping. Device windows, huge-page, non-writable or unaligned mappings are excluded; active cold paging or device preparation also blocks advice. On macOS, advice for otherwise eligible mappings reports unsupported. The report records per-mapping accepted, skipped, unsupported or error status; `accepted_bytes` counts only the ranges for which advice was accepted, not merged bytes, memory savings or proof that the KSM scanner is enabled. pVisor changes no global KSM settings and starts no new service.

`ram_dedup` conflicts with `vm.memory_pool`, `vm.ram_compression`,
`vm.cold_ram_compression` ([Linux local live pager](compression-local.md)) and the legacy `PVISOR_EXPERIMENTAL_MEMORY_POOL` opt-in. Advice preserves snapshot baselines, private writes and independently held backing references; it takes no ownership of the only state copy. Cross-workload content-sharing risks require explicit opt-in. Unavailable advice leaves the existing mappings intact, with no fixed saving ratio or scan deadline.

Current unit tests cover default-off configuration and round trips, CLI executor selection, conflict rejection, eligibility and partial reports, unchanged bytes and addresses, and restored COW write isolation and backing lifetime. They allow real advice to be unavailable and do not require a running scanner; they are not production acceptance or evidence of measured merging, savings or density.

The [proof of concept](proof-of-concept.md#correctness-evidence) preserves probes for shared baselines, independent writes, and reference lifetimes. The [cold pool](proof-of-concept.md#ownership) demonstrates encoded-object sharing, not active-page sharing. Existing mappings and experiments do not establish production acceptance for new backends.

Benefit comparisons must include COW, scan CPU, peaks, and application latency. Summed RSS is not unique physical usage. Linux KSM capabilities and statistics are governed by the [actual kernel interface](https://docs.kernel.org/admin-guide/mm/ksm.html).
