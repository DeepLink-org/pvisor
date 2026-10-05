# Filesystem profiling and small-file optimization

`PVISOR_FS_PROFILE=1` enables opt-in aggregate diagnostics in OverlayCore and
the VM virtio-fs adapter/protocol server. Disabled profiles do not acquire
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
cache live host attributes indefinitely. Directory snapshot wire format and
device freeze/restore contracts are unchanged.

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
