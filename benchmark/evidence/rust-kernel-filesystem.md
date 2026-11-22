# Rust guest kernel: filesystem and boot diagnostics on x86_64 KVM

Engineering only (B-FS-ENG, B-STARTUP-DIAG). Ryzen 7 9700X, Fedora
`7.3.3-300.fc45.x86_64`. Linux is the default firmware. Both Rust builds
include the same console, boot ID and guest supervisor fixes; only the
candidate honors `quiet`. Neither build adds named task stdio.

## Filesystem

The `quiet` flag was already on the VM command line. The Rust baseline still
logged syscalls and faults synchronously to its consoles. The candidate keeps
warnings and errors; `loglevel=debug` after `quiet` restores verbose logging.
The data path, fsync, checked closes and byte validation were not relaxed.

We ran two [B-FS-TOOLS mechanisms](../filesystem/workloads_v2_guest.rs) in
fresh 1-vCPU, 512 MiB VMs under CPU affinity 0,1. Five seeded randomized
blocks included Linux, Rust baseline and Rust candidate. No cgroup quota
or cold-cache claim. Each value below is the sum of the three guest
`completed_ns` intervals in one VM, converted to ms; the three rounds are
not independent samples. The read case includes full pattern validation.
The write case includes final file and directory sync. A zero exit, expected
three round records, DONE and a separate host `--verify` were required.

| Case | Firmware | Five observations (ms, block order) | Median (ms) |
|---|---|---|---:|
| M04: three sequential passes per round, 4 MiB file, 64 KiB reads | Linux | 88, 54, 60, 53, 59 | 59 |
| M04 | Rust baseline | 6620, 5373, 5258, 5383, 5651 | 5383 |
| M04 | Rust candidate | 573, 305, 272, 265, 288 | 288 |
| M07: 4 MiB per round in 1 MiB writes, final sync | Linux | 64, 53, 44, 182, 161 | 64 |
| M07 | Rust baseline | 6586, 6612, 7123, 6851, 6340 | 6612 |
| M07 | Rust candidate | 76, 33, 262, 91, 148 | 91 |

Candidate minus Rust baseline: paired median differences **−5117 ms**
(95% interval −6046 to −4986) for M04 and **−6579 ms** (−6861 to −6192)
for M07. Against Linux, M04 was **+230 ms** (+212 to +485); M07 was
**−13 ms** (−91 to +218), with no resolved difference. These are percentile
intervals over all $5^5$ bootstrap resamples of the five paired differences,
not simultaneous intervals or tail estimates. The experiment measures the
cost of verbose instrumentation, not the performance ceiling of virtio-fs.

### Demand-read batching

A separate campaign compared the `quiet` kernel with and without the page-cache
change. On a multi-page cache miss the old buffered read waited for each 4 KiB
page. The new path submits a run of *absent, demanded* pages through the existing
multi-page backend operation. It stops at a resident page or the requested
end, holds no page-array lock during I/O, and drains completions on error.
It does not prefetch past the requested range, extend attribute TTLs or
change writeback. Five more randomized blocks used the same M04 and M07
protocol. Raw records are in `benchmark/filesystem/.data/fw-batch-pair/`.

| Case | Linux (ms) | Rust quiet (ms) | Rust quiet + batching (ms) | Paired change vs quiet, median [95% interval] (ms) |
|---|---:|---:|---:|---:|
| M04 sequential read | 50.35 | 251.68 | 66.21 | −186.18 [−190.45, −185.27] |
| M07 large write + sync | 62.80 | 41.39 | 44.00 | +1.23 [−4.79, +4.18] |

Relative to Linux in this batch, paired differences were +15.47 ms
[+12.23, +17.13] on reads and −18.80 ms [−27.88, −10.72] on writes.
The write result does not establish a write-path improvement from read
batching. The two batches have different absolute levels; do not divide
medians across them. Both arms requested 512 MiB; the payload reported
528444 KiB `MemTotal` on Linux versus 654964 KiB on Rust. Those guest
counters do not establish equal available memory. Neither clock measures
storage-device bandwidth independently of validation, caching and VM I/O.

### End-to-end filesystem workflows

The final firmware pair ran all three published tasks with 1 vCPU/512 MiB,
CPU affinity 0,1, cloned prepared toolroot, a fresh root per environment and
three rounds per environment. In each of three blocks the order of cases
and kernels was randomized. Both used the final probe; all 18 VMs exited
normally and passed host validation. Task clocks exclude initial preparation
and final validation. Per-case medians below are for the three-round total;
paired differences use the same bootstrap method with three blocks (the
interval endpoints are simply the smallest and largest pair difference).

| Task | Linux (s) | Rust batched (s) | Rust minus Linux, paired median [95% interval] (s) |
|---|---:|---:|---:|
| S01 repository inspection | 5.520 | 4.317 | −0.989 [−1.204, −0.952] |
| S02 incremental compilation | 1.077 | 1.828 | +0.692 [+0.682, +0.781] |
| S03 unpack and publish | 19.181 | 25.153 | +6.524 [+0.666, +6.673] |

Local launch scripts, guest records, errors and host oracles are under
`benchmark/filesystem/.data/fw-workflows-final/`. OSDK test compilation
overlapped the beginning of this exploratory batch; another VM group was
started near the end. No interference gate or dedicated parent quota was
applied. These are not publication-grade comparisons.

An earlier Linux-only workflow check used the archived toolroot and payload in
three fresh 2-vCPU/512 MiB VMs per case. Median three-round totals:
repository inspection **10.813 s**, incremental build **2.503 s**, unpack
and publish **36.637 s**. An initial incremental-build preflight could not
find `ld`; the corrected launch exported `PATH=/usr/bin:/bin`. These were
direct VM API runs, not the documented nine-runtime campaign with a private
Docker daemon, namespaces and a shared 2 GiB budget. Do not compare the
absolute levels with the user-facing table as a regression estimate.

## Reproduction and evidence

Use the [firmware stage probe](../startup/README.md#linux-and-rust-firmware-stage-diagnostic-kvm)
with a new output per batch. In five pairs, median host configuration to console
acknowledgement was 47.08 ms (Linux) and 36.89 ms (Rust). Spawn to the verified
shell file marker was 76.83 and 106.02 ms respectively. All ten shells exited
normally. The file poll is not the published CLI stdout Ready clock.
With the subsequent demand-read patch, an independent five-pair diagnostic
(`benchmark/startup/.data/fw-suite-final/`) observed 43.19 / 35.50 ms to
console acknowledgement and 71.02 / 99.76 ms to the shell file marker
(Linux / Rust). Both had five normal exits. These are not same-batch
estimates of the patch's effect on boot.

For a filesystem cell, copy the prepared shell tree, static `workloads_v2_guest`
as `/subject`, and the frozen fixture `read.bin` as `/input/read.bin` into a
fresh rootfs. Run the release `vm_api_case` probe under a 90 s host deadline:

```sh
PVISOR_NATIVE_KERNEL=rust taskset -c 0,1 "$probe" --rootfs "$root" \
  --cpus 1 --memory 512 -- /bin/sh -c \
  'exec /subject "$1" /output /input 3 512 >/result 2>/errors' sh M04
"$payload" --verify M04 "$root/output" "$root/input" 3 512
```

Use `M07` for writes; for the GNU Linux arm select `linux` and pass
`--firmware "$linux_fw"` before `--`. The `vm_api_case` build recipe is in
the startup guide.

| Frozen input | SHA-256 |
|---|---|
| Rust baseline kernel | `c3e81085775d31efb083962dc95d2a16991cd606251641fd7979b4ffbc3d5c22` |
| Rust candidate kernel | `5c582c2f41e16579e80afb8e72cdfba99617d8fe25a925880b754b556ee57c70` |
| Rust quiet + demand-read batching | `ccd3b30376ea6d2e28d1522fe16640063b258939849d83c34fd75dd4c2a781c5` |
| Linux firmware library (paired mechanisms and memory) | `6b8bf8378520b50ec2ce8ccc99c1a1b3722cb045bf585d7ac22881e3c9cc4bda` |
| Linux firmware library (archived workflow fixture) | `4291201e9d63335b35c37902b2d45b8f96bd7675bb53a56a597163f0623cf87e` |
| Static filesystem payload | `f1b6cae222648b3f53e380070537e1d30e8c2b7f3fb89e04dc2643569208d09e` |
| 4 MiB read fixture | `196738ff3f6c48e0f5c90ef97f4e3ae4460986986df09166c1f9b78486bfd000` |

Local raw records: `benchmark/{startup,filesystem}/.data/fw-comparison-final/`;
Linux workflows: `benchmark/filesystem/.data/{fw-vm-subset,fw-vm-path}/`.
Each file case retains the invocation, runner digest, output, per-round
clocks and host verification. No public benchmark numbers were updated.

## Four-VM memory preflight (sharing disabled)

One fresh group per firmware, Linux then Rust (`quiet` candidate). Four
1-vCPU/256 MiB VMs each ran the same static `pvisor-memory-guest` with 64 MiB
`random-shared`. Each group ran under its own user service: CPU affinity
0–3, 400% quota, 2 GiB memory limit, zero swap. The observer was outside.
Guest shell timers fed the worker commands instead of named stdio. At 60 s
after all four had reported ready, none had begun readback. All four then
completed full readback, 25% and 100% private writes and checked exit;
all phase digests matched a separate host invocation. No OOMs.

| Phase | Linux group charged / runner PSS (MiB) | Rust group charged / runner PSS (MiB) |
|---|---:|---:|
| All ready | 425.3 / 411.7 | 389.4 / 381.5 |
| Idle 60 s | 417.6 / 411.8 | 383.9 / 381.5 |
| After 100% writes | 432.8 / 412.9 | 399.7 / 385.6 |

Charged is the service's `memory.current`; PSS sums the four VMM processes'
`smaps_rollup`. These are phase observations, not peak or marginal VM costs.
The 60 s difference was 33.7 MiB for this group. N=1, fixed order and no
sharing treatments: this is not a density or deduplication result. KSM was
disabled on the host. The registered matrix with pools, Docker, Firecracker
and shared libraries was not run. Raw evidence, including argv, guest output
and cgroup events: `benchmark/memory/.data/fw-off-preflight/`.

The final batched kernel was checked in a new cohort, Rust first, then Linux.
All four guests in each group passed the same integrity and exit gates. At
60 s the Rust group charged **378.4 MiB** versus **408.2 MiB** for Linux;
summed runner PSS was **381.3 vs 411.7 MiB**. After full private writes,
group charges were **394.0 vs 419.3 MiB**. Raw records and the launcher:
`benchmark/memory/.data/fw-off-batched/`. Do not combine these distinct
kernel builds as repeated samples or infer a memory saving from read batching.

## Validation scope

The final x86_64 KVM probes on both kernels verified per-boot UUID format
and stability, mixed warm/cold unaligned reads, short reads at EOF,
shared-rootfs writes, rejection of forged exit reports and exit code 7.
The kernel builder tests passed (11), as did the guest suite (75) and
targeted VM console, selection and profiling cases (14). The OSDK
page-cache ktest image compiled but the QEMU runner timed out without a
test result: its concurrency/failure-injection assertions are **not**
confirmed by the successful hardware workload. ARM KVM/HVF were not run.
