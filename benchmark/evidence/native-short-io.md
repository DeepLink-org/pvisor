# Rust native short waits and deadline scheduling

Engineering diagnostic for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
Does keeping an idle deadline armed across maintenance interrupts reduce the
cost of short waits, and do demand-driven worker dispatch and one-shot
preemption preserve interactive workloads?

## Design

Compare `b4212df0`, the idle-only change, and the combined idle, worker, and
scheduler changes. All three use the Rust native wall-clock deadline profile
with idle handoff disabled on a single-vCPU, 256 MiB KVM guest. Each block
shuffles the three executables on the same physical-core affinity mask.

- The [wall deadline probe](../../pvisor-kernel/tests/wall_deadline.c) measures
  how late userspace observes a 200 µs timerfd expiry. A separate guest busy
  loop verifies that the remaining CPU-accounting tick is still present.
- The [shared file probe](../vm/handoff_probe.c) sums the time for 256 warm
  `pread` calls with 1 ms of sleep between calls, excluding sleep. It verifies
  the bytes after each call. The gate's pipe and `getpid` probes exercise
  context switching and the syscall path, respectively.
- The [scheduler probe](../../pvisor-kernel/tests/deadline_scheduler.c)
  requires two runnable tasks to make progress under both fair and round-robin
  scheduling without voluntary yields during contention. It also checks the
  delivery of a wall-clock alarm to an idle process and a CPU-time alarm
  to a busy process; both alarm callbacks dispatch through a worker pool.

Runner CPU is the whole child lifetime, including boot and teardown. Guest
interrupt counts are not counts of APIC register writes. Raw probes, inputs,
arm order, per-run logs, and estimates are under
`benchmark/vm/.data/short-io.O8yxtV/`. The standard guest gate is independent
and continues to use its original default configuration and threshold.

## Results

16 shuffled blocks per workload, 240 valid VMs, no excluded runs. AMD Ryzen
7 9700X, Linux `7.3.3-300.fc45.x86_64`, CPUs `2-5`, shared btrfs host.
Intervals below are 95% intervals from 10,000 bootstrap resamples of whole
blocks. Time is the guest median across runs; changes are paired relative
changes against `b4212df0`, not ratios of the displayed medians.

| Workload | `b4212df0` | Idle only | Combined | Combined paired change |
| --- | ---: | ---: | ---: | ---: |
| Median lateness of 200 µs `timerfd` (µs) | 11.57 | 10.12 | 10.05 | −11.94% [−17.88%, −6.84%] |
| 256 spaced warm reads (ms, excluding sleep) | 4.497 | 4.467 | 4.510 | −0.19% [−2.51%, +7.62%] |
| 4,096 pipe round trips (ms) | 10.464 | 10.328 | 10.900 | +4.90% [+1.70%, +7.91%] |
| 131,072 `getpid` calls (ms) | 9.541 | 9.718 | 9.234 | −3.47% [−6.36%, +1.98%] |
| Delivery of a 30 ms `ITIMER_REAL` alarm (ms) | 49.78 | 50.07 | 30.13 | −39.52% [−39.65%, −39.13%] |
| Delivery of a 30 ms `ITIMER_VIRTUAL` alarm (ms) | 201.13 | 201.16 | 30.96 | −84.61% [−84.62%, −84.60%] |

The idle-only arm did not resolve a timerfd lateness improvement
(−12.63% [−17.97%, +16.69%]). No clear whole-runner CPU reduction was
resolved for timerfd or spaced reads. The `getpid` workload's whole-runner
CPU cost rose +8.25% [+2.06%, +13.15%] in the combined arm, which includes
boot and teardown and does not establish a per-syscall CPU cost. The pipe
slowdown is a measured cost in the enabled profile. The alarm probe shows
signal delivery no longer waiting for a quiet work queue's monitor poll;
CPU-time accounting itself still uses the 1 kHz tick.

The standard local VM guest gate against `530fba64` completed all 45 cells
per revision and **failed, 28/30 product checks passed**. Rust native
`tmpfs-read` (+19.63%) and `tmpfs-shuffled-overwrite` (+32.82%) exceeded
15%. Both warm reads passed. The standard run does not enable the
experimental deadline mode, so it does not test the pipe cost above.
The repository's follow-up ran 16 alternating pairs for each red cell:
`tmpfs-read` was −4.25% [−11.07%, +18.55%] and shuffled overwrite was
+3.04% [−13.13%, +34.97%]. Neither median reproduced the threshold
crossing, but both intervals still admit a 15% regression. The gate
remains failed.
