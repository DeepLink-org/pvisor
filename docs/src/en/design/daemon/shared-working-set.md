# Shared working sets and lazy loading

Reuse immutable contents by identity, and keep actual access and private modification costs explicit. Native pVisor caches and node resources provide mechanisms for this direction; the native VM daemon has **not** acquired those resources or validated their density benefits.

## Three distinct mechanisms {#principles}

1. **Content deduplication** reduces duplicate disk/object bytes, not automatically guest RAM.
2. **Shared resident pages** require appropriate backing identity and mappings. Native restore can use the same read-only RAM inode with `MAP_PRIVATE`; unchanged pages may share while writes remain private. An identical image name or separate identical files is not equivalent backing.
3. **Lazy loading** avoids reading/decoding untouched content but moves costs to faults and later tool access. Readiness alone cannot establish lower first-result or completion costs.

Anonymous-page scanning, cold-page compression and whole-VM offload remain separate strategies. Do not add their claimed gains together as if they were one mechanism.

## Existing native foundations {#current}

| Component | Mechanism | Boundary |
| --- | --- | --- |
| `pvisor/src/node.rs`, `node/registry.rs` | Same-user, same-host immutable owners; identity-based preparation, connection pins, bounded warming | Runtime protocols retained, CLI node supervisor removed; no daemon adapter or transparent live takeover |
| Native image cache | Validated demand reads, paged metadata and content reuse | Published FS/S3 objects have paged reads; an uncached ordinary OCI prepare can still fully prepare before returning |
| Native Linux RAM restore | Authorized sealed identity/compatibility and shared read-only inode, private COW guest mappings | Ordinary fresh boot bypasses RAM restore; compatible native profiles remain required |
| Snapshot lazy reader | Validate/decode blocks on fault with bounded decoded cache | First-access costs remain; legacy raw formats can require full validation |
| Historical macOS compressed pool | Session references and private cold-page restoration | Recorded-version mechanism; standalone launcher removed; not the current Linux protocol |
| Daemon-owned Linux pool | Resident duplicate 4 KiB pages shared through bounded memfd slots and private COW mappings | Explicit `serve --memory-pool`; separate budgets and failure scope, no unique-page compression |
| NativeRuntime daemon | Independent immutable rootfs, private VM writes | Optional daemon-owned pool; no automatic node socket acquisition, RAM restore or lazy snapshot integration |

Additional source areas: `image/cache/lazy.rs`, `image/cache/portable/binary.rs`, `executor/vm/restore_ram.rs`, `environment_snapshot/lazy.rs` and `pvisor-vm/src/memory.rs`. Native node acquisition validates authorized stores, publication and compatibility rather than accepting cache presence as authority.

## Working-set accounting {#model}

A native sharing cost model is:

```text
node memory = service overhead
            + shared resident working-set union
            + private dirty RAM and private filesystem state
            + per-execution runtime overhead
            + bounded caches and in-flight scratch
```

Count physically shared pages once. Shared costs vary with revisions and access patterns; private costs grow with executions and write ratios. Disjoint accesses and many writes can eliminate gains. File copy-up, transfer blocks, read-ahead and COW may amplify requested bytes.

Keep logical reservations, physical occupancy and reclaimable cache separate. Native node retained-payload accounting covers content blocks, paged metadata and decoded RAM, not complete metadata, scratch, external references or kernel pages. Kernel caps and whole-group observation remain necessary. None of this relaxes daemon [hard-limit admission](admission.md).

## Ownership and future connections {#integration}

Connection pins protect active immutable mounts/backing until native runners are reaped. Last release permits teardown or bounded warming. Preparation for the same identity is serialized without holding the global map lock across slow I/O. Refetchable decoded cache may be evicted; active owners or the only remaining copy of private cold RAM cannot be treated as cache.

Connecting the existing native runtime to node sharing would need explicit acquire/release, cancellation, compatible input handoff, cleanup, evidence and budget contracts. Removing the CLI node supervisor does not implement that adapter or migrate node protocols into the daemon. Generic warm-template Agent restoration is not already available: source command/input/environment/policy bindings and native no-network restore constraints still apply.

Bounded critical-page prefetch, coalescing additional identical misses and extending complete metadata/scratch accounting remain possible work. There is no daemon host-affinity policy or placement hint protocol; external orchestration owns host choice.

## Evidence questions {#experiments}

No new measurements or PASS claims are available. Separate native-path experiments from daemon node-sharing integration evaluation:

| Question | Required controls and observations |
| --- | --- |
| Shared baseline | Same sealed bytes and accessed working set; shared/independent backing, eager/lazy and write ratio controlled separately; group physical memory, native PSS and COW plus write isolation |
| Lazy environment | Same published revision and verified outputs, fixed accessed set as untouched bytes grow; origin reads/decodes before first result and full completion |
| Concurrent misses | Same versus distinct objects; requests/decodes per unique object, scratch peaks and blocked latency, distinguishing retries/read-ahead |
| Prefetch and warmth | Fixed total resources and arrivals; first result, correct completion throughput, CPU and memory-time, tails and amortized preparation costs |
| Failure preservation | Publication authorization, pin/owner release, GC, slow/failing reads and owner/pool loss without claiming unimplemented reconnect |

Use at most four simultaneous real native guests, counting live source VMs; cap each guest and service. Do not flush global page cache or infer sharing from untouched-page savings. Freeze workloads, sample counts and meaningful thresholds before collection. Historical independent fresh-boot probes neither establish nor refute native sharing, and do not establish daemon density.

The daemon pool is separate from node-backed restoration. Pool loss fails dependent VMs; API restart retains the detached component but cannot recover failed pool contents. See [activation, ownership and limits](../../guides/daemon/index.md#memory-pool).

Related designs: [responsibility convergence](responsibility-convergence.md), [shared image storage](../shared-image-cache-storage.md), [memory optimization](../memory-optimization/index.md) and [experimental cold RAM pool](../memory-optimization/proof-of-concept.md).
