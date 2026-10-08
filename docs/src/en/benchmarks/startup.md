# How long does a usable environment take to start?

## Conclusions {#conclusions}

**With prepared environments, pVisor host and staged produce first output in 12.72 ms and 25.13 ms respectively. With Linux/KVM and 2 vCPU / 128 MiB, pVisor VM takes 100.91 ms, ahead of Firecracker (285.04 ms), QEMU (702.98 ms) and microvm (321.56 ms) using the full Fedora kernel.**

| Need | Selection implication |
| --- | --- |
| Frequent short commands | Host and staged startup waits are in the tens of milliseconds |
| Frequently created disposable VMs | pVisor VM waits about 0.1 seconds for the first command |
| Complete Agent tool tasks | Use tool and repair results to choose an execution mode |

## Motivation {#motivation}

Disposable environments repeatedly pay for the first command. Choosing host, staged, containers or VMs requires knowing their startup waits and the cost of the isolation you need. Short tasks also pay for exit and tools.

## Experiment design {#interpretation}

Linux x86_64 / KVM, AMD Ryzen 7 9700X, Fedora host. The controls are native, pVisor host, pVisor staged, pVisor VM, rootless Docker / overlay2, Firecracker PCI, QEMU q35 and QEMU microvm. Launch trees are pinned to CPUs 0,1; every VM has 2 vCPU / 128 MiB. Processes and containers have no memory limit.

Firecracker, QEMU and microvm use the full Fedora distribution kernel, Linux 7.2.8-200.fc44.x86_64. pVisor's virtualization is based on libkrun and uses its companion libkrunfw project's customized kernel, Linux 6.12.109.

Prepared userspace environments run the same `/bin/sh` output probe. Each execution uses a fresh environment and private workspace; every VM boots afresh, without snapshots or environment pools. Warm page caches, three warmups and sixty measured samples per backend, with randomized interleaving within each measurement group. Environment preparation and input copying are outside startup timing. Kernel, device and storage paths differ, so results cover each complete startup configuration. This probe does not cover full distribution boot, cold images, Agent CLI initialization, concurrent capacity or snapshot restoration.

Ready ends at the checked first shell marker. Exit includes execution and teardown, requiring correct output and a normal zero exit. Firecracker is stopped under control after checking markers and results and confirming no panic; only Ready is reported. Failures, panic, mismatched output or changed artifacts are excluded from timing distributions; valid slow samples are retained.

## Data and analysis {#results}

### Startup and exit times {#reference-startup}

<a id="reference-exit"></a>
<a id="legacy-startup"></a>

Unit ms, N=60 per backend, zero measured failures. Process/container and VM measurements were collected separately, with each row retaining its own statistics; all four VM controls were randomly interleaved within one group. P95 describes the distribution rather than guaranteeing stable tail latency.

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.91 | 151.27 | 141.52 | 176.59 |
| Docker | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker | 60 / 0 | 285.04 | 291.88 | — | — |
| QEMU | 60 / 0 | 702.98 | 789.36 | 725.55 | 818.85 |
| microvm | 60 / 0 | 321.56 | 404.39 | 347.99 | 428.27 |

Host and staged wait tens of milliseconds, making them suitable for frequently launched short commands. VMs also initialize a kernel and devices; choose isolation with complete-task timing in mind. Blank Firecracker Exit cells mean normal shutdown is unmeasured.

| VM control | Wait saved by pVisor ms | 95% interval for difference ms | Wait reduction | Control time / pVisor time |
| --- | --- | --- | --- | --- |
| Firecracker | 184.12 | 181.60–185.84 | 64.6% | 2.82× |
| QEMU | 602.06 | 582.17–635.99 | 85.6% | 6.97× |
| microvm | 220.64 | 195.91–251.74 | 68.6% | 3.19× |

Differences are differences between the two medians, with 95% intervals from 5,000 bootstrap resamples of matching rounds. All three intervals exclude zero, supporting faster pVisor VM startup for these configurations. Dedicated firmware and simplified initialization provide a shorter startup path; this comparison cannot separately quantify kernel, filesystem and VMM contributions.

For cold-image client waiting and content downloads, see the separate [on-demand image startup](lazy-image-startup.md) experiment. Image-service preparation is recorded separately, and its samples are not pooled with these prepared-environment results.

### Downloads and reproduction {#run}

[Startup statistics](startup.csv) · [VM differences and 95% intervals](startup-stock-comparisons.csv) · [VM source and artifact provenance](startup-stock-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#explicit-firecracker-kernel-controls). Binaries, complete inputs and raw logs stay in local ignored `.data/`; CSVs retain statistical sources and sample counts, and provenance records retain CPU/RAM configuration and report/artifact digests.
