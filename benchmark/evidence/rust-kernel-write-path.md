# Rust native virtio-fs: write-path experiments

Engineering A/B (B-FS-ENG), not a replacement for the public filesystem table.
The Rust baseline is the [previous quiet kernel with demand-read batching](rust-kernel-filesystem.md#demand-read-batching),
not the earlier verbose kernel. Two avoidable operations mattered for small writes:
populating a page cache that no consumer had read, and fetching mode bits that
the current task was already permitted to retain.

## What changed

* A regular virtio-fs inode now allocates its cache on first buffered read or
  mapping. Before then, a write sends the caller's range through the existing
  direct FUSE write operation, including its short-write and error handling.
  The inode lock serializes cache creation, size updates and the choice of
  write path. A writer with an already-open descriptor switches to cached
  writes when a reader creates the cache. No dirty-data queue, new protocol,
  relaxed TTL or deferred fsync.
* The guest VFS still removes `security.capability` first. When the live LSM
  check grants `CAP_FSETID`, it need not read the file mode to decide whether
  to keep the set-ID bits. Other callers still need the mode, and the SGID
  group/execute rules still apply. No permission decision is cached across
  writes. On the mutable benchmark share a mode lookup can require `FUSE_GETATTR`.
* Two completion strategies were tested and **not retained**. Suppressing
  empty deferred jobs and briefly spinning on a one-shot completion reduced
  M06 from 1324 to 1240 ms in a three-block preflight. Polling the used ring
  on an otherwise idle queue, on top of lazy caching, reduced it from 1071
  to 936 ms in five blocks and reduced S03 from 19.17 to 15.86 s in three.
  The latter spins with local IRQs disabled and the queue locked; these trials
  did not establish scheduler/tail-latency cost under host contention. The
  retained changes do not spin. Do not combine the separate preflight gains.

## Registered workloads, new engineering cohorts

Fedora x86_64, Ryzen 7 9700X, Linux `7.3.3-300.fc45.x86_64`, KVM.
Fresh 1-vCPU/512 MiB VM and writable rootfs per case. Affinity 0,1; no dedicated
parent cgroup quota, host cache drop or automated interference gate. Seed 912
determines case and arm order within each of five blocks. The three resident
rounds in each VM are summed, not counted as independent samples. Every result
requires the correct case and round sequence, normal exit, DONE and a separate
host `--verify` of the retained bytes. Work plus final sync is measured by the
guest. GNU `time` user + system CPU includes the whole runner, including boot,
validation and exit; it is not a guest-only or work-only metric.

| Mechanism (ms, median) | Prior Rust | Lazy cache | Final Rust | Linux | Paired final − prior Rust, ms [95% interval] |
|---|---:|---:|---:|---:|---:|
| M06: 1 KiB sequential writes, final sync | 1379 | 1138 | 393 | 363 | −1008 [−1128, −981] |
| M08: 4 KiB random overwrites | 169 | 201 | 90 | 103 | −74 [−107, −55] |
| M10: fsync after each 4 KiB write | 365 | 368 | 328 | 362 | −37 [−58, −24] |
| M07: 1 MiB writes, final sync | 27 | 28 | 26 | 30 | −1 [−25, +2] |
| M04: 64 KiB sequential reads | 65 | 79 | 83 | 51 | +15 [−19, +19] |

Intervals are percentile bootstrap intervals over all $5^5$ resamples of
five paired differences. No multiple-comparison correction or tail claim.
M06 elapsed time fell 71.6% [67.1%, 73.6%], estimated from the paired ratios;
median whole-run CPU fell from 1.71 s to 0.56 s. This case did **not** establish
a win over Linux (paired difference interval −26 to +80 ms). The M04 read
median went up; the paired interval crosses zero vs prior Rust, and Rust
remained slower than Linux. The `lazy` arm predates a small read-path cleanup
in the final build; its subtraction is not a pure measurement of LSM cost.

The reviewed source also handles callers without an associated POSIX thread.
It was rebuilt, passed the hardware write/mapping/permission probe, and was
timed again in an independent five-block campaign (`native-fs-reviewed-mechanisms/`):

| Case (ms) | Prior Rust | Reviewed Rust | Linux | Paired reduction vs prior Rust [95% interval] |
|---|---:|---:|---:|---|
| M06 | 1404 | 432 | 381 | 68.3% [67.6%, 73.6%] |
| M08 | 164 | 91 | 101 | 45.0% [44.1%, 53.3%] |
| M04 | 67 | 65 | 50 | no resolved change (−12.6 to +3.5 ms) |

M06 whole-run CPU was 1.72 / 0.58 / 0.51 s, respectively. Both cohorts
agree that sequential reads still take longer than on Linux. These are
separate runs; no ratios or intervals pool their observations.

A separate instrumented M06 invocation per Rust arm (`PVISOR_FS_PROFILE=1`,
not a timing sample) corroborated the cause. The harness allowed a periodic
checkpoint after the task. Last cumulative rootfs counts for prior / lazy /
candidate: `GETATTR` **12732 / 12732 / 436**, `WRITE` **12294 / 12294 / 12294**,
`FSYNC` **3 / 3 / 3**, `FSYNCDIR` **3 / 3 / 3**. The workload issues 12288
small writes per VM. The counts include bootstrap and validation; they are
not per-call latency estimates. Source logs are in `native-fs-opcode-diagnostic/`.

### Unprivileged writes

An independent five-block run dropped supplementary groups, set UID/GID
1000 (the backing tree's owner), and checked that both effective capability
words were zero. These guests used the same workload and normal host oracle.
The UID 65534 preflight on the rootless passthrough fixture failed with
`EPERM` before producing any timing rows; it was not counted.

| Case (ms) | Prior Rust | Candidate | Linux | Paired change vs prior Rust |
|---|---:|---:|---:|---|
| 1 KiB writes | 1333 | 1166 | 388 | 17.8% less time [8.2%, 30.6%] |
| Random overwrites | 208 | 202 | 126 | no resolved change (−5.6 to +0.2 ms) |

The large privileged result does **not** represent ordinary user writes.
Without `CAP_FSETID` the permission check still needs the mode. Local
records: `native-fs-uid1000/`, `unpriv-fs-preflight/`, `user-fs-preflight/`.

### Does it help a complete task?

In S03 each guest round extracts the local archive, creates 128 more files,
writes a 32 MiB stream in 1 KiB chunks, verifies the tree, fsyncs files and
directories, renames the staging directory and fsyncs its parent. From the
same five paired blocks, three-round totals were **22.502 s prior Rust,
14.157 s candidate, 17.656 s Linux**. The paired reduction was 37.1%
[36.8%, 37.3%] vs prior Rust and 19.9% [19.4%, 20.1%] vs Linux. Median
whole-run CPU was 27.32, 17.63 and 21.90 s respectively. These are results
for this rootfs and task, not a general bandwidth or Linux filesystem ranking.
The workflow payload and the host verifier were frozen separately, SHA-256
`a66221031b8db246106f230f248ca009a7a1be5783222848625245ac3329ae20` and
`f1b6cae222648b3f53e380070537e1d30e8c2b7f3fb89e04dc2643569208d09e`.
An additional build on CPUs 8–15 overlapped the last workflow block; no
formal interference gate or cross-machine replication was applied.

Three new randomized blocks with the reviewed build (`native-fs-reviewed-workflows/`)
reproduced S03: **22.46 s prior Rust, 14.13 s reviewed Rust, 17.66 s Linux**.
All nine VMs passed host verification. The five-block estimates above are
from the earlier candidate, not obtained by pooling these runs.

S01 repository inspection was 3.877 / 3.872 / 5.017 s and S02 incremental
compilation 1.783 / 1.764 / 1.078 s (prior Rust / candidate / Linux). The
within-Rust differences were not resolved: paired intervals −0.491 to +0.049 s
and −0.144 to +0.032 s, respectively. In particular, the write change
did not close the compilation gap.

## Verification and reproduction

The hardware `--check-write-cache` probe uses a write-only FD, a sparse hole,
first buffered read or `MAP_SHARED`, another write through the original FD,
truncate/regrow and a final sync. Linux and Rust both exited with authenticated
status 7 and matched the host byte oracle. On guest tmpfs it also verified
that a write removes a preexisting empty file-capability xattr even with
`CAP_FSETID`, and checked executable and non-executable set-ID modes before
and after dropping the capability. The previous mixed warm/cold, unaligned
read and EOF probe passed on both kernels. The kernel builder tests passed
(11); no ARM KVM/HVF validation.

Use the [VM probe build recipe](../startup/README.md#linux-and-rust-firmware-stage-diagnostic-kvm).
Linux uses the GNU runner with `--firmware`; Rust uses an embedded kernel with
`PVISOR_GUEST_KERNEL=rust-native`. Mechanism launches use the same static payload
and read fixture as the previous experiment. For example, in a fresh prepared
rootfs containing `/subject`, `/bin/sh` and `/input`:

```sh
PVISOR_GUEST_KERNEL=rust-native timeout 90 /usr/bin/time -f '%U %S' -o cpu \
  taskset -c 0,1 "$probe" --rootfs "$root" --cpus 1 --memory 512 \
  -- /bin/sh -c 'exec /subject M06 /output /input 3 512 >/result 2>/errors'
"$payload" --verify M06 "$root/output" "$root/input" 3 512
```

Frozen kernel SHA-256 (not the host executable):

| Arm | SHA-256 |
|---|---|
| Prior Rust (`base`) | `ccd3b30376ea6d2e28d1522fe16640063b258939849d83c34fd75dd4c2a781c5` |
| Lazy cache (`lazy`) | `24cadbe1b43fbb6101d3356511c4e2b10c9d7da5f364cd092d58ef7656fd0e34` |
| Lazy cache + live capability check first (`lookup`, five-block comparison) | `f2a9421b19885c943a5f037d46a26290736ff83e187d1524262ad37a6dbf6d38` |
| Reviewed build (`final`, handles absent POSIX thread explicitly) | `0d7e33d046fe053c7309b6076fbbfd7ddeefe2a76865effd82a2dac5cc4ffd6c` |
| Linux `libkrunfw.so` | `6b8bf8378520b50ec2ce8ccc99c1a1b3722cb045bf585d7ac22881e3c9cc4bda` |

Raw records under `benchmark/filesystem/.data/`: `irq-preflight`,
`lazy-preflight`, `inline-preflight`, `native-fs-workflow-selection`,
`native-fs-final-mechanisms` and `native-fs-final-workflows`. Each batch
retains its order, command, per-VM stdout/stderr, guest phase records,
CPU and host oracle. Kernel receipts bind source and artifact hashes.

## Rebase verification on main `908aa71c`

Same x86_64 KVM host, frozen payloads and verifier, fresh roots, one rebuilt
release runner (`2b99b055`) for both firmware selections. Rust kernel SHA-256:
`49da6339256d4e1ee91f3a672e49af43c9cca3e8cabf62b40198ce64caac8a07`.
Local records: `benchmark/filesystem/.data/pr183-2b99b055/`.
These are independent firmware comparisons, not a new prior-Rust A/B.

| Median completed time (three resident rounds per VM) | Rust native | Linux |
|---|---:|---:|
| M04 sequential read | 65.6 ms | 50.9 ms |
| M06 small writes | 380.7 ms | 356.8 ms |
| M07 large writes | 20.9 ms | 18.7 ms |
| M08 random overwrites | 93.6 ms | 101.9 ms |
| M10 per-write fsync | 322.8 ms | 325.3 ms |
| M06, UID 1000 without effective capabilities | 1095.2 ms | 358.8 ms |
| M08, same UID | 163.6 ms | 102.3 ms |
| S01 repository inspection | 3.961 s | 5.122 s |
| S02 incremental compilation | 1.746 s | 1.051 s |
| S03 unpack and publish | 14.200 s | 17.685 s |

All 88 filesystem VMs passed the host oracle (five paired blocks per
mechanism, three per workflow). S03 reproduced the earlier reduction in
elapsed time; ordinary-user small writes still took about three times as long.
In five separate startup pairs, console ACK was 33.2 / 43.1 ms (Rust / Linux),
but spawn to verified shell marker was 95.3 / 69.9 ms. No historical samples
or failed launches using the obsolete firmware selector enter these medians.

The rebased guest regression probe passed both filesystem modes on Rust.
On Linux the new main-branch UUID rewind assertion failed before the filesystem
checks; the pre-rebase probe passed both filesystem modes and the host byte
oracle. This is not a pass of the rebased standalone probe on Linux.
