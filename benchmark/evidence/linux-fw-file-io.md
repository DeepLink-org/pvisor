# Linux firmware filesystem I/O (B-VM-GUEST-ENG)

This is an engineering A/B on x86_64/KVM, not a storage product comparison.
Keep three questions separate: does Linux tmpfs go to the host, does a shared
file need to go to the host, and can a producer explicitly publish a new
artifact instead of updating shared bytes in place?

## Default path: gate and paired measurements

Baseline is the #212 transport (`f32f3e6d`, before the Linux changes), not
the original #209. Two complete 45-cell cohorts used the [gate recipe](../vm/guest.md):
Ryzen 7 9700X, CPUs 2–5, btrfs, fresh 1-vCPU/256 MiB guests. The C probe,
harness, embedded Rust guest and Firecracker reference were held fixed;
the Linux .config SHA-256 was `f82a281571ec…` in both firmware receipts.
Linux firmware hashes were `7082f46e5754…` / `c5b874abfebc…`; runner hashes
`2354e9ed2a94…` / `93dd0f32526b…`. Run directory:
`benchmark/vm/.data/linux-fw-io-20261126T153538Z/` (local, not tracked).
Neither the gate probe, inclusion rules nor thresholds were changed.

Linux file workloads, Criterion median ms per **whole** workload:

| Workload | Before | After | Change |
|---|---:|---:|---:|
| Disk sequential write + fsync | 43.609 | 36.959 | −15.25% |
| Disk warm sequential read | 28.008 | 12.638 | −54.88% |
| Disk warm shuffled read | 28.939 | 12.597 | −56.47% |
| Disk shuffled overwrite + fsync | 35.058 | 29.830 | −14.91% |
| tmpfs write | 8.408 | 8.738 | +3.93% |
| tmpfs read | 0.407 | 0.422 | +3.74% |
| tmpfs shuffled read | 0.267 | 0.361 | **+35.09%** |
| tmpfs shuffled overwrite | 0.317 | 0.348 | +9.80% |
| tmpfs metadata | 4.842 | 4.901 | +1.22% |
| tmpfs dirscan | 0.289 | 0.293 | +1.19% |

**Gate failed, 25/30 passed.** The other four failures were Linux CPU
(+18.00%), getpid (+22.05%), pipe roundtrip (+62.66%) and memory copy
(+45.19%). All 15 Rust guest cells passed. The 15 Firecracker cells are
references, not votes. The full report is `compare/report.md` in the run
directory. Do not replace failed cells with a diagnostic's median.

Separately, 15 randomized blocks of the four disk workloads on the **final
defaults** gave median within-block changes of −52.39% (warm sequential),
−52.16% (warm shuffled), −11.39% (sequential write+sync) and −16.90%
(shuffled overwrite+sync). A block uses the same guest probe, randomizes
workload and arm order, and gives each observation its own root and VM.

## Where the time goes

**tmpfs.** First we fixed the Linux firmware bytes and randomized #209 vs
#212 VMM order. In 15 blocks, arm medians for read / shuffled read /
shuffled overwrite / metadata were respectively 0.412 / 0.322 / 0.353 /
4.928 ms vs 0.383 / 0.334 / 0.329 / 4.968 ms. The earlier 73–140% read/write
increases did not reproduce. On independent diagnostic boots we bracketed
the operation with host profile checkpoints on the root mount. All four
tmpfs phases had **zero workload FUSE calls**; one marker IOCTL appears in
the difference. The shuffled read had 9 total KVM exits including its two
stdio handshakes, not an exit per syscall. Linux already represents a hole
without committing a page: a first 32 MiB hole read used no measurable
additional guest free memory. These are not reasons to change shared-mount
TTLs, weaken the memory configuration or patch Linux shmem.

The new gate failures were followed up in 25 randomized three-arm blocks:
baseline, candidate with both transport features off, final defaults. The
median **paired** percentage changes (defaults vs baseline) were +0.14% CPU,
+0.05% getpid, −0.37% pipe, +4.27% memory copy and +8.54% tmpfs shuffled
read. The latter two 95% block-bootstrap intervals were [−9.24%, +18.95%]
and [−11.59%, +81.89%]; neither excludes a >15% regression. This follow-up
does not establish the cause of a particular slow boot.

A separate diagnostic repeated the warmed shuffled loop 16 times on the
same fd and validated bytes between passes. Across 31 randomized blocks
on CPUs 2–5 the first pass took 0.329 / 0.333 ms before / after; the median
of passes 2–16 took 0.154 / 0.155 ms. On CPU 2 alone the repeated medians
were again 0.154 / 0.155 ms. File pages being resident does not imply hot
CPU caches or identical first-pass costs. The gate still measures its
original single pass.

**Shared file.** On the baseline each hot-read phase issued 1,024 `GETATTR`
and **zero `READ`**. The pages were resident; Linux's `AUTO_INVAL_DATA` and
zero timeout still require fresh attributes before serving a buffered read.
Baseline host `GETATTR` service totaled 0.37 ms, not the ~28 ms the guest
spent in the loop. The two write phases each sent 1,024 `WRITE`, one
`GETXATTR` and one `FSYNC`; host WRITE service totaled 2.36–2.38 ms and the
sync took 4.12–4.60 ms. Service clocks, whole-guest clocks and KVM exit
counts come from separate runs; they are not additive.

Nine randomized blocks isolated the two mechanisms with the **same
experimental firmware and runner**. CPU affinity 2–5; guest ms:

| Workload | Neither | IRQ take only | Inline GETATTR only | Both |
|---|---:|---:|---:|---:|
| Warm sequential read | 27.42 | 23.89 | 12.79 | 12.53 |
| Sequential write + fsync | 43.32 | 37.78 | 43.62 | 37.85 |
| Shuffled overwrite + fsync | 34.69 | 29.33 | 35.55 | 28.92 |

On **one host CPU shared by the worker and vCPU**, neither → both was
42.09 → 12.52 ms for the warm read and 67.60 → 63.22 ms for sequential
write+sync. There is no spin/sleep delay. Separate `KVM_GET_STATS_FD`
observations (two guest handshakes around the phase, no profiling during
timings) measured baseline → final:

| Phase | Total exits | Userspace MMIO exits | Halt exits |
|---|---:|---:|---:|
| Warm sequential read | 3,480 → 1,049 | 2,056 → 1,032 | 369 → 2 |
| Sequential write + fsync | 5,183 → 4,156 | 2,060 → 1,034 | 1,021 → 1,019 |

The inline path eliminates the worker round trip, **not** the check. The
IRQ path removes one acknowledgement access, **not** the write reply.
For shipping behavior, the vendor ID follows the bit-22 offer, as in old
snapshots. Thus `PVISOR_VM_FS_IRQ_READ_ACK=0` selects the standard transport
for Linux; the inline-only experimental arm is not a shipping configuration.
For a reproducible three-way ablation use neither, default with
`PVISOR_VM_FS_INLINE_METADATA=0`, and default. Set these in the host
environment of `vm_api_case` directly; the Criterion gate sanitizes it.

## Explicit publication: eight files as one new directory

The [standalone diagnostic](../vm/staged_tree.c) creates 8 × 512 KiB files as
UID 1000, with 4,096 total 1 KiB writes. Direct and staged writers use the
same caller-owned output parent, fresh per VM. The direct arm builds in a
pending shared directory. The staged arm builds on guest tmpfs, then copies
in 256 KiB chunks into a pending shared directory (16 output data writes).
Both sync each destination file and the pending directory, publish it as
`ready` with `renameat2(RENAME_NOREPLACE)`, then sync the output parent.
The timer includes preparation through that final sync. This is a new name
on one filesystem, not a transaction across arbitrary existing paths.

| Final Linux defaults, 15 randomized pairs | Median ms |
|---|---:|
| Small writes to pending shared tree, then publish | 132.80 |
| Small writes locally, bulk export, then publish | 26.36 |

The host independently checked every filename, size and byte. Separate
untimed runs stopped at `generated`, `prepared` and `renamed`. Killing the VM
before rename left `ready` absent (pending data may need cleanup); killing
it after rename left a complete `ready`. A conflicting `ready` survived with
its original contents (`EEXIST`). Limiting the runner's file size to 64 KiB
failed export without publishing `ready`. A VM kill is **not** a host power
failure: if parent sync fails after rename, the new name is visible but
durability is not confirmed. The pending directory is not hidden from other
host processes; atomicity applies to the published name.

To reproduce, compile `staged_tree.c` with `cc -O2 -static -Wall -Wextra
-Werror`, install it as `/probe` in a fresh disposable root with `/dev` and
an `/output` writable by guest UID 1000, and run `vm_api_case --firmware
"$FW" --rootfs "$ROOT" --cpus 1 --memory 256 --task-stdio -- /probe local`
(or `shared`). `observe` as a second probe argument adds phase handshakes:
read one line, inspect the host directory, then write a newline to continue.
On the host, expected byte `k` of `ready/part-i` is `(31*i + k/1024) % 256`
with integer division. Check exactly eight files of 524,288 bytes each.

Keep this boundary explicit: producer memory is volatile, the observer
does not see incremental updates at `ready`, and export errors surface at
publication. No transparent delayed acknowledgements on ordinary `:rw`.
The pre-existing guest-owned overlay writeback is a different mechanism:
its KVM full/mapped/concurrent case and injected backing-error case also
passed; they do not make ordinary mutable passthrough exclusive.

## Compatibility checks

- Host rewrite with changed mtime, growth, truncation and rename/recreate
  across still-open descriptors: passed with 1 and 2 guest CPUs. The
  hardware check also reads negotiated bits 22/23 in guest sysfs. Profiled
  inline completions: 110 (default 1 CPU), 0 (inline disabled), 0 (2 CPUs),
  0 (standard transport). Old firmware/new host and new firmware/old host
  both passed the visibility probe with 0 inline completions.
- An actual short host write followed by `EFBIG` preserved bytes, file
  position and sync results with both old and new Linux paths.
- Fresh-process KVM captures/restores exercised old firmware with legacy
  single-/four-queue offers and the new default, restoring onto a runner
  configured for three workers. The test used an open seekable fd, a live
  thread, and a heap checked after resume. These were freshly generated
  matching-layout snapshots, not a test of every historical artifact or a
  cross-firmware restore. Changing the host tree before resume was rejected.
- Linux x86_64 firmware rebuilt offline; both changed driver objects
  cross-compiled for arm64. No ARM hardware or power-loss test.
