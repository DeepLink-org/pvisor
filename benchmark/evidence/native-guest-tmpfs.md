# Native guest tmpfs: sparse data and concurrent readers

Engineering evidence for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
The [preceding virtio-fs round](native-guest-file-io.md) left one noisy
`tmpfs-shuffled-read` cell above the gate. Unlike the disk tests, its
timed loop is entirely inside the guest. No mount options, host cache
policies, FUSE features, clocks, probe code or thresholds changed here.

## Full regression gate

Three complete 45-cell cohorts, in order: the previous-round executable,
the PR #209 executable, then the new executable. Only the pVisor runner
hash differs across manifests. Times below are Criterion medians in ms
per workload, not paired estimates.

| Native guest workload | PR #209 | Previous round | This round |
|---|---:|---:|---:|
| `tmpfs-shuffled-read` | 0.338 | 0.335 | 0.362 |
| `tmpfs-read` | 0.506 | 0.502 | 0.488 |
| `tmpfs-shuffled-overwrite` | 0.389 | 0.383 | **0.459** |
| `tmpfs-write` | 7.826 | 7.734 | 7.576 |
| `tmpfs-dirscan` | 0.695 | 0.433 | 0.415 |
| `disk-warm-read` | 25.014 | 13.181 | 13.231 |
| `disk-shuffled-read` | 25.199 | 13.213 | 13.099 |
| `disk-write-fsync` | 30.977 | 26.310 | 25.301 |
| `disk-shuffled-overwrite-fsync` | 32.565 | 28.651 | 26.806 |

**Both comparisons failed the unchanged 15% gate.** Against PR #209,
1/30 product cells failed: `tmpfs-shuffled-overwrite` (+17.85%).
Against the previous round, that cell (+19.91%) and Linux firmware's IPC
pipe round-trip (+15.45%) failed. Shuffled read is now within threshold,
but was **slower**, not faster, in the short sequential cohorts.

To investigate overwrite, the 25-pair run of the *unchanged* short
workload gave 0.456 → 0.409 ms (median paired reduction 8.9%;
95% interval −1.2% to +22.6%). Another 15-pair diagnostic did 64
resident passes, alternating byte patterns and checking the final file;
4 KiB `pwrite` took 330.12 → 305.82 ns/call. Neither follow-up rescinds
the failure or establishes that the one-pass regression was machine noise.

### Pre-PR rerun

A fresh three-cohort run measured #209, the previous round, and the rebuilt
candidate (`2354e9ed2a94`) in that order. Only the runner hash differed.
All 15 Rust native cells passed both comparisons. Against #209, native
warm and shuffled disk reads fell from 25.377 / 25.909 ms to 13.412 /
13.230 ms; sequential and shuffled writes with `fsync` fell from 31.344 /
33.820 ms to 25.455 / 26.747 ms. Native tmpfs shuffled overwrite was
14.61% slower than the previous round, just below the gate threshold.

**The overall gate still failed 4/30 cells against each baseline.** This
time all four failures were in Linux firmware, whose bytes did not change:

| Linux firmware cell | #209 → candidate (ms) | vs. #209 | vs. previous round |
|---|---:|---:|---:|
| `tmpfs-read` | 0.388 → 0.672 | +73.03% | +77.01% |
| `tmpfs-shuffled-read` | 0.317 → 0.659 | +107.91% | +222.09% |
| `tmpfs-shuffled-overwrite` | 0.264 → 0.634 | +140.49% | +134.65% |
| `tmpfs-metadata` | 4.854 → 5.958 | +22.75% | +23.37% |

For Linux shuffled read, none of the ten candidate samples overlapped the
#209 range. The sequential gate does not distinguish host conditions from
VMM-version effects. A randomized same-firmware VMM comparison is still
needed. This rerun does not replace the earlier failing cohort. Full local
records: `benchmark/vm/.data/gate-20261126T151020Z.dYy9UC/`.

## What the tmpfs read actually does

The native kernel implements tmpfs on ramfs, using a sparse anonymous
VMO (virtual memory object) per regular file. It formerly used the same
page collection path as a filesystem that fetches missing pages from a
backend. A read of a missing anonymous page committed a zeroed frame;
a resident read allocated a temporary vector even for one page. A file
mutex serialized user copies, including reads that did not modify data.
On non-page-aligned EOF the inode separately computed the logical read
count, but passed the *whole* destination to the page-rounded VMO.

The new anonymous path looks up and pins each resident frame before user
copy, dropping the non-preemptible cursor guard first. A hole is read as
zeros; zeroing a hole does not materialize it either. Writes still commit
a zero-initialized frame. Backed caches keep their demand-read batching
and writeback rules. Ramfs now takes a shared cache lock across the size
check and copy; writes, truncation and hole punching take the exclusive
lock. The destination is limited to logical EOF. Growing writes pass the
actual new EOF into resize, not the end of the page to which capacity rounds.

## Measurements that isolate the mechanisms

Ryzen 7 9700X, Linux x86_64/KVM, 256 MiB, host CPUs 2–5. Each block ran
both arms in random order, each in a fresh VM, without concurrent builds
or deliberate cache drops. Intervals below are 95% percentile bootstrap
intervals for the median of the within-block reductions `1 − after/before`
(20,000 resamples, seed 20912; no multiple-comparison correction). Raw
medians are not paired estimators.

**Single reader, resident vs. sparse.** Fifteen blocks, previous-round
runner vs. final runner, one vCPU. The resident diagnostic uses repeated
passes of a 4 MiB file to separate per-call work from copying. A constant
syscall workload (`getpid`) and a copy-only loop are controls, not costs
subtracted from file operations.

| Operation | Previous | Final | Paired reduction [interval] |
|---|---:|---:|---:|
| 1-byte positioned read from shuffled resident pages | 200.31 ns/call | 176.65 ns/call | 11.2% [11.1%, 11.9%] |
| 4 KiB shuffled resident `pread` | 265.85 ns/call | 252.80 ns/call | 5.2% [3.7%, 6.2%] |
| 128 KiB sequential resident `pread` | 2.461 µs/call | 2.505 µs/call | −4.7% [−8.1%, 0.03%] |
| At-EOF `pread`, 1 byte requested | 117.13 ns/call | 119.13 ns/call | −1.7% [−2.6%, −0.7%] |
| `getpid` control | 67.40 ns/call | 68.39 ns/call | 0.1% [−1.6%, 1.2%] |
| User-space 4 KiB shuffled `memcpy` control | 70.84 ns/call | 69.42 ns/call | 1.6% [0.4%, 4.2%] |
| First read of 32 MiB of holes, 4 KiB/call | 52.963 ms | 1.431 ms | 97.3% [97.2%, 97.4%] |
| Guest `MemFree` decrease during that sparse pass | 32,888 KiB | 0 KiB | not a latency metric |

The sparse pass reuses an already-touched destination, checks for zeros,
and samples `/proc/meminfo` before and after. `MemFree` measures the guest
allocator, not host RSS. The sparse result does **not** describe a dense
file. The 128 KiB result does not establish an improvement, and the EOF
case is slightly more expensive with the size check under the cache lock.

**Same inode, concurrent reads.** Seven blocks, four vCPUs, changing
only the file cache locking on top of the sparse-read change. Each reader
does 32 passes over the same 4 MiB file, with its own buffer and displaced
block order, verifying the boundary bytes. These are elapsed times for a
fixed *per-reader* workload; compare across arms, not thread counts.

| Readers × request | Exclusive lock | Shared read lock | Paired reduction [interval] |
|---|---:|---:|---:|
| 1 × 4 KiB | 8.071 ms | 7.924 ms | 5.0% [−11.8%, 7.9%] |
| 2 × 4 KiB | 19.266 ms | 12.233 ms | 36.5% [32.2%, 43.0%] |
| 4 × 4 KiB | 46.932 ms | 25.249 ms | 46.2% [44.6%, 46.3%] |
| 4 × 128 KiB | 25.001 ms | 3.134 ms | 88.0% [75.9%, 90.0%] |

**Small appends.** Eight blocks comparing the shared-read build before
and after changing the anonymous write and zero-fill paths. For 1,024
successive 64-byte appends, elapsed time fell from 425.34 to 347.51 µs
(paired reduction 19.1% [13.4%, 39.4%]); at 4 KiB the interval included
zero (7.783 vs. 7.626 ms). No dirty buffer or delayed acknowledgement.

The original [`probe.c`](../vm/probe.c) also ran in 25 separate randomized
pairs (one vCPU). For the short `tmpfs-shuffled-read` cell the medians were
0.391 and 0.367 ms, but the paired reduction interval was **−2.5% to
+19.9%**. The short cell alone cannot establish a stable speedup.

## Reproduction and correctness

Use [the original gate](../vm/guest.md), the same release harness, and the
same static probe. For this batch the previous-round runner SHA-256 starts
`fc9c3eaed76d`, the PR #209 runner `af3d1cf80fc3`, and the new runner
`25ad63035f4d`. Neither Linux firmware nor Firecracker changed. Local
records under `benchmark/vm/.data/tmpfs-native-2/` include all gate
manifests and samples, `read-cost.c`, `io-phases.c`, their paired drivers,
per-VM outputs, and `paired-summary.json`. The 25-pair original-probe run
is in `benchmark/vm/.data/fs-ab/step2-final-pairs/`.

The existing [staged publication probe](../../pvisor-kernel/tests/write_path.rs)
also ran in nine shuffled four-arm blocks, with independent host byte
verification after every VM exit. For a new 4 MiB file generated by a
non-root guest in 1 KiB writes, direct sharing took 139.69 → 141.38 ms;
tmpfs plus bulk export, file sync, rename and directory sync took
16.52 → 16.10 ms. Most of the difference between modes still comes from
*explicitly* publishing in larger chunks, not from this change to tmpfs.
There is no transparent transaction on an ordinary shared mount.

The [KVM regression workload](../../pvisor-kernel/tests/guest.rs) checks
holes adjacent to live data, exact short reads, untouched output at and
beyond EOF, hole punching, mapped-page tail contents on growth, and two
readers racing truncation/regrowth.
It also reads one byte per page of a 96 MiB hole on a 64 MiB guest. The
old runner failed the output-suffix assertion; the new runner completed
the full check with the required exit 7. The generated debug runner and
unoptimized static guest were tested using the same invocation as CI.
The separate release KVM cases for unaligned backed reads, sparse writes,
shared mappings, short host writes, errors and directory iteration passed.

`mise run test -- pvisor-vm` passed 835 tests (13 skipped) and
`mise run kernel -- test` passed 11 builder tests. The anonymous-hole
ktest and both packed-write reply ktests emitted explicit PASS results on
Firecracker/KVM with the OSDK test ELFs entered at `__linux64_boot` and
`earlycon` enabled. This was a substitute for the standalone OSDK/QEMU
runner, which timed out. Firecracker lacks the QEMU `0xf4` poweroff port;
the host bounded the VM lifetime after capturing the guest's test results.
The ARM64 guest cross-built; ARM hardware was not tested. After timing,
only VMO documentation changed in the compiled kernel sources; the
documentation-only x86 rebuild was rechecked on KVM and measured in the
separate pre-PR rerun above.
