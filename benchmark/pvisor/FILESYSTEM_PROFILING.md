# Filesystem profiling and small-file optimization

`PVISOR_FS_PROFILE=1` enables opt-in aggregate diagnostics in OverlayCore, the
host FUSE adapter, the compact preimage log, and the VM virtio-fs
adapter/protocol server. Disabled profiles do not acquire
profiling locks, allocate per operation, or read per-operation clocks. The
profiler records no paths or contents. Enabling profiling changes execution
cost; use separate unprofiled runs for performance acceptance.

```sh
PVISOR_FS_PROFILE=1 pvisor run --vm --rootfs /path/to/rootfs -- rg needle .
```

Records on stderr start with `pvisor-fs-profile ` followed by JSON. Use
`pid`, `component`, and `instance` together to identify an independent profile.
Each record is a cumulative snapshot, not a delta; do not add consecutive
records. Timed stages are inclusive, so do not sum parent and child times.
Schema 2 retains the earlier counters and adds `max_ns` (largest completed
inclusive span) and `latency_buckets` (counts in <=1 ms, <=10 ms, <=100 ms,
<=1 s, >1 s buckets, in this order). Schema-1 artifacts have neither field;
do not treat missing fields as zero. Buckets are cumulative counts, not
percentile estimates. A maximum is wall time, not CPU time or proof of an
I/O cause. Diagnostic logging itself can perturb parent spans; keep it out
of timing comparisons.
`measurements[label].calls` and `total_ns` describe timed operations;
`units` describes a separately named work counter, such as `resolve_components`,
`fingerprint_bytes`, `journal_publications`, or `copy_up_bytes`.
`layer_parent_stats` counts parent-observation attempts inside the layer-path
helper; fused queries can provide type and identity together. `layer_leaf_stats`
counts leaf metadata calls in that helper. They exclude whiteout/opaque probes, other Core paths,
native-adapter syscalls, and xattr work: they are not a syscall census.
`mount_identity_attempts` counts successful directory observations for which a
backing-lookup caller requested Linux mount context. The fused-parent experiment
also counts `parent_identity_statx_calls` and
`parent_identity_metadata_fallbacks`: one statx can supply both the type and
identity, while unsupported/restricted queries fall back without mount reuse.

Journal spans separate lock acquisition (`journal_lock_wait`), destination
lookup, serialization, temporary-file creation, writing, atomic publication,
loser cleanup, and fsync. `journal_publications` counts attempts, not successful
first observations. `journal_rename_attempts` and `journal_link_attempts`
distinguish the selected publication route (a rename attempt may fall back).
Native no-replace rename consumes the temporary file on
success; `journal_cleanup` then only appears for losing publications. The
link/unlink fallback includes unlink in `journal_publish`. Lock wait is not
lock hold time; `preimage` includes the whole transaction.

When enabled, active profiles emit at most one periodic record per 250 ms.
Filesystem snapshot capture emits a checkpoint. Normal destruction emits
`final_record=true`; periodic/capture records have `final_record=false`.
VMM `_exit` or forced termination may omit the final record: a periodic record
is evidence of work up to that point, not proof of a complete run. A short
run may have no periodic record. `whiteout_probe` and `opaque_probe` isolate Core namespace-marker checks; opaque includes the marker-file lookup and opaque xattr probes, not only xattr CPU time.

Core and protocol spans measure inclusive service time. Pool diagnostics add
`pool_queue_wait` (submission to worker), `pool_service` (worker execution), and
`pool_completion_wait` (completed worker to used-ring publication). They do not
measure time before the host accepts a guest request. Adapter
`operation_read_lock_wait` and `operation_write_lock_wait` measure acquisition,
not lock hold time. Disabled diagnostics read no timestamps.

Protocol labels distinguish metadata, xattr, rename/unlink, flush, and directory
release requests as well as reads and writes. In particular, do not attribute
`OTHER` to flushing: inspect a profile from a binary that labels `RENAME`,
`GETXATTR`, and `FLUSH` separately. The private rootfs and reviewed workspace
have independent Core/adapter/protocol instances. Inspect both views; a
workspace-only profile can miss most of a package manager's filesystem work.

## Two-stage optimization campaign (2026-10-05)

The campaign keeps the original seven-workload fixture and the existing
`filesystem_ab.py` correctness gates. The frozen reference environment is
`target/reference-env-final-20261004`; its write workload is **256 x 64 KiB**,
not the current product-v1 60 KiB fixture. Host cache is warm, each job gets a
fresh workspace/upper, all seven tools run sequentially per environment, and
VMs use 2 vCPU / 16 GiB on physical host cores `0,1`. This permits workload
comparison with the published P0 report, while historical batch differences
remain descriptive rather than causal.

Stage A is screening: use prebuilt release adapter microbenchmarks (2 warmups,
8 samples, one separate diagnostic), then real FUSE/VM A/B with **1 warmup and
3 measurements per cell**. Preflight remains mandatory. Each comparison pins
one mechanism or an explicitly labeled combination; combination results cannot
attribute costs to one constituent. All five cells are shuffled each round.
Screening can reject a costly idea, but cannot establish a tail or
whole-task performance claim. Profile runs are separate and use no acceptance
timings. Do not compile or run tests concurrently with a timing batch.

The `bc08f457` source archive now defaults staged jobs to `rootless_process`,
whereas the published P0 artifact used `host_process`. Declare both expected
isolation types explicitly with `--baseline-staged-isolation` and
`--candidate-staged-isolation`. These flags only validate observed behavior;
they do not relax or reconfigure isolation. A declared rootless stage also
requires the read/write/non-bypassable kernel-boundary flags. The first P1
preflight stopped on this difference with zero measured samples; retain it.
Same-source A/B uses rootless for both sides. Historical artifact comparisons
keep the differing boundaries labeled, especially for host completion time.

Stage B is acceptance: retain the winning implementation, verify correctness,
and run real native/FUSE/VM A/B with **3 warmups and 30 measurements per cell**.
Report P50/P95/P99, absolute overhead versus same-batch native, each of the seven
tools, and complete launch-to-exit time. Retain every successful sample and
failure. Compare the candidate to the published pinned P0 candidate, and also
to a same-source baseline to separate incremental benefit from other changes.
Do not pool earlier percentiles. Repeat noisy or ambiguous whole-task results
in a second new directory rather than relabeling a screening batch as accepted.

| Hypothesis | Stage A test | Semantic gate / current state |
|---|---|---|
| One parent `statx` returns type/inode/device/mount context | Shallow/deep adapter + real A/B; count fused queries/fallbacks | Recheck physical ancestors, reject parent symlinks, preserve errors; unsupported fields/syscalls disable directory reuse |
| READDIRPLUS without the AUTO heuristic reduces follow-up attributes | Adapter directory case + real metadata/git/rg | READDIRPLUS already negotiated; keep inode lookup/refcount semantics |
| Parallel metadata dispatch avoids an inline queue bottleneck | Real A/B plus multiclient load; worker-count controls | Snapshot/destroy must drain accepted work, handles stay alive, mutation ordering preserved |
| Longer attribute/entry/negative/directory caching saves repeated requests | Repeated-pass diagnostic on immutable trees | A live lower permits external changes; no global TTL increase or absence cache without an ownership/invalidations contract |
| Writeback coalesces small writes | Real write/npm and repeated partial-write diagnostic | First preimage durable before backing mutation; dirty guest pages drained before terminal/export/snapshot; O_APPEND/truncate/read-after-write retained |
| Guest-local tmpfs/kernel filesystem gives a transport-cost control | Same-guest local-versus-virtiofs diagnostic | Diagnostic only; cannot substitute an unrecorded local write path for the accepted pVisor staging contract |

The baseline scheduler sends directory enumeration and reads >=64 KiB to the
pool; metadata and small reads run inline. One screening variant also
permitted LOOKUP, GETATTR and read-only OPEN when multiple requests were outstanding;
its patch and measurements are retained separately. Overlay operations retain a
shared/exclusive operation lock, with exclusive mutation sections. Increasing
worker count alone may add thread handoff without exposing useful parallelism.
`Server::init` already includes ASYNC_READ and PARALLEL_DIROPS in its default
supported capabilities; inspecting only the adapter's `init` misses these.
Measure actual opcode counts, queue wait, service time and lock wait separately;
inclusive Core spans are not a complete scheduling profile.

Every trial must pin binary/firmware/harness hashes and source/build provenance.
Archive the source outside a parent Git worktree before applying a patch (or
use explicit file copies and verify the exact diff); `git -C` in a nested
archive can silently skip paths. Write changed source files with fresh mtimes:
preserving an older source timestamp can let Cargo reuse a stale artifact even
when bytes changed. Wait for each build process to exit before copying its binary. Keep rejected experimental source patches and results in
the campaign evidence, while production defaults contain only validated changes.

`report.source_commit` identifies the worktree launching the harness, not the
binary's source. Frozen build source, patch and per-file hashes live under
`build_provenance`; preserve both records when the live worktree advances.

## Adapter benchmark

This manually selected test exercises 2,048 files in 32 directories through
the actual VM overlay adapter and platform passthrough code. It does not
boot a VM, mount FUSE, or use guest/kernel caches. Fixture setup is outside
the timer. Each case has two warmups, eight unprofiled samples, and one
additional profiled sample. Every operation verifies the fixture result.

```sh
cargo nextest run --release --locked -p pvisor-vm \
  --run-ignored ignored-only --no-capture \
  -E 'test(small_file_adapter_benchmark)'
```

Parse `PVISOR_FS_BENCH ` JSON lines. Exclude `warmup=true` and
`profiled=true` from timing distributions; use the final profiled sample
only to compare work counts. `lookup_getattr` traverses and stats every
file; `directory_plus` opens each directory and enumerates READDIRPLUS.
`lookup_open_getattr` looks up each file, opens it read-only, verifies attributes
through that handle, then releases it; it does not read its contents and has no
preimage journal. Compare Core `resolve` and native `backing_lookup` counts as
well as time; product search workloads validate actual content reads separately.
`deep_lookup_open_getattr` repeats this with eight directory levels before the
file, including guest-style lookup of the seven added parent names. It exposes
repeated Core ancestor checks, rather than treating shallow paths as a proxy
for dependency trees. Keep depth cases and absolute times separate from the
paired whole-VM acceptance protocol.
Each sample starts with fresh adapter inode tables and a private upper.

This is a mechanism benchmark. Validate product improvement separately with
the existing paired `macos_migration.py` harness or Linux reference workloads.
Keep executable hashes, firmware, filesystem/ownership mode, host/guest cache
state, raw samples, failures, and correct output. A whole-VM completion time
must not be compared directly with this adapter time.

## Kernel and concurrency diagnostics

`filesystem_kernel_probe.py` complements the unchanged seven-tool acceptance
fixture. It checks repeated traversal immediately and after 1.2-second TTL
expiry, disjoint file partitions with one/four Python threads, and 64 files
written using eight unbuffered 1 KiB writes each. It reproduces the exact tree
sizes on verified `/dev/shm` tmpfs inside the same environment. Tmpfs excludes
virtio-fs/Core/journal work and storage latency; it is an architectural control,
not a replacement workspace or a measurement of transport alone. Setup is
outside timers; each output checks all file counts/sizes. Staged partial writes
must be present in upper and absent from lower. All Run Bundle isolation gates
remain required. These probes are diagnostics, not extra acceptance samples.

```sh
python3 benchmark/pvisor/filesystem_kernel_probe.py \
  --assets target/reference-env-final-20261004 \
  --firmware target/p0-filesystem-artifacts-20261005/firmware \
  --binary baseline=/absolute/path/to/pvisor-before \
  --binary candidate=/absolute/path/to/pvisor-after \
  --output target/kernel-probe-new
```

Pass `--profile` only in a separate output directory. Keep the first probe
failure, any corrected harness and both hashes; do not discard failed controls.

The initial fused-query screen fell back after ordinary ENOENT/ENOTDIR too.
Missing upper parents then consumed an extra metadata query, cancelling saved
lower work. The final implementation returns those namespace errors directly,
while unsupported syscall/fields and permission restrictions still use metadata
with no mount reuse. The adapter diagnostic verifies zero fallbacks on its
normal fixture; tests cover unsupported queries, symlinks and directory changes.

READDIRPLUS without AUTO reduced traversal in screening but substantially
increased Git time; it is archived rather than enabled globally. Broad metadata
pool dispatch likewise remains an archived experiment. Production retains the
original dispatch classification and AUTO negotiation, alongside the shared
read-only OPEN lock and diagnostic queue stages. No new writeback, DAX,
passthrough or unbounded TTL policy is enabled by these changes.

## Preserved contracts

`metadata_resolved` returns the checked layer, path, and metadata from one
request. It is not a cross-request cache or permission capability. Merged
directory entries retain visibility, whiteout, opaque, and alias checks;
attribute-bearing enumeration still authorizes ask-protected children.
Adapters reuse metadata and the last passthrough lookup's Entry, but do not
cache live host attributes indefinitely. Existing materialized directory
snapshots remain readable; new name/type directory cookies are described below.
Device freeze/restore contracts are preserved.

The VM adapter retains at most 256 native parent-directory lookup references.
`metadata_for_backing_lookup` supplies request-local physical identities from
the selected layer; these are checked before reusing a reference. The final
component is always looked up afresh. The cache does not retain attributes or
authorization decisions. Linux identities include mount ID to distinguish
bind/idmapped mount contexts; unavailable mount IDs disable this optimization.
Snapshot capture/restore and filesystem destruction release cached references.
`directory_cache_hits`, `directory_cache_misses`,
`directory_cache_invalidations`, and `directory_cache_evictions` count this
work; compare them with `backing_lookup`, not just elapsed time.

With a live baseline and a preimage journal, a first content observation hashes
the complete baseline file using SHA-256. Subsequent observations reuse the
published entry while it exists. Frozen-baseline reads skip this step, but a
mutation still needs an initial preimage. Positive metadata lookup, getattr,
and directory enumeration do not hash file contents. Apply re-fingerprints
files for conflict detection/comparison. Path hashes used as journal filenames
are separate from content hashing; there is no incremental content hash.
`fingerprint` includes path/metadata/xattr work, file I/O, digest calculation,
and encoding; it is not a measure of the hash algorithm's CPU time alone.

`prepare_file_read` combines no-follow checks with read observation and backing
resolution. No-journal/frozen reads resolve once. Live-journal reads check before
capturing the baseline and again after publication, so returned backing is not
an observation retained across that potentially long transaction. The VM
variant returns parent identities from the final check, enabling directory
reference reuse on open. The host adapter uses the same contract without
collecting parent identities. Hash/journal format and write-open semantics are
unchanged; a failed native open releases its owned lookup reference.

Preimages retain the per-path JSON format. Positive first-content observations
use macOS `RENAME_EXCL` or Linux `RENAME_NOREPLACE`, falling back to hard
link/unlink when unsupported. Negative observations and direct mutation
preimages retain link/unlink: paired repair/diff trials regressed when those
also used rename. All routes publish complete contents without overwriting
the first observation. A losing writer verifies and adopts the winner;
mutation still syncs the winning file and its directory before changing upper.
Read-only observations continue to survive normal stage copy/reopen without
promising power-loss durability until promoted by a mutation.

For whole-VM paired runs, place `macos_migration.py --output` outside the
checkout (e.g. in `/private/tmp` on the same volume). Generating thousands of
files under `target/` can still trigger editor watchers despite Git ignores.
`--filesystem-profile` explicitly enables aggregate filesystem checkpoints and
records the option in protocol metadata. Use it only for separate diagnostic
runs, not uninstrumented performance acceptance; inherited profiling variables
are otherwise disabled by the harness.
Retain all samples, including batches that fail the regression threshold;
do not pool percentiles across batches. `--seed` permits an independent order;
`--diagnostic-timing` retains CLI startup/persistence stages and explicitly
marks the protocol as instrumented. Keep diagnostic and uninstrumented
acceptance results separate. Large evidence stays outside tracked sources.

## COW content-observation screening (macOS/APFS)

```sh
env NEXTEST_TEST_THREADS=1 cargo nextest run --release --locked \
  -p pvisor-overlay-core --run-ignored ignored-only --no-capture \
  -E 'test(content_observation_benchmark)'
```

`PVISOR_OBSERVATION_BENCH ` records compare actual `fingerprint_at` with
no-follow APFS `clonefile`: 3 warmups and 15 samples, alternating order. Inputs
are prepared outside the timer in `/private/tmp`; clones are read/verified and
source writes are checked for COW independence outside the clone timer.
The clone time excludes policy, durable publication, JSON journal, and later
apply comparison, so it is only a lower bound for an owned-preimage design.
No production journal path changes. Unsupported clone filesystems fail the
manual benchmark; do not silently replace the measurement with data copying.


### Deep directory VM workloads

`macos_migration.py` includes `metadata-deep-2048` and `search-deep-2048`. They retain the shallow workload's 2048 file contents and 32 search matches, changing only relative directory depth from 1 to 8. Fixture setup is outside the measured process interval. Search uses the supplied guest's `grep`, not ripgrep; no git/npm or Agent compatibility claim follows from these cases. Keep all paired samples and report each experiment separately, including P99 counterexamples. Do not run fixture checks, compilation, or other validation concurrently with a timing campaign.


Fingerprint spans distinguish `fingerprint_metadata` (layer resolution),
`fingerprint_xattrs`, and `fingerprint_content` (open/read/hash/encode).
They are inclusive sub-stages of `fingerprint`; content is not hash CPU time.


### READDIRPLUS capability verification

OverlayFs negotiates its own DO_READDIRPLUS/READDIRPLUS_AUTO capabilities independently of native layers. The macOS native passthrough capability set previously hid the overlay implementation. Compare protocol checkpoints separately from uninstrumented paired timing; retain only the last cumulative record per pid/component/instance. The 2026-10-05 30-pair results and exact scope are recorded in [the mechanism report](../../review_project/05-strategy/vm-performance-design/filesystem-readdirplus.md).


### Cloned destination verification

`cloned_destination_verification_paired_benchmark` is an ignored release lib test comparing full destination inventory with metadata/topology inventory whose contents are proven by kernel COW. It uses 3 warmups, 15 pairs and alternating order. Run after building the release test binary, with no concurrent compilation/tests. This measures verification only, excluding copy/fsync/source scans/VM restore. Production skips destination content reads only if all regular files have confirmed clone success (macOS WAS_CLONED or Linux FICLONE); any fallback retains full destination hashing. See [the evidence and ownership contract](../../review_project/05-strategy/vm-performance-design/filesystem-cow-verification.md).


### Leased stage materialization

`owned_stage_materialization_paired_benchmark` compares the public strict method with the internal immutable leased-stage restore used by the CLI. Each call includes copying, metadata and fsync; publication/open and post-call full verification are outside timing. Run the ignored test in a prebuilt release lib test binary with 3 warmups/15 alternating pairs. The ownership contract excludes external store edits during the lease; public strict APIs retain source content audits. [Results](../../review_project/05-strategy/vm-performance-design/filesystem-owned-stage.md) show that removing source hashing barely improves the 2050-small-file case, so copy/metadata/sync must be measured separately next.


### Stage copy breakdown

Set `PVISOR_FS_PROFILE=1` and run the prebuilt release lib test `owned_stage_materialization_diagnostic --ignored --nocapture`. `sealed-stage-copy` and `owned-tree-copy` report inclusive source inventory, native copy, privileged-mode metadata compensation, destination open/fsync and destination inventory. Use final records, never sum a parent span with children or cumulative checkpoints. Clone/fallback counters confirm which proof path ran. These are instrumented diagnostics, not uninstrumented acceptance medians. See [the breakdown and removed duplicate metadata work](../../review_project/05-strategy/vm-performance-design/filesystem-copy-breakdown.md).


### macOS persistence barriers

Rust `File::sync_all` uses `F_FULLFSYNC` on Apple. Owned-tree copy now fsyncs each destination inode and completes with one final parent `sync_all` to drain the same device. Linux is unchanged. `destination_sync` includes inode open/flush; `parent_sync` includes the final full barrier. Never interpret an inode flush alone as durable completion. [Verification and timing](../../review_project/05-strategy/vm-performance-design/filesystem-batched-sync.md) distinguish new-binary strict/leased pairs from historical independent before/after campaigns.


### Real stage VM gate

`vm_stage_snapshot.py` uses `snapshot_guest.rs` with a base marker `/stage-file-count`. The guest creates small files in the writable stage and checks their contents after restore. Run with eager raw RAM to isolate this gate from FUSE availability. Heartbeat must differ from the sealed value; full guest check and host fork write-isolation checks are separate. Report forks by index, since two restores within one trial are correlated. See [the native HVF evidence](../../review_project/05-strategy/vm-performance-design/filesystem-stage-vm.md).


### Journal ordering diagnosis

`vm_stage_snapshot.py --diagnostic-profile` explicitly marks instrumentation and preserves opt-in records. `journal_order` includes the macOS ordering barrier or unsupported-capability full-sync fallback; `journal_fsync` remains the final durable directory boundary. Use last cumulative checkpoints and retain failed trials separately from successful rechecks. [The implementation and unresolved startup failure](../../review_project/05-strategy/vm-performance-design/filesystem-journal-order.md) include exact scopes; later success does not erase an exit-125 trial.

## Real-tool paired cases on Apple Silicon

Explicit `--cases rg-2048,rg-deep-2048,git-status-2048,npm-offline-32` requires a guest rootfs with Linux ARM64 rg, git, Node and npm. The default BusyBox cases are unchanged. Git fixture creation/commit and copies are outside the timer. Npm uses 32 local-file packages, `--offline --ignore-scripts`, an isolated guest cache, and verifies every installed module result; this is not registry download/unpack or lifecycle-script coverage. Tool executable and APK package-manifest digests are recorded when available. Run a separate one-sample smoke before timing. Both compared binaries must have compatible guest address-space policy; startup failures are not speed samples.

## Indexed paths, lazy directories, admission and immutable receipts

The five-item optimization campaign is recorded under
`target/optimization-five-20261005`, with reviewable reports, source patch and
validation logs in [the evidence directory](../../docs/src/assets/benchmarks/filesystem-optimizations-20261005/).
Its baseline was copied before these edits;
the candidate applies only the seven relevant source/build files to that copy.
Concurrent lazy-cache/backend work is excluded from the comparison. Source and
artifact SHA-256 values are in `build-provenance-v3.json`; the frozen source trees
and build/test logs are retained alongside the reports.

The implementation changes are:

- **1.** Overlay inode paths retain `HashMap` lookups and add a component-ordered
   `BTreeSet` for subtree updates. Rename/unlink start a
   range at the affected path and stop at the end of its subtree, instead of
   scanning every known inode. Profile counters `nodes_remapped` and
   `nodes_removed` count affected entries. Inode identities and sibling paths
   remain intact across directory renames.
- **2.** `just build performance` builds `target/performance/pvisor` with `opt-level=3`
   and the release profile's LTO/ABI settings. The size-oriented release profile
   remains available for a controlled build comparison.
- **4.** New directory handles store names, directory type hints and stable offsets,
   then allocate inodes as each page is consumed. Plain `READDIR` uses the captured
   types, as the previous materialized handles did, without full child metadata
   queries. `READDIRPLUS` checks fresh attributes and policy and reuses that
   resolution's parent identities. Views with deny rules still validate children
   at open time to preserve hard-link filtering. Type hints grant no backing-file
   access. Old materialized and name-only directory cookies remain readable.
- **5.** A full I/O pool admits inline metadata until the next expensive request at the
   ring head; that head is returned to the ring with `undo_pop`. The pool remains
   bounded, with at most one additional descriptor held temporarily by the owner.
   Already-ready completions share a notification check/interrupt; inline replies
   notify immediately. No timer delays replies. Counters `pool_capacity_stalls`,
   `published_completions`, `notification_checks` and `used_interrupts` distinguish
   admission pressure from completion batching.
- **6.** Cold VM launch recognizes authenticated imported bases, holds their GC leases
   for the VM lifetime, and attaches digest-bound content receipts before entering
   Landlock. Only the receipt file is added as read-only host access. Mutable roots
   retain live fingerprints. A copied snapshot drops the original generation's
   receipt and fingerprints its own bytes; retained imported bases keep their
   existing leased receipt path.

The isolated candidate passes `just test pvisor-overlay-core pvisor-vm pvisor`
(877 passed, 31 skipped) and targeted Clippy with `-D warnings`. The integrated
worktree also passed the targeted tests (890 passed, 34 skipped), Clippy and
`just fmt`. The six real KVM cases S-DOC-057 through S-DOC-062 passed with the
isolated release CLI/SDK driver; review status remains UNREVIEWED. Regression tests
cover component-prefix siblings, partial directory reads and cookie restoration,
fresh attributes and denied aliases, full-pool inline admission, batched EVENT_IDX
notification, authenticated GC leases, and copied-generation receipt fallback.

Acceptance uses the frozen seven-workload environment, 2 vCPU / 4 GiB, affinity
`0,1`, rootless staged isolation for both artifacts, 3 warmups and 30 samples per
cell. `accept-code-v3-4g/report.json` compares original and optimized release builds;
`accept-profile-v3-4g/report.json` compares optimized release and performance builds.
The mutable fixture does not exercise receipt reuse, so these workload timings
cannot quantify item 6. Correctness gates verify tool results, Run Bundle
isolation, untouched lowers and expected upper contents before accepting timing.

The earlier 16 GiB `accept-profile` run terminated a trial with SIGKILL and is
incomplete; no cause such as kernel OOM is established. Its failure evidence is
retained. The smaller memory budget applies equally to both variants in the new
campaigns; do not mix their samples with the 16 GiB campaign. Earlier directory
implementations regressed Git status, so their results are retained separately
from the final name/type snapshot implementation. Whole-job results compare the
combined changes and do not establish how much each individual mechanism saves.

The final release comparison observed the following VM worker-time medians
(milliseconds). These are combined-change measurements, not per-mechanism
attribution:

| Workload | Original release | Optimized release | Change |
| --- | ---: | ---: | ---: |
| Whole-job completion | 4416.25 | 4236.61 | -4.07% |
| metadata | 194.21 | 169.20 | -12.88% |
| read | 119.64 | 118.28 | -1.13% |
| write | 235.28 | 244.97 | +4.12% |
| git | 404.79 | 382.24 | -5.57% |
| rg | 466.45 | 452.79 | -2.93% |
| cargo | 518.92 | 526.55 | +1.47% |
| npm | 1504.83 | 1395.37 | -7.27% |

Negative changes mean less time. Whole-job completion includes startup and
teardown and is distinct from individual worker times. VM completion P95
increased 2.35%; Git P99 increased 25.50%. The default release change therefore
does not establish a tail-latency improvement. Staged completion P50 changed
-0.26%, consistent with the adapter-specific nature of most changes. The write
and Cargo medians increased 4.12% and 1.47%, respectively. With 30 samples, P99
is strongly affected by individual trials; retain all raw samples.

The independent build-profile comparison uses the same final source. Its VM
medians are below; its release samples belong to this comparison and must not be
combined with the original-release comparison above.

| Metric | Optimized release | Performance | Change |
| --- | ---: | ---: | ---: |
| Whole-job completion | 4233.32 | 3839.44 | -9.30% |
| metadata | 154.31 | 149.17 | -3.33% |
| read | 117.99 | 117.12 | -0.74% |
| write | 246.94 | 221.11 | -10.46% |
| git | 499.61 | 331.47 | -33.65% |
| rg | 444.72 | 391.12 | -12.05% |
| cargo | 511.36 | 467.37 | -8.60% |
| npm | 1350.94 | 1242.92 | -8.00% |

Performance-profile VM completion P95 decreased 7.88% and P99 decreased 6.90%.
Staged completion P50 decreased 6.25%, but its P99 increased 8.54%; individual
workload tails also varied. The executable grows from 14.87 MiB to 18.59 MiB
(+24.96%). Use `just build performance` when that size tradeoff is acceptable.
These timings cover this fixed Linux/KVM fixture, not every workload or platform.

Final raw samples and provenance are preserved in
[the release comparison](../../docs/src/assets/benchmarks/filesystem-optimizations-20261005/code-v3-4g.tsv),
[the build-profile comparison](../../docs/src/assets/benchmarks/filesystem-optimizations-20261005/profile-v3-4g.tsv)
and [the manifest](../../docs/src/assets/benchmarks/filesystem-optimizations-20261005/manifest.tsv).


## Shared filesystem service and direct lazy-image version evaluation

The new integrated source is frozen under `target/fs-service-benchmark-20261005`.
The comparison baseline is the archived, already optimized v3 artifact above,
not an unoptimized P0 build. Source and binary hashes distinguish the frozen
new worktree from the harness-launch Git status. Concurrent cluster/service
changes are included in the new version, so this is not a single-function A/B.

Run the unchanged `filesystem_ab.py` with matched release or performance
artifacts, 2 vCPU / 4 GiB, affinity `0,1`, three warmups and 30 measurements.
Both staged variants require `--baseline-staged-isolation rootless_process`
and `--candidate-staged-isolation rootless_process`. These seven workloads use
local rootfs and do not exercise removal of the lazy-image host FUSE mount.

`filesystem_lazy_ab.py` separately supplies an immutable cache-v1 Unix-socket
fixture using the frozen development rootfs. Both binaries use performance
builds. The server has affinity `2,3`; the VM and runner have `0,1`. Each shuffled
version pair runs first with fresh client disk caches, then with those caches
reused and a new guest/projection/upper. Host page caches remain warm. There is
no artificial network latency; this is not a production Rust server or TCP/S3
throughput measurement. Preflight and three warmups are excluded from 30 samples
per cell. The guest's fixed 1.2-second TTL wait is excluded from operation times
and included in whole-job completion. Immediate repeated operations do not
guarantee that every attribute remains in the kernel cache.

The fixture verifies traversal of 2,048 × 1,023-byte files in 32 directories,
open/read/close of all files, 64 MiB SHA256, 32 small-file rootfs copy-ups, staged
workspace writes, VM Run Bundle isolation, and unchanged immutable source.
Mount sampling requires a host FUSE mount for the archived baseline and no
image-store host FUSE mount for the new version. Request counts demonstrate
identical cold content and zero warm content downloads. Download diagnostics
remain enabled equally for both artifacts. The first preflight's incorrect
fixture byte count is retained as failed evidence; corrected runs are separate.

```bash
python3 benchmark/pvisor/filesystem_lazy_ab.py \
  --assets target/reference-env-final-20261004 \
  --baseline target/fs-service-benchmark-20261005/artifacts/baseline-performance \
  --candidate target/fs-service-benchmark-20261005/artifacts/candidate-performance \
  --firmware target/p0-filesystem-artifacts-20261005/firmware \
  --output target/fs-service-benchmark-20261005/lazy-repeat \
  --samples 30 --warmups 3 --cpu-affinity 0,1 --server-affinity 2,3
```

The new release local-VM whole-job P50 is 4,152.52 ms (+3.26%), despite traversal
P50 of 157.57 ms (-6.50%). Lazy warm-cache open/read and bulk read improve 8.60%
and 6.65%, while traversal rises 13.71% and 32-file copy-up rises 151.65%.
Lazy whole-job P50 rises 2.29% cold and 0.64% warm. Removing the extra mount does
not establish general acceleration. The experiment does not separately profile
metadata projection, materialization, preimage and synchronization costs.

Full results and historical context are in the [benchmark article](../../docs/src/en/design/filesystem-performance-analysis.md#filesystem-service).
Public raw reports and provenance are preserved in [the evidence directory](../../docs/src/assets/benchmarks/filesystem-service-20261005/).
`plot_filesystem_service.py` renders the standalone SVG figures from those reports.

## Host stage journal profiling and optimization (2026-10-05)

The pure host FUSE control established that staging adds substantial work beyond
transport. The stage-specific campaign is retained under
`target/stage-hotpath-20261005`. It profiles the original frozen release and a
current-source baseline separately; historical artifact results are not the
same-source optimization comparison.

`host-fuse` reports inclusive callback spans, directory snapshot work, observation
aggregation, and inode reclamation. `reclaim_paths_scanned` and
`reclaim_objects_scanned` are work counters. `preimage-log` reports inclusive
transactions and append/refresh/sync spans, with separate lock-wait and binding
checks. `preimage-log-read` profiles replay by consumers. Use the last cumulative
record for each PID/component/instance; do not add nested or repeated snapshots.

The host baseline still used the legacy per-path JSON journal, although the VM
initializer already selected compact observations. A complete diagnostic job
published 2,705 observations and made 341 `journal_order` and 341
`journal_fsync` calls. Typical preimage service time was about 340 ms; copy-up was
only a few milliseconds. Host inode reclamation was also only a few milliseconds,
so this change does not modify inode caches or directory handling.

The owned host-stage initializer now opts into the existing compact log. It
retains durable promotion before backing mutation, first observations, replay,
apply conflicts and failure poisoning. Standalone mounts retain their default
format. Inspection does not select a new format; existing journals and nonempty
uppers are not migrated. The same diagnostic workload produces 2,705 verified
frames and about 335–336 log syncs, with preimage service around 180 ms. Probe
costs are excluded from acceptance timings.

Both artifacts use the same frozen source with identical profiling additions;
only the host mount configuration and owned initializer differ. Builds use one
isolated Cargo target directory and a stable build-source path. An earlier
shared-cache diagnostic reused another source tree's relative dep-info; its
`profile-current` data is retained and excluded. Source hashes, exact patch,
binary hashes, build logs and validation are in `build-provenance.json` and
`optimization.patch`.

Acceptance uses the unchanged frozen seven-tool fixture, fresh workspaces and
uppers, warm host page caches, affinity `0,1`, rootless staged isolation, release
builds, profiling disabled, three warmups and 30 samples per cell. The native,
baseline stage and candidate stage cells are shuffled each round.
`filesystem_ab.py --backends pvisor-staged` selects this host-only comparison;
the default still measures both host FUSE and VM.

Two eligible independent batches, `host-acceptance` and `host-repeat-clean`,
observed the following P50 changes. Their samples and percentiles are not pooled.
Times are milliseconds:

| Metric | First baseline / candidate | Clean repeat baseline / candidate |
| --- | ---: | ---: |
| metadata | 78.14 / 77.65 | 78.29 / 77.62 |
| read | 73.97 / 72.53 | 78.34 / 71.92 |
| write | 208.16 / 122.82 | 204.32 / 123.11 |
| git | 172.68 / 129.52 | 169.94 / 130.85 |
| rg | 91.68 / 89.01 | 91.73 / 90.13 |
| cargo | 125.96 / 99.37 | 121.10 / 97.17 |
| npm | 277.98 / 255.09 | 271.59 / 256.93 |
| completion | 1346.55 / 1175.92 | 1326.17 / 1170.74 |

Write medians improve 40–41%, Git 23–25%, and completion 11.7–12.7%. Completion
P99 increased in both batches: 1518.47 to 1594.61 ms and 2920.75 to 3266.49 ms.
These shared-host samples do not establish a tail-latency improvement. Metadata
traversal is largely unchanged; resolution and first-content-read fingerprints
remain measured costs. An intervening `host-repeat` batch overlapped 1.9 seconds
of previous-journal audit I/O and is retained with an exclusion record.

Each eligible batch has 90 verified jobs / 630 tool results and 60 audited stage
journals. Audits validate compact frame digests, unique first observations, all
256 new-file absence observations, 2,048 tree reads and the 64 MiB payload hash.
They run after timing completes. Each directory retains `report.json`,
`summary.tsv`, `samples.tsv` and `journal-integrity.json`; diagnostic spans are
also exported as `profile-stages.tsv`.

Targeted OverlayCore/FUSE tests passed (112), as did pVisor tests on the host
(496), the additional Linux FUSE roundtrip/apply/read-conflict case, 43 Python
harness tests, and targeted Clippy with warnings denied. The new mount-config
checks cover explicit selection, inspection, preserved legacy observations and
nonempty uppers. Initial sandbox EPERM failures are retained separately. This
campaign establishes host-stage median improvements; it does not measure a VM
speedup.

## Stage persistence boundaries (2026-10-05)

Owned stages default to `checkpoint` durability; `--stage-durability strict`
retains per-first-mutation synchronization. Execution still records the exact
first content observation. Explicit workload fsync orders the journal before
data. Completion stops writers, persists journal, upper data and directories,
then atomically publishes `preimages/sealed-v1`. Managed, interrupted stages
cannot be applied or reopened as complete. Live workspace checkpoints seal
only their copied backing. Legacy journals without a policy remain strict.

`filesystem_stage_durability.py` compares both policies using one pinned release
binary, fresh workspaces/stages, three warmups and 30 shuffled samples per cell.
Each sample verifies all seven workers, lower isolation, all 256 upper writes,
the requested policy and durable completion marker. Completion time includes
sealing. Evidence is in `target/stage-boundary-20261005/host`; its binary SHA-256
is `42aff3423cfa186867f0a2bcc37e6e95567d106a50c70a504a45edb0a33ece63`.

| Operation | Strict P50 ms | Checkpoint P50 ms | Change |
| --- | ---: | ---: | ---: |
| Metadata | 77.11 | 77.02 | -0.1% |
| Read 64 MiB | 77.32 | 77.89 | +0.7% |
| Write 256 files | 119.15 | 27.25 | -77.1% |
| Git | 127.19 | 123.53 | -2.9% |
| rg | 89.05 | 88.73 | -0.4% |
| Cargo | 96.05 | 84.80 | -11.7% |
| npm | 260.24 | 229.58 | -11.8% |
| Completion | 1295.85 | 1167.81 | -9.9% |

Single-job instrumented diagnostics are separate from these samples. Final
`preimage-log::sync` counters are 340 versus 5; this counts the open handle's
initialization and execution barriers, excluding the final seal's file/directory
syncs. Inclusive preimage time is 174.76 versus 67.79 ms. Both jobs retain 5,725
preimage calls and 2,157 content fingerprints (about 43 ms), so the improvement
comes from moving durability to boundaries, not dropping content checks.
Nested spans and cumulative checkpoints must not be added. This is a host FUSE
performance comparison; real VM lifecycle checks do not establish VM speedups.
The completed-marker, corruption and recovery tests are not physical power-loss
experiments.

Final validation passed 982 Rust tests across Core, OverlayCore, FUSE, VM and
pVisor, 48 Python documentation/harness tests, targeted Clippy and two explicit
Linux FUSE roundtrips. All 14 existing STAGE semspec cases passed without changing
their approval state. Three real KVM Job lifecycle cases passed separately:
ordinary, nested-stage and nested-stage pooled snapshots, each covering capture,
suspend, resume and execution fork. These are correctness checks, not VM timing
acceptance. Logs and source hashes are indexed by
`target/stage-boundary-20261005/verification.json`.

## Documented filesystem benchmark retest (2026-10-05)

The documented filesystem-service candidate release is compared with a newly
frozen/rebuilt current release, using the same fixture, firmware, affinity 0,1,
2 vCPU/4 GiB, rootless host staging and three warmups/30 samples. Both timing
batches finish before journal audits; no builds/tests overlap our measurements.
The five-cell main batch has 150 verified jobs. Host write P50 falls 86.4% and
completion 13.1%. VM write falls 42.1%, completion 6.2%, but completion P95/P99
regress. A separate native/old-VM/new-VM repeat has 90 verified jobs: VM write
falls 40.5%, completion only 0.9%, and the initial tail regression does not recur.
The evidence supports VM write gains, not stable overall/tail acceleration.
All valid samples remain in separate distributions. Ninety candidate journals
pass policy/seal, full-frame and original-content-observation audits afterward.

Frozen source/build records are in `target/stage-doc-benchmark-20261005`; the
main and repeat reports are `target/sb/run` and `target/sb/vr`. Short output paths
avoid the new VM control socket's length limit; the failed initial preflight is
retained and excluded. Public raw reports, journals, source overlay, provenance
and reproduction commands are linked from the bilingual
[stage benchmark](../../docs/src/en/benchmarks/filesystem.md#results).
