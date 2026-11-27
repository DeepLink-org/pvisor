# Rust guest file I/O on a mutable virtio-fs root

Engineering evidence for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng);
not a physical disk or cross-runtime bandwidth comparison. The [final report
on #209](https://github.com/DeepLink-org/pvisor/pull/209#issuecomment-7164248032)
put native warm and shuffled reads at 54.8 and 52.7 ms, against 0.27 and
0.52 ms on Firecracker. Its two failing gate cells were *not* these disk
tests. Firecracker uses a private ext4 block device; pVisor must observe a
mutable directory shared with the host.

## Mutable virtio-fs round: default-path result

Both full three-backend cohorts completed (45 cells each). **The original
15% gate failed 1/30 product cells.** `tmpfs-shuffled-read` is included
below; the failure is not cancelled by any faster cell or reference. The
numbers are Criterion medians, ms/workload, from sequential cohorts, not
the paired mechanism estimates below.

| Rust native filesystem workload | Base | Candidate | Change |
|---|---:|---:|---:|
| `disk-write-fsync` | 31.757 | 24.499 | −22.85% |
| `disk-warm-read` | 24.563 | 13.116 | −46.60% |
| `disk-shuffled-read` | 24.715 | 13.237 | −46.44% |
| `disk-shuffled-overwrite-fsync` | 34.165 | 25.140 | −26.42% |
| `tmpfs-write` | 7.846 | 7.740 | −1.35% |
| `tmpfs-read` | 0.532 | 0.589 | +10.61% |
| `tmpfs-shuffled-read` | 0.322 | 0.371 | **+15.25%** |
| `tmpfs-shuffled-overwrite` | 0.436 | 0.407 | −6.68% |
| `tmpfs-metadata` | 3.371 | 3.253 | −3.52% |
| `tmpfs-dirscan` | 0.668 | 0.411 | −38.47% |

Linux firmware passed all 15 cells. The identical Firecracker reference
varied too: its shuffled tmpfs read went from 0.327 to 0.363 ms, and even
its CPU integer workload ran 27.92% slower in the second cohort. This is
not evidence that the native tmpfs regression is caused by Firecracker
or that the gate passed.

To investigate the failed cell, a separate 15-block randomized base/candidate
run repeated the same tmpfs operations. Shuffled read medians were 0.377 /
0.430 ms; the median **within-pair** change was +1.5% (95% bootstrap interval
−3.6% to +18.0%). Sequential read's paired change was +6.5% [−5.7%, +11.3%];
dirscan's was −35.7% [−38.4%, −33.2%]. Seed 20911 for arm order, 209 for
20,000 percentile bootstrap resamples. The read interval includes both no
effect and a change above the threshold. No FUSE request is required in
this tmpfs read loop, and the final build does not change its VMO read code.
These runs did not resolve the read regression or replace the gate. A
[separate tmpfs investigation](native-guest-tmpfs.md) uses the final
executable from this round as its incremental baseline.

## Where the time went

Baseline `9c248c37`, Ryzen 7 9700X, Linux `7.3.3-300.fc45.x86_64`, btrfs,
affinity 2–5, one guest vCPU, 256 MiB. Separate diagnostic runs bracketed
the 1,024-operation loops in [the PR probe](../vm/probe.c) with a pause
and `fstat` to obtain two cumulative `PVISOR_FS_PROFILE=1` checkpoints.
The delta includes one extra `GETATTR` for the checkpoint. No profiling in
the timing runs.

| Timed operation | FUSE requests, excluding the checkpoint | Host protocol service in this run |
|---|---|---:|
| Sequential warm read | 1,024 `GETATTR`, **zero `READ`** | 0.39 ms |
| Shuffled warm read | 1,024 `GETATTR`, **zero `READ`** | 0.38 ms |
| Sequential write and sync | 1,024 `WRITE`, one `FSYNC` | 2.20 + 4.95 ms |
| Shuffled overwrite and sync | 1,024 `WRITE`, one `FSYNC` | 2.39 + 5.13 ms |

The reader repeatedly validates size and mtime, not file contents. A longer
attribute TTL would change what an already-open descriptor can see. A 4 KiB
write must account for the host's accepted byte count. Both paths formerly
paid for the worker wakeup, guest sleep, interrupt handling and return for
each operation on the default four-queue device. An ordinary KVM doorbell
uses `ioeventfd`; it is not necessarily a userspace MMIO exit.

The single-queue fast metadata path provided a useful mechanism control.
In three shuffled blocks of fresh VMs, warm read medians were **23.89 ms**
with four queues, **23.43 ms** with one queue and inline service disabled,
and **13.52 ms** with one queue and inline service enabled. Shuffled reads
were **23.90 / 23.25 / 13.40 ms**. Changing the queue count alone did not
explain the difference. Writes did not benefit from inline metadata.

The guest now uses that path on an idle multiqueue ring on one vCPU without
removing workers. The host checks for unpublished completions, including
stolen requests, before claiming a single metadata request. A busy ring
falls back to its worker. SMP with multiple queues keeps the parallel path:
concurrent inline calls would serialize on the MMIO device lock.

We also read the test vCPU's `KVM_GET_STATS_FD` counters between two guest
stdio handshakes around the operation (one diagnostic VM per cell, including
the handshakes, no inference from FUSE counts). Before the change the warm
read took 4,159 total exits, including 2,058 `mmio_exits` and 1,014
`halt_exits`. With multiqueue inline revalidation and packed replies it took
1,070 / 1,032 / 2, respectively. This is still a host validation per read,
not private ext4 page-cache speed. Sequential write+fsync still incurred
2,056 MMIO exits in this intermediate build: metadata elimination alone
does not solve data acknowledgements.

## Fewer exits for synchronous writes

On an ordinary used-ring interrupt, the old guest reads interrupt status
and writes the acknowledgement, two synchronous MMIO exits. With negotiated
[read-to-ack](../../pvisor-kernel/README.md#experimental-selection), one
MMIO read returns and clears **all pending causes**. Queue and configuration
events keep their normal callbacks. Neither the write nor `fsync` returns
early. Baseline and candidate still do actual host I/O. Linux firmware does
not receive the new feature offer.

Same native-enabled executable, feature off vs. on, seven randomized pairs
per cell, one fresh VM per operation. Affinity `2` forces the host worker
and guest vCPU to share a CPU; `2–5` allows four physical cores.

| Host affinity | Operation | Off → on median, ms | Median paired reduction [95% interval] |
|---|---|---:|---:|
| 2–5 | `disk-write-fsync` | 29.87 → 24.32 | 17.2% [14.1%, 18.6%] |
| 2–5 | `disk-shuffled-overwrite-fsync` | 30.71 → 25.48 | 17.0% [14.3%, 18.5%] |
| 2 | `disk-write-fsync` | 50.50 → 44.85 | 10.9% [9.9%, 17.3%] |
| 2 | `disk-shuffled-overwrite-fsync` | 50.77 → 45.44 | 10.9% [8.5%, 11.3%] |

Intervals are percentile bootstrap intervals on per-block `1 − on/off`,
20,000 resamples, seed 22; no multiple-comparison correction. All attempts
are included, including one slower single-CPU overwrite. In separate
counter runs, write+sync `mmio_exits` fell from **2,060 to 1,035**; shuffled
overwrite+sync from **2,058 to 1,037**. `halt_exits` stayed near 1,024.
This eliminated an acknowledgement exit, not the wait for host completion.

## Other mechanisms tried

- **Pack small control replies:** return header and body in one DMA buffer
  rather than allocating and synchronizing two. The accepted write count
  is validated once. A 4 KiB write now uses three descriptors rather than
  four, but it still receives a host reply and the final `fsync` still runs.
  Large control replies and caller-supplied page buffers stay separate.
  The [short-write fixture](../../pvisor-kernel/tests/write_ack.rs) exercises
  the synchronous, cacheless file path. The existing page-writeback batch
  does not propagate a partial byte count to its caller; it was not expanded
  into a deferred-write path.
- **Serialize a directory record once:** the tmpfs directory scan does not
  cross virtio-fs. `getdents`/`getdents64` now reuse space for one record
  across the call instead of allocating a `CString` per entry and making
  separate user copies for header, name and padding. In five randomized
  preflight blocks the base / directory-change arm took 0.644 / 0.451 ms;
  that arm also changed a FUSE waiter not used by the timed tmpfs loop.
- **Rejected: spin before sleeping.** An IRQ-enabled wait of up to 8 µs for
  small synchronous requests reduced sequential write+sync from 31.12 to
  28.80 ms with four host CPUs available, but increased it from 53.39 to
  64.81 ms when the vCPU and its worker shared a single host CPU. It did not
  improve the four-CPU shuffled overwrite. No new polling or tuning knob.
- **Not retained: special-case one-page VMO reads.** Removing a small local
  batch allocation did not remove a host validation or show a consistent
  win on the shuffled tmpfs control.

## What a staged commit can and cannot buy

We separately ran the [single-new-file publication experiment](../../pvisor-kernel/tests/write_path.rs),
as UID 1000 without effective capabilities. Both arms write 4 MiB in
4,096 calls, synchronize the file, rename and synchronize its directory.
The local arm builds on guest tmpfs, then copies in 256 KiB chunks to a new
shared temporary file, syncs, renames and syncs the shared directory. The
host verified every block after the VM exited. Three shuffled pairs:

| Time from first output write through publication (ms) | Pair 1 | Pair 2 | Pair 3 |
|---|---:|---:|---:|
| Write directly to shared root | 197.59 | 196.30 | 194.65 |
| Stage locally and publish | 18.27 | 17.83 | 18.13 |

This really removes small FUSE writes, but it is **one new, explicitly
published file**, not transparent writeback or a multi-file transaction.
Before publication a host reader cannot see intermediate bytes; a failed VM
loses guest RAM, and host errors can arrive at publication instead of at the
original write. Existing file descriptors and hard links are not redirected
by `rename`. Do not enable this on ordinary `:rw` shares. A general staged
commit needs an explicit ownership boundary, a bounded dirty-data budget
and a recoverable host-side publication/error contract.

## Reproduce and inspect

Use the [three-backend gate recipe](../vm/guest.md) with base `9c248c37`
and a separate build of the candidate. Keep the same harness, guest C
executable, Linux firmware, Firecracker v1.13.1 and its Linux 6.1.141 ELF.
Run the two cohorts sequentially with `taskset -c 2-5`; do not profile the
Criterion runs. For the IRQ-only comparison, launch the same native runner
with `PVISOR_VM_FS_IRQ_READ_ACK=0` and `=1`, alternating order in fresh
roots. Each disk cell checks lengths and contents; both writes include
the final `fsync`. The [publication probe](../../pvisor-kernel/README.md#tests-and-hardware-acceptance)
has separate host verification.

Local raw records are in `benchmark/vm/.data/fs-ab/`: `base/`, `candidate/`
and `criterion/` for the gate, `mechanisms/` for phase counts and staged
publication, `component-pairs/` and `irq-pairs/` for the ablations, and
`tmpfs-followup/` for the three paired tmpfs diagnostics. Only `vm_sha256`
differs between gate manifests (base `af3d1cf80fc3`, candidate `fc9c3eaed76d`).
The embedded kernels' SHA-256 prefixes were `f0488852a6c7` and `7cdad21a9d5a`.
`diagnostics/` contains the frozen diagnostic probe variants and observer.
The stats observer opens a stats FD in its own test VMM and reads cumulative
vCPU counters; it is not part of the product. The guest pauses at both
boundaries, so those records are for exit counts only, not latency. No host
cache drop, dedicated host, or cross-machine replication. A separate small
static C correctness fixture compiled during the candidate tmpfs shuffled
overwrite cell; no samples were dropped.

**Correctness checks, separate from timing.** The final x86/KVM runner
observed host rewrite, growth, truncate and rename/recreate through live
descriptors, with inline metadata enabled and disabled and with one and
four queues. The SMP multiqueue run stayed on the worker path. Three
seeded staged-namespace stress runs passed. Sparse writes, short replies,
`EFBIG`, host byte verification, buffered and mapped I/O, unaligned reads,
EOF, and directory scan retry/resume all passed. The builder tests and
`mise run test -- pvisor-vm` passed (835 host tests, 13 skipped). The new
write-frame ktests compiled, but standalone QEMU did not report a test
result before timeout; the KVM fault fixture tests real host replies and
is not a replacement for the malformed-frame unit cases.
