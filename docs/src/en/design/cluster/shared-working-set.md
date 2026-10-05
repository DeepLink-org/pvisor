# Shared working sets and lazy loading

The resource direction is: **pay once for immutable environments and shareable baselines, and pay incremental costs for actual access and private modification**. Node ownership and retained-payload budgets are now connected as described in [service consolidation](server-consolidation.md); the text distinguishes implemented mechanisms, future extensions and outstanding performance acceptance.

## Distinguish three benefits {#principles}

1. **Content deduplication** reduces duplicate disk/object-storage bytes without automatically reducing guest RAM.
2. **Shared resident pages** need appropriate backing identity and mappings. Booting from one image does not automatically share guest anonymous RAM. Restoring from the same read-only RAM inode through `MAP_PRIVATE` provides a direct path for sharing unchanged baselines.
3. **Lazy loading** avoids reading/decoding unused data in the current phase, deferring costs to later access. It may improve readiness while adding faults/I/O to the first tool operation. Measure the first useful result and full task completion together.

Prioritize immutable sharing with known identity and ownership. Arbitrary anonymous-page scanning, cold-page compression and whole-VM offload are separate strategies; their results cannot be combined into one benefit.

## Existing implementation foundations {#current}

| Path | Existing mechanism | Current boundary |
|---|---|---|
| Immutable environment lower | A configured node socket shares same-identity mounts across Workers, with bounded connection pins/strong warming and task-private uppers | Without Node, Worker-local `Arc<MountedImage>` reuse remains; live takeover after node failure is unsupported |
| Image lazy cache | Demand reads, local 64 MiB/4096-block caps, 64 KiB metadata pages/256-page LRU and cross-image content CAS | Node aggregates retained blocks, metadata pages and decoded RAM; complete metadata, scratch, external Arcs and kernel pages are excluded; disk reclamation needs a separate policy |
| Linux native restore | Node identity uses sealed ID/compatibility to share one read-only RAM inode across Workers and authorized stores; guest mappings use `MAP_PRIVATE` COW | Without Node, reuse remains supervisor-local; ordinary boot bypasses RAM restore, and compatibility/no-network profile requirements remain |
| Snapshot RAM lazy reader | Faults validate/decode blocks on demand; a small cache retains four decoded blocks, with kernel page cache providing primary decoded reuse | First-access costs need measurement; legacy raw formats without block indexes can still require full validation |
| Cache affinity | Controller ranks a bounded candidate window by `cache_keys` and environment-layer matches | Reports currently originate from static `--cache-key`, rather than measured residency, complete block hit rates or global shortest-readiness placement |
| Cold RAM compression pool | A separate experimental mechanism for deduplication and cold-page restoration | `vm.memory_pool` explicitly requires macOS/Apple Silicon; this is not evidence of a background cold-page pool in Linux Cluster |

Source locations: `bin/worker/environment.rs`, `image/cache/lazy.rs`, `image/cache/portable/binary.rs`, `executor/vm/restore_ram.rs`, `environment_snapshot/lazy.rs`, `pvisor-vm/src/memory.rs` and Controller `scheduler.rs`.

Published FS/S3 native-cache objects support paged on-demand reads. An ordinary OCI cache server still fully prepares an uncached image before the initial `prepare` returns. Report these cold paths separately. Control objects such as `checksums.bin` are still read upfront; paging does not mean reading no metadata for unaccessed content.

## Target cost model {#model}

For tasks using a common template, physical memory should approach:

```text
M(N) = base services
     + shared resident working-set union
     + sum(private dirty RAM + private upper + per-VM runtime overhead)
     + bounded decoded caches and in-flight I/O
```

Count shared resident pages/objects once; private costs grow with N. Shared costs also change with working set, environment versions and access patterns, rather than staying universally constant. High write ratios and disjoint accesses reduce gains.

The read-cost target is necessary control objects plus accessed metadata pages/data blocks plus explicitly bounded prefetch, rather than scanning/fetching the entire environment per task. Record read, decode and COW amplification: large transfer blocks, read-ahead and copy-up may fetch more than requested bytes.

Keep logical RAM reservations, physical occupancy and reclaimable caches separate. Lower PSS/RSS cannot directly justify optimistic admission-budget reductions. Validate peaks, private writes and concurrent misses before changing capacity policy.

## Engineering connections to complete {#integration}

**Validate connected cross-Worker node ownership.** Tasks with the same environment share a read-only mount; compatible restorations share a read-only RAM owner. Environment identity includes handle/digest and RAM identity includes sealed ID/compatibility. Each acquire validates authorized stores/publication, then connection pins protect active objects and bounded strong references keep them warm. Release follows normal teardown; GC retains its existing publication-root/pin contract, without writable shared mappings. See the [unified service guide](../../guides/cluster/service.md) for deployment.

**Extend payload-budget coverage and observation.** Node already aggregates retained cache payload, owner count and preparation concurrency; delegated cgroups cap complete process memory. Account for metadata, content blocks, RAM decoded caches, kernel residency, scratch and in-flight I/O. Bound controllable caches, mount counts and transient work; manage kernel page cache through host caps and observation without claiming precise userspace control. Warm mounts need byte/count/TTL or eviction policies instead of retaining another cache per task.

**Reuse object identity and coalesce identical misses.** Measure repeated downloads/decodes of the same valid object, distinguishing existing file-lock reuse, per-mount hot caching and cross-revision sharing. New single-flight needs keys, cancellation, retry, budgets and ownership. Corrupt bytes remain rejected, and cache cannot resurrect revoked publication references.

**Prefetch a small startup-critical set.** Use a frozen trace to prefetch required index pages, executable/library data or restored RAM pages, with byte and in-flight caps; load the rest on demand. Compare against pure lazy and complete materialization using useful-result and completion latency, without interpreting readiness alone as a gain.

**Make affinity reflect actual observations.** Proposed hints distinguish published, mounted, local-block and resident states with freshness, alongside queueing and resources. Current behavior is Worker polling plus candidate sorting, not global predictive placement; extensions are future work. Hints converge through in-memory views and reconciliation, without durable logging per cache hit.

Generic warm-template Agent startup is not already available. Restore preserves source command/input/environment/policy contracts; arbitrary task definitions cannot be substituted. Dynamic work needs controlled bootstrap/input handoff. Current no-network native restore does not provide complete network-model Agent restoration. Validate existing immutable-environment lazy paths and compatible forks first; accept extensions separately.

## Five questions to freeze before measurement {#experiments}

These are draft experiments, with no new test results. All retain at most four real sandboxes, per-guest RAM/vCPU/CPU-time bounds and host kernel caps. Run A/B sequentially without flushing global page cache and disrupting other tasks.

| ID / question | Controls and variables | Primary metrics and rejection conditions |
|---|---|---|
| S1: Does a common baseline reduce marginal physical RAM per fork? | Identical sealed bytes/tasks, 1/2/4 branches; shared/independent backing and eager/lazy controlled separately; fixed nonzero working set and varied private-write ratios | Whole-group task/owner memory, native RAM PSS/shared-clean/private-dirty, COW bytes, first result/completion; failed write isolation is failure, and untouched-page savings alone are not sharing gains |
| S2: Does a small working set load on demand from a large environment? | Eager/lazy on the same published revision; increase untouched files/bytes while fixing access/output; distinguish application-cache cold, disk warm and same-mount warm | Reads/GETs/ranges before readiness, first result and completion; metadata/content RAM and amplification; deferred costs or slower full completion do not establish overall gains |
| S3: Do concurrent identical misses duplicate reads/decodes? | Up to four synchronized tasks accessing the same versus distinct objects, with matched work/budgets/backend | Origin requests/decodes per unique object, transient peaks and blocked-task latency; separate read-ahead/retry from duplication rather than relying on hit counts |
| S4: Can bounded critical-page prefetch reduce cold-path tails? | Pure lazy, bounded trace prefetch and complete materialization; identical revision/output/budget | First useful result/completion, total reads/decodes and memory peaks; unused prefetch or miss-queue interference can refute the policy |
| S5: Do warm ownership and affinity improve fixed-budget useful throughput? | Identical arrivals/node budgets; current policy versus bounded warm retention/fresh hints; shared and distinct environments | Correct completion throughput, CPU and memory-time/task, queueing/tails; bigger caches, queue concentration or starving cold tenants can offset gains |

S1 ablations such as eager/shared do not currently have complete public switches. Build a controlled harness retaining equal validation and ownership first; different rootfs/output/compatibility profiles are not toggles. Seal and stop the source VM before starting up to four branches; if the source remains live, allow only three branches. **The source counts toward the four-guest cap.**

Use private endpoints/prefixes/cache directories with counters for cold-cache tests. Uncontrolled host page cache is unknown/warm rather than physical-disk cold. Record publication and template-sealing costs separately and amortize over actual reuse. S1 fixes shared content and access volume to separate zero-page/compression effects; S2 fixes bytes and verified outputs rather than reducing work to manufacture lazy gains.

Acceptance includes block/page validation, private COW writes, pinned versions, owner/pin release and GC correctness, and lease responsiveness under slow/failed reads. Include cache/pager services instead of summing VM RSS alone. Freeze workloads, samples, meaningful gains and acceptable latency costs before formal collection. No PASS or speedup forecast is available yet.

## Curves to plot {#plots}

- **S1: group physical memory versus branch count**, with fixed working sets/write ratios and separate shared/independent and eager/lazy arms. Plot private COW pages against write ratio to identify the gain's source.
- **S2: bytes and latency before the first useful result versus total environment size**, with a fixed accessed set, alongside full-task values. Separate application-cold, disk-warm and same-mount-warm cases to identify deferred reads.
- **S3/S4: blocked latency and transient peaks versus concurrent misses / prefetch budgets**, keeping task inputs and output checks matched and reporting actual reads, work and amplification.
- **S5: correct completion throughput, CPU seconds/task and memory-time/task versus reuse count**, with fixed total budgets and amortized publication/template-sealing costs.

Report steady-state gains, initial-use costs and reuse counts needed to break even. Never reaching break-even is a valid counterexample. Unsupported safe controls or unmatched conditions mean missing, unmeasured points instead of filling curves with other configurations.

## Priority {#priority}

Start with **S1 existing shared RAM/owner reuse** and **S2 existing environment lazy loading** to establish benefits from available mechanisms. Use S3 to explain concurrent bottlenecks before deciding whether node warming, prefetch and dynamic hints warrant implementation. Finally validate combined gains in [fixed-budget useful-work experiment Q3](../cluster-benchmark-plan.md#q3).

Earlier minimal-directory-rootfs, fresh-boot, independent-Worker probes specify neither immutable environments nor restore references, bypassing S1/S2's core paths. Retain them as base-cost controls; they neither establish nor refute sharing/lazy benefits.

Related contracts: [image cache](../../reference/shared-image-cache.md), [shared image storage](../shared-image-cache-storage.md), [cold RAM pool](../memory-sharing/index.md), [lifecycle](lifecycle.md) and [admission](scheduling.md). Their support boundaries remain separate.

See [service consolidation](server-consolidation.md) for unified node owners, budgets and deployment, distinguishing refetchable caches from indispensable active RAM and preserving independent Controller restart boundaries.
