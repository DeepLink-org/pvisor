# Rust native CPU-runtime deadlines

Engineering diagnostic for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
Can the native guest stop the remaining 1 kHz tick without advancing
CPU-time timers while the host has descheduled its vCPU?

## Correctness on KVM

The single-vCPU fixture verified fair and round-robin preemption,
wall-clock alarms, process virtual/profiling timers, and a thread CPU-time
timer. The full tickless configuration also passed six idle-handoff runs.
With a busy host process sharing the same logical CPU, the guest's 120 ms
virtual and profiling timers were observed after 240.45 ms and 238.98 ms
of wall time, respectively, but at 120.21 ms and 120.05 ms of charged
process CPU time. These are functional observations, not timing bounds.
KVM steal time covers host runqueue delay, not all time spent outside
guest execution, such as voluntary busy-vCPU pause or VMM exit handling.

## Initial same-artifact comparison

All 16 randomized pairs per workload completed on a Ryzen 7 9700X,
Linux `7.3.3-300.fc45.x86_64`, host affinity `2-5`, 1 vCPU/256 MiB. The
runner and guest image are identical in both arms. Both use wall and
preemption deadlines; only the tickless arm enables interval accounting
and disables the compatibility tick. Neither enables idle handoff.

| Workload | Compatibility | Tickless | Paired median change, 95% interval |
| --- | ---: | ---: | ---: |
| Hardware IRQs in a 150 ms busy loop | 153 | 3 | −98.04% [−98.04%, −98.04%] |
| 200 µs timerfd median lateness (µs) | 10.32 | 8.69 | −15.56% [−26.21%, −11.94%] |
| Spaced warm reads (ms, waits excluded) | 4.224 | 4.214 | −0.66% [−4.07%, +2.45%] |
| 8 million integer iterations (ms) | 8.873 | 8.824 | −0.54% [−0.85%, −0.19%] |
| 131,072 `getpid` calls (ms) | 9.546 | 14.984 | +59.15% [+56.00%, +71.04%] |
| 4,096 pipe round trips (ms) | 11.042 | 74.220 | +567.75% [+555.55%, +584.54%] |

Intervals resample whole blocks (10,000 draws). The pipe workload's
whole-runner CPU also increased from 82.71 to 144.33 ms, so the elapsed
slowdown is not explained by waiting alone. Disabling the tick works, but
the measured context-switch and syscall costs rule out enabling this
implementation by default.

Raw logs, frozen binary digests, and the batch plan are in
`benchmark/vm/.data/cpu-runtime.rNMr2K/`.

The unchanged standard gate against `530fba64` ran all 45 cells per cohort
and **failed (27/30 product checks passed)** on this build. The failures
were Linux `tmpfs-read` (+54.76%), Linux `tmpfs-shuffled-read` (+34.88%),
and Rust native `memory-copy` (+28.39%). The gate does not enable either
experimental deadline profile, so these results do not measure the
opt-in syscall or pipe costs above.

## Retain the earlier APIC interrupt

The pipe alternates between waking its peer (which can arm a preemption
deadline) and blocking before the slice expires. The compatibility tick
often supplied an earlier interrupt and hid this timer programming cost.
The follow-up keeps an already programmed earlier interrupt when a
preemption deadline moves later or is removed, and rechecks all current
deadlines when it fires. An earlier new deadline still programs the APIC
immediately. Wall and CPU-timer changes retain their previous behavior.

Another 16 same-build pairs per workload gave these results. This is a
separate batch; its relative gaps are measured against its own control.

| Workload | Compatibility | Tickless | Paired median change, 95% interval |
| --- | ---: | ---: | ---: |
| Hardware IRQs in a 150 ms busy loop | 152 | 3 | −98.04% [−98.36%, −98.03%] |
| 200 µs timerfd median lateness (µs) | 10.21 | 8.87 | −11.77% [−19.74%, −3.35%] |
| Spaced warm reads (ms, waits excluded) | 4.179 | 4.306 | +2.47% [−2.36%, +7.18%] |
| 8 million integer iterations (ms) | 8.867 | 8.813 | −0.74% [−1.44%, −0.06%] |
| 131,072 `getpid` calls (ms) | 9.874 | 15.006 | +52.92% [+38.45%, +59.87%] |
| 4,096 pipe round trips (ms) | 11.111 | 12.693 | +14.55% [+12.28%, +15.72%] |

The large pipe penalty did not recur after the APIC change; the remaining
interval still crosses the 15% threshold. This experiment does not count
APIC register writes. The `getpid` cost remains the clearest blocker:
each round trip still samples the KVM-corrected clock twice. The standard
gate above was not rerun on the follow-up, which only changes the
tickless preemption path. Follow-up artifacts and samples are in
`benchmark/vm/.data/cpu-runtime-apic.5bU5yS/`.

## Syscall boundary sampling

Both user/kernel transitions still record a timestamp. The optimized path
reads TSC between two checks of the KVM steal-time version without
serializing the whole guest instruction stream. Linux KVM publishes the
steal delta before resuming the vCPU; a sample crossing a publication
retries. User/kernel boundary accounting records the elapsed segment and
the next privilege level in one update. No syscall is exempt from
accounting.

The correctness probe drives `ITIMER_PROF` with repeated raw `getpid`
syscalls and checks that it charges both user and system time. With a host
busy loop pinned to the same logical CPU, the 120 ms virtual and profiling
timers reported 120.18 ms and 120.05 ms of charged process CPU time after
238.93 ms and 240.62 ms of wall time. The thread CPU-time timer and
competitive fair/RR scheduler probes also passed.

The follow-up compares four arms in each shuffled block: before/after
the sampler change, with/without CPU-runtime deadlines in each build.
Wall and scheduler deadlines stay enabled in all arms; idle handoff
stays off. `getpid` uses the unchanged gate probe (131,072 actual syscalls
with a PID check). Ryzen 7 9700X, Linux `7.3.3-300.fc45.x86_64`, host
affinity `2-5`, fresh 1-vCPU/256 MiB guests: 20 blocks per workload and
400 valid runs across the five workloads, with no exclusions.

| Workload | Tickless before (ms) | Tickless after (ms) | After vs. before, tickless paired change | After: tickless vs. compatibility, paired change |
| --- | ---: | ---: | ---: | ---: |
| 131,072 `getpid` calls | 14.873 | 11.028 | −25.72% [−26.94%, −24.78%] | +12.56% [+12.10%, +13.32%] |
| 4,096 pipe round trips | 12.439 | 12.016 | −3.63% [−4.88%, −2.54%] | +8.71% [+7.23%, +10.64%] |

Intervals use 10,000 bootstrap resamples of complete blocks. Before the
sampler change, `getpid` was +54.99% [+53.61%, +56.76%] over its own
compatibility arm. After the change, its same-binary reference took
9.818 ms. Both measured hot paths are below 15% in this diagnostic; the
unchanged standard gate was not rerun. In the same follow-up the busy-loop
interrupt median was still 3 with tickless enabled versus 153 in
compatibility mode. Logs, digests, order, and per-VM samples are in
`benchmark/vm/.data/cpu-runtime-getpid.pdANHg/`.
