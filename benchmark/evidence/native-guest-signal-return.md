# Rust guest: the empty signal queues on syscall return

Engineering diagnostic for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
Does consulting the signal dispositions of a process with no queued signals
cost enough to matter, particularly when multiple threads share that process?

## What changed

Before taking the two disposition locks,
[`dequeue_pending_signal`](../../pvisor-kernel/kernel/core/src/process/signal/mod.rs)
checks the existing thread and process queue counts. Both must be empty to
skip taking the locks. A signal arriving after the check remains queued; the
sender's wakeup and subsequent kernel-event checks are unchanged. Nonempty
queues still go through masked/ignored signal selection and ptrace handling
as before. No new state is published and no mask or disposition is cached.
The test is inside dequeue: `ERESTARTSYS` conversion, mask restoration and the
second dequeue after mask restoration still run in their original order.

## Guest timings

Linux x86_64/KVM, Ryzen 7 9700X, host CPUs 2–5, fresh VM and writable
virtio-fs root per operation, 256 MiB guest RAM. Identical host VM source
and guest workload binaries. Only the two signal module files differ in
the kernel sources. Baseline is the preceding directory-cache build on
`9f251505` (kernel SHA-256 `528f49442ed8`); candidate is `069d4a99e3b0`.
Each runner was checked for the packed bytes of its intended kernel.

| Workload | Before, ms | After, ms | Paired change, ms (95% interval) |
|---|---:|---:|---:|
| 131,072 `SYS_getpid`, 1 thread / 1 vCPU | 9.63 | 7.88 | −1.63 [−1.86, −1.54] |
| 4,096 two-process pipe round trips, 1 vCPU | 10.42 | 10.04 | −0.37 [−0.81, −0.24] |
| 131,072 `SYS_getpid` per thread, 4 threads / 4 vCPUs | 72.60 | 10.73 | −61.24 [−64.28, −59.86] |

The first two rows use the unchanged [VM gate probe](../vm/probe.c). The
fan-out diagnostic starts four pthreads in one process, releases them from
a barrier, and checks every returned PID. The clock spans the earliest
worker start to the latest finish, not the sum of four thread times. Guest
scheduling is unrestricted. An earlier fixed-affinity preflight failed on
the baseline: its scheduler does not migrate a running thread immediately
on `sched_setaffinity`. No fixed-affinity timing is used here.

Guest monotonic time excludes boot and exit. For each of 30 blocks the
case order and the order of the two arms within each case were shuffled
(seed 21302). One preflight block was excluded by design, no measured
observations discarded. Columns are arm medians; the change is the median
of paired differences with a percentile bootstrap interval (10,000
resamples, seed 2203). This is a shared host and a focused diagnostic, not
the full Criterion three-backend regression gate. The four-thread loop
intentionally stresses a shared lock; it is not an application speedup.

**Real tool check.** In 20 separate randomized pairs we ran the same
[three-pass Git fixture](native-guest-wide-directory.md#follow-up-repeated-scans-on-9f251505):
2 vCPUs / 128 MiB, host CPUs 0,1,6,7, unmodified `git status --porcelain=v1`,
2,048 tracked files, one modified file and one untracked file. All three
guest outputs and the host's final status matched. The three-command total
was 1,539.1 → 1,557.6 ms, paired +24.6 ms [−12.0, +45.1]; rounds 2+3
were 396.4 → 400.8 ms, paired +3.5 ms [−7.5, +15.5]. Neither interval
resolves an improvement. Raw observations: `target/signal-return/git-status/paired/rows.json`.

## Correctness and reproduction

The [guest probe](../../pvisor-kernel/tests/signal_return.c) checks a `ppoll`
timeout with empty queues, a pending signal unblocked by `sigsuspend`, and
the restored mask in each case. A parent waits until the child is sleeping
in a pipe read before sending a signal; it checks both `EINTR` and successful
read restart. Another thread alternates process- and thread-directed signals
while the receiver issues syscalls, acknowledging each delivery so ordinary
signal coalescing cannot hide a loss. Run with static glibc and a native-enabled
`vm_api_case`, as in the Linux KVM CI step. Both kernels passed ten fresh
VMs per configuration at 1, 2 and 4 vCPUs (64 MiB).

For the timed single-vCPU cases, build `benchmark/vm/probe.c` with
`cc -static -O2`, copy it to `/probe` in a disposable root and pass
`--cpus 1 --memory 256 --task-stdio -- /probe syscall-getpid` (or
`ipc-pipe-roundtrip`). Require exit 0 and the exact `VM_GUEST_OK` record.
Local raw data and workloads: `target/signal-return/{identities.json,paired/rows.json,fanout.c,pairs.py,semantic-kvm/}`.
