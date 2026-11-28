# Rust native idle acquisition on KVM

Engineering diagnostic for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng),
independent of the guest regression gate. It asks whether a host arm interrupt
shortens the acquisition of an already-idle guest and how much CPU the runner
uses before acquisition. It does not measure task throughput or RAM reclaim.

## Design

Each run starts a 1-vCPU, 256 MiB Rust native VM on Linux x86_64/KVM and waits
for the static guest's `BLOCK` marker. After a 100 ms settling period, the
controller samples whole-runner CPU time across a one-second unarmed wait.
After a seeded 0–49 ms phase offset, it arms the lease and times through the
pause acknowledgement. The guest must read the expected input byte after
resume and exit with the authenticated status `7`.

The three arms are periodic tick (handoff disabled), ABI v2 with the 50 ms
guest retry, and ABI v2 with the host LAPIC arm interrupt. The follow-up
uses one binary and the same embedded guest for all three arms; the retry
arm sets `PVISOR_DIAG_NATIVE_IDLE_WAKE=0` to withhold the interrupt extension.
The periodic control has no arm-to-pause sample. CPU time sums all runner threads' `/proc` `schedstat`
runtime, not only the vCPU. The timing excludes boot, host arm phase offset
from the latency interval, and input delivery/exit from both intervals.

The [KVM fixture](../../crates/pvisor-vm/tests/native_idle_handoff.rs) runs
the short diagnostic with `PVISOR_CASE_IDLE_DIAGNOSTIC` set; use `periodic`
to disable handoff or set it to the arm name otherwise. Pass the shared phase
offset in `PVISOR_CASE_IDLE_ARM_PHASE_MS`. The ordinary fixture separately
checks device wake, timer wake and resume-and-arm. Enable `PVISOR_DIAG_IDLE=1`
only in the untimed check to distinguish an injected `native_arm_irq` from
the retry fallback.

## Results and limits

The fixed build completed 12 randomized three-arm blocks after two warm-up
blocks (36 measured VMs and six warm-ups, all valid). AMD Ryzen 7 9700X,
Linux `7.3.3-300.fc45.x86_64`, host affinity `6,7`, non-dedicated host,
debug VMM. The negotiation preflight accepted four offers in each enabled
arm, including resume-and-arm; only the IRQ arm logged `native_arm_irq`.

| Condition | Runner CPU per 1 s idle, median (min–max), ms | Arm → pause ACK, median (min–max), ms |
| --- | ---: | ---: |
| 1 kHz periodic tick | 12.39 (10.12–13.14) | — |
| One-shot + 50 ms retry | 4.41 (3.57–4.81) | 15.04 (0.17–37.69) |
| One-shot + host arm IRQ | 3.58 (2.86–3.88) | 0.133 (0.115–1.164) |

Median within-block changes (10,000 paired bootstrap draws, 95% intervals):
IRQ versus periodic CPU −71.3% [−72.4%, −69.7%]; IRQ versus retry CPU
−17.7% [−25.6%, −14.2%]; IRQ versus retry arm-to-pause −99.2% [−99.5%,
−95.9%]. IRQ was lower in all 12 pairs for each comparison. The incremental
CPU saving over retry is small in absolute terms (about 0.8 CPU-ms per
second from the arm medians). Arm-to-pause is not wake-to-tool latency,
and one idle VM does not establish concurrent capacity or Agent throughput.
No tail percentile or RAM-reclaim result is reported.

The fixed build also passed the full KVM fixture 12/12 with IRQ and 12/12
with retry. Frozen executable and kernel hashes, source patch, randomized
plan, all logs, CSV and summary are in `target/native-idle-fixed.J6duCt/`.

### Earlier incomplete batches

**Earlier batches did not complete.** Two longer three-arm batches and a
shortened three-arm batch stopped on 20-second timeouts in the retry arm.
An independent periodic-versus-IRQ batch also stopped on a timeout in the
IRQ arm. The next debug run reproduced the failure before the guest `BLOCK`
marker: when a reschedule was pending at idle entry, the guest could invoke
the scheduler with local interrupts disabled and panic. `halt_cpu_until`
now releases the IRQ guard before scheduling on that branch. No samples from
the incomplete batches were pooled with the fixed build.

The earlier artifacts, raw samples and failed logs are in
`target/native-idle-ab.vYeMZV/` and `target/native-idle-ab.iGSWuv/`. The
guest panic is recorded in `target/native-idle-check.o2b9J1/diagnostic-stress.log`.
