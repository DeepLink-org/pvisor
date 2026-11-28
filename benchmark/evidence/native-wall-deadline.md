# Rust native wall-clock deadlines on KVM

Engineering diagnostic for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
Does arming the earliest wall-clock deadline reduce short timer wake lateness
and polling interrupts without regressing the existing guest workload matrix?

## Short timer diagnostic

Use the [static guest probe](../../pvisor-kernel/tests/wall_deadline.c) with
the same release VM runner and embedded Rust native bundle in both modes.
Neither mode enables idle handoff. The probe checks deadline replacement,
cancellation, expiry while still runnable, periodic timerfd and futex timeouts.
It samples `/proc/stat` across a 250 ms sleep and a 150 ms busy loop. It also
runs 256 blocking waits in each VM against a 200 µs monotonic timerfd deadline
and reports the per-VM median excess wait; this includes time until userspace
can observe expiry, not just APIC interrupt delivery.

All 12 seeded AB/BA pairs completed in fresh VMs (1 vCPU, 256 MiB). CPU affinity
was `2,3` on a shared AMD Ryzen 7 9700X host, Linux `7.3.3-300.fc45.x86_64`.
No samples were excluded.

| Metric (median across VMs) | Periodic | Deadline | Paired median change, 95% bootstrap interval |
| --- | ---: | ---: | ---: |
| Hardware interrupts during 250 ms sleep | 258 | 16 | −241.5 [−243, −240] |
| Hardware interrupts during 150 ms busy loop | 151 | 153 | +2 [+2, +3] |
| Timer softirqs during 150 ms busy loop | 150 | 2 | −148 [−148, −147] |
| Per-VM median timerfd lateness (µs) | 803.48 | 11.93 | −791.03 [−792.79, −788.28] |

The interval resamples entire pairs (10,000 draws). The busy scheduling and
CPU accounting tick remains. These counts do not measure host CPU time,
real workload throughput, or system-wide tail latency.

## Guest regression gate

The unmodified 45-cell matrix and 15% threshold were run in three sequential
cohorts, all on `2,3` with the same Linux firmware and pinned Firecracker
reference. The main revision was `530fba64`. The candidate included the idle
handoff changes, explicit clocksource refresh and this wall-clock deadline
implementation. All three produced 45 valid cell estimates.

- **Standard gate, default candidate versus main: 17/30 pass.** Nine Linux
  and four Rust native cells exceeded the threshold. The candidate did not
  enable the deadline path. The identical Firecracker reference itself moved
  between −58.40% and +17.12% on individual cells during those cohorts.
  This fail is not evidence that enabling wall deadlines caused the red cells.
- **Opt-in matrix, deadline versus default on the same candidate: 30/30 pass.**
  The wrapper enables the Rust native option after the harness clears
  inherited `PVISOR_*` variables; it does not activate it for Linux.
  The largest Rust native increase was shuffled disk read, +9.47%.
  Both arms use the same executable, but they were measured sequentially and
  the unchanged Linux control moved substantially (e.g. memory copy −44.44%).
  This is not a throughput speedup claim or a replacement for the failed gate.

The repository's follow-up diagnostic completed 16 alternating pairs for each
of the 13 red cells. None had a paired median slowdown above 15%. Rust native
sequential write plus `fsync` went from +301.24% in the gate to −8.80%
[−18.09%, +10.57%] in the paired diagnostic. Not all intervals rule out a 15%
slowdown; for example, Rust native tmpfs shuffled overwrite was +0.75%
[−15.57%, +21.78%]. The original failed gate still stands. Calibrate with a
same-artifact A/A on a dedicated KVM runner before making a release decision.

Local raw evidence: `benchmark/vm/.data/wall-deadline.RdHptY/`, including the
pair order and samples, both Criterion reports, paired gate diagnostics,
artifact digests and host load observations. The APIC path was tested on
this KVM host; ARM64 was cross-built, not run.
