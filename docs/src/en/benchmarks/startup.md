# How long does a usable environment take to start?

## Conclusions {#conclusions}

**With Linux/KVM, prepared userspace and 2 vCPU / 128 MiB, pVisor VM produces first output in 100.91 ms, ahead of Firecracker (285.04 ms), QEMU microvm (321.56 ms) and QEMU q35 (702.98 ms) using the original distribution kernel from `/boot`.**

| Need | Selection implication |
| --- | --- |
| Frequently created disposable VMs | pVisor's dedicated firmware and startup path reduce measured first-command waiting |
| Original distribution kernel | Compare actual stock Firecracker/QEMU readiness costs |
| Complete Agent tool tasks | Consult tool and repair results; startup advantages do not establish complete-task performance |

## Motivation {#motivation}

Disposable environments repeatedly pay for the first command. Distribution kernels and dedicated firmware have different initialization costs, so selection requires waiting times for the actual deployment configuration. Short tasks also pay for exit and tools.

## Experiment design {#interpretation}

Linux x86_64 / KVM, AMD Ryzen 7 9700X, Fedora host kernel 7.2.8-200.fc44.x86_64. All four launch trees are pinned to CPUs 0,1; every VM has 2 vCPU / 128 MiB. They share prepared rootfs contents and the `/bin/sh` output probe, with a fresh VM and private workspace/disk per trial; no snapshot or environment pool. Warm page caches, three warmups and sixty measured samples per backend, randomized interleaving with seed 20261007. Environment preparation, kernel extraction, initrd construction and input copying are outside timing; `prepare_ms` is retained separately.

Firecracker 1.13.1 PCI uses the original ELF extracted from `/boot/vmlinuz-7.2.8-200.fc44.x86_64`; QEMU 10.2.2 q35/microvm use that file's original vmlinuz bytes. Matching config and RPM payload digests are checked. Kernel, extractor, config, initrd and source metadata are frozen and checked before/after execution; no kernel rebuild. All three share a minimal initrd that loads the original distribution `virtio_mmio` module and enters the common ext4 userspace. Microvm disables ACPI, option ROMs, PIT and PIC while retaining RTC for stock-driver initialization.

pVisor uses a frozen GNU release CLI, libkrunfw 5.6.2 dedicated Linux 6.12.109 firmware and staged virtio-fs. Kernel versions, devices, PID1 and storage semantics differ: the comparison covers each complete startup configuration, without attributing the entire difference to the VMM or a particular firmware optimization. This shell probe does not cover full Fedora/systemd/SSH, cold images, real Agent CLI initialization, concurrent capacity or snapshot restoration.

Ready ends at the checked first shell marker. pVisor/QEMU must produce the correct result and exit normally with code zero; Exit includes teardown. Firecracker uses ready-only: after unique ordered Ready/Result/Exit0 and no panic, the owned process receives controlled SIGTERM. Only Ready is reported for FC, without normal-shutdown timing. Failures, panic, mismatched output or changed artifacts are rejected; valid slow samples are retained without speed-based exclusions.

## Data and analysis {#results}

### Original distribution kernel controls {#reference-startup}

<a id="reference-exit"></a>

Measured on 2026-10-07: 240/240 valid samples from one cohort, zero measured failures. Unit ms, N=60 per backend; P95 describes the distribution rather than guaranteeing stable tail latency. No group triggered the predeclared separated-distribution rule.

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| pVisor VM | 60 / 0 | 100.91 | 151.27 | 141.52 | 176.59 |
| Firecracker PCI / stock | 60 / 0 | 285.04 | 291.88 | — | — |
| QEMU q35 / stock | 60 / 0 | 702.98 | 789.36 | 725.55 | 818.85 |
| QEMU microvm / stock | 60 / 0 | 321.56 | 404.39 | 347.99 | 428.27 |

Blank FC Exit cells mean normal shutdown is unmeasured and cannot be ranked against other Exit values.

| Control | Wait saved by pVisor ms | 95% interval for difference ms | Wait reduction | Control time / pVisor time |
| --- | --- | --- | --- | --- |
| Firecracker PCI / stock | 184.12 | 181.60–185.84 | 64.6% | 2.82× |
| QEMU q35 / stock | 602.06 | 582.17–635.99 | 85.6% | 6.97× |
| QEMU microvm / stock | 220.64 | 195.91–251.74 | 68.6% | 3.19× |

Differences are differences between the two medians, with 95% intervals from 5,000 bootstrap resamples of matching rounds. All three intervals exclude zero, supporting faster pVisor startup for these configurations. Frequently created short-lived VMs save the first-command waiting time shown above. Dedicated firmware and simplified initialization provide a shorter startup path; this comparison cannot separately quantify kernel, initrd, filesystem and VMM contributions.

### Separate custom-reference kernel cohort {#legacy-startup}

The following independent cohort was measured on 2026-10-06, N=60 per backend with zero measured failures. Firecracker is legacy reference/unknown and QEMU uses custom reference kernels, rather than stock `/boot` kernels. Userspace and artifacts differ; results are neither pooled with the stock cohort nor used for cross-cohort percentages. pVisor VM trails lightweight reference Firecracker/microvm here, showing that the advantage depends on the kernel and complete configuration.

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.73 | 109.17 | 136.65 | 156.89 |
| Docker rootless / overlay2 | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker PCI / legacy reference | 60 / 0 | 72.61 | 74.53 | 95.98 | 104.27 |
| QEMU q35 / custom reference | 60 / 0 | 213.07 | 217.77 | 237.52 | 245.37 |
| QEMU microvm / custom reference | 60 / 0 | 86.57 | 95.30 | 109.52 | 119.96 |

### Full distributions and macOS {#full-ubuntu}

<a id="macos"></a>

Full Ubuntu cold boot and Apple Silicon/HVF have no new matched comparison; the lightweight shell probe cannot replace deployment measurements.

### Downloads and reproduction {#run}

[Stock startup statistics](startup-stock.csv) · [Stock differences and 95% intervals](startup-stock-comparisons.csv) · [Stock source and artifact provenance](startup-stock-provenance.csv) · [Custom-reference cohort](startup.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#explicit-firecracker-kernel-controls). Binaries, complete inputs and raw logs stay in local ignored `.data/`; the provenance CSV retains cohort, CPU/RAM configuration and report/artifact digests.
