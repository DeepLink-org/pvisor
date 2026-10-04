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
`measurements[label].calls` and `total_ns` describe timed operations;
`units` describes a separately named work counter, such as `resolve_components`,
`fingerprint_bytes`, `journal_publications`, or `copy_up_bytes`.
`layer_parent_stats` and `layer_leaf_stats` count metadata calls inside the
layer-path helper only. They exclude whiteout/opaque probes, other Core paths,
native-adapter syscalls, and xattr work: they are not a syscall census.
`mount_identity_attempts` counts Linux parent mount-context queries when a
backing-lookup caller asks for identities.

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
run may have no periodic record. These spans measure server service time;
they do not independently measure guest scheduling or queue waiting time.

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
