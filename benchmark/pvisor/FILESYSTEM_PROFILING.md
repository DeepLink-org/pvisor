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

Journal spans separate lock acquisition (`journal_lock_wait`), destination
lookup, serialization, temporary-file creation, writing, atomic publication,
loser cleanup, and fsync. `journal_publications` counts attempts, not successful
first observations. Native no-replace rename consumes the temporary file on
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

Preimages retain the per-path JSON format. Publication uses macOS
`RENAME_EXCL` or Linux `RENAME_NOREPLACE`, falling back to hard link/unlink
when unsupported. All routes publish complete contents without overwriting
the first observation. A losing writer verifies and adopts the winner;
mutation still syncs the winning file and its directory before changing upper.
Read-only observations continue to survive normal stage copy/reopen without
promising power-loss durability until promoted by a mutation.
