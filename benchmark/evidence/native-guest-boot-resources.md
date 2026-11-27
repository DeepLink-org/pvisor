# Rust native: AP startup and headless guest allocations

Engineering diagnostics for the native direct-start arm of
[B-VM-STARTUP-ENG](../vm/startup.md) and the large-I/O controls of
[B-FS-ENG](../REGISTRY.md#b-fs-eng). These are not the full CI gates.

**Conditions.** Ryzen 7 9700X, Linux 7.3.3-300.fc45.x86_64/KVM, affinity
2–5, one VM at a time, fresh writable virtio-fs root, no explicit NIC.
Release kernels in debug `vm_api_case` runners; same VMM and PID 1 sources.
The four cumulative arms are baseline `4ee9e3d2`, KVM AP startup only (AP),
AP plus headless console (VT), and both plus demand-allocated DMA (final).
Each block ran all arms for its comparison in shuffled order. No sample
exclusions or deliberate host cache drops. Brackets are percentile bootstrap
95% intervals for the median **within-block difference** (10,000 resamples,
seed 9281); these estimates need not equal differences of arm medians.

## Fresh VM to first command

Unmodified [startup probe](../vm/startup_probe.c) and
[Rust observer](../criterion/src/startup/observer.rs), 256 MiB, 15 blocks
per CPU count (order seed 4271). Time starts before spawn and stops at the
complete command-start stdout marker; the validation marker and successful
exit are required but not timed. No daemon warm pool or Firecracker arm.

| vCPU | Baseline (ms) | AP (ms) | Final (ms) | AP − baseline (ms) | Final − baseline (ms) |
|---:|---:|---:|---:|---:|---:|
| 1 | 88.2 | 81.7 | 79.4 | −7.2 [−17.2, −1.5] | −7.2 [−13.6, −4.4] |
| 2 | 132.8 | 93.3 | 92.5 | −43.0 [−47.9, −35.4] | −44.2 [−48.5, −34.0] |
| 4 | 140.9 | 98.5 | 96.2 | −36.5 [−45.6, −32.4] | −44.1 [−47.2, −24.3] |

Single-vCPU guests do not execute AP bring-up. The difference in that control
cannot be credited to removing the IPI delays. These are end-to-end clocks,
not direct measurements of time in the BSP spin loop.

## At the first command's stdin barrier

The [static payload](../../pvisor-kernel/tests/boot_memory.rs) reads guest
`/proc/meminfo` and waits. After its complete marker, the host reads runner
`smaps_rollup`, releases stdin with `!` and checks authenticated exit 0.
Seven blocks per layout. Root + share adds one empty read-only virtio-fs
device. Positive `MemFree` means more guest free memory; negative PSS means
less host memory resident in the runner at this barrier.

| vCPU / MiB / devices | VT − AP `MemFree` (KiB) | Final − VT `MemFree` (KiB) | VT − AP PSS (KiB) | Final − VT PSS (KiB) |
|---|---:|---:|---:|---:|
| 1 / 16 / root | +884 [884, 884] | +2,974 [2,974, 2,974] | −252 [−492, −229] | −911 [−957, −535] |
| 4 / 256 / root | +884 [864, 904] | +17,164 [17,124, 17,164] | −373 [−477, −336] | −764 [−918, −572] |
| 4 / 256 / root + share | +884 [864, 884] | +17,600 [17,592, 17,620] | −196 [−253, −187] | −1,257 [−1,518, −1,064] |

The final increment combines small fs pools, directional arenas and network
pools; it does not isolate those three changes. Deferred arenas can still
consume the original bounded allocation on large transfers. No multi-VM
density, peak RAM or lower admission limit is inferred.

## First large transfer and repeated I/O

[Filesystem worker](../filesystem/workloads_v2_guest.rs), 2 vCPU / 256 MiB,
12 shuffled VT/final pairs per case (order seed 511). Three rounds per VM;
each arm ran the separate host `--verify` byte oracle after guest exit.
M04 reads a 4 MiB file in 64 KiB calls, three passes/round. M07 writes 4 MiB
in 1 MiB calls/round and includes final sync and checked close.

| Case | VT three-round total (ms) | Final three-round total (ms) | Paired change (ms) | First round alone, paired change (ms) |
|---|---:|---:|---:|---:|
| M04 read | 108.2 | 105.6 | −4.0 [−7.7, +1.3] | −1.9 [−4.0, +0.9] |
| M07 write | 51.6 | 55.9 | +3.7 [−1.3, +18.4] | +0.2 [−1.3, +6.2] |

Neither comparison resolves a change in completed time. The write interval
does not rule out a meaningful slowdown; this is not a throughput guarantee.

## Reproduction and coverage

- Build and embed each kernel in `vm_api_case` with the
  [resource matrix recipe](../../pvisor-kernel/README.md#x86_64-vcpuram-matrix).
  The AP-only arm changes the two `ostd` SMP files; the VT arm additionally
  changes the two `core/src/device/tty` files. Use the same static payloads
  across arms. Raw kernel SHA-256 prefixes, respectively:
  `9af58acdcd21`, `a596bfb063ea`, `d064d5671f41`, `84a6017cc104`.
- Local raw trials: `target/native-ab/.data/`, including per-boot timing
  records, guest output, runner `smaps_rollup`, and `large-io/` round data.
  Diagnostic drivers and artifact receipts are under `target/native-ab/`.
- The final x86_64 image passed the 15-case CPU/RAM matrix three times,
  named stdin/stdout/stderr including immediate EOF, a PTY byte round-trip,
  shared-root writes, read-only secondary mount and authenticated exits.
  A separate Unix socket-backed virtio-net check received and answered 128
  ARP requests. ARM64 cross-built; no ARM KVM or macOS HVF execution.
