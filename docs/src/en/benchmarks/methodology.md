# Which benchmark results support an adoption decision?

## Main conclusions {#conclusions}

**Choose execution modes using measured workloads with matching budgets and timing boundaries; use different configurations to understand their individual performance levels.** Startup, tool execution, application and resource occupancy answer different questions.

| Decision | Required evidence |
|---|---|
| Choose local tool execution | Native, Docker and pVisor host/staged/VM task comparisons |
| Choose an independent guest kernel | Firecracker, QEMU and pVisor startup plus tool data |
| Set capacity or timeouts | Success rates, complete-task waiting and matching resource accounting |

## Motivation {#motivation}

Faster startup does not guarantee faster compilation. Writable mounts and staged review also provide different workflows. Adoption decisions require speed, execution boundaries and how changes reach the original directory.

## Experiment design {#interpretation}

### Same-host controls

B-STARTUP, B-FS-TOOLS and B-AGENT-TASK use one offline tool environment and fixed inputs across native, pVisor host/staged/VM, private rootless Docker, Firecracker PCI and QEMU q35/microvm. Images, tools and daemon are prepared beforehand. Every execution uses the same two-core affinity; tool VMs have matching configured memory. Each job gets a fresh workspace, with seeded randomized runtime order. Complete Ubuntu, macOS and distinct binaries remain separate cohorts.

Downloads, benchmark compilation, image import and input copying are excluded. Timing separates first valid output, internal tools plus validation, returned results and launch-to-process-exit. Successful samples require validated outputs, observed executor and complete staging. Staged modes also verify that original host files remain unchanged.

### Distributions and failures

Cohorts are not pooled, and slow samples are not removed after measurement. Failures and invalid outputs are counted separately and never treated as zero latency. Capacity guards are not completed jobs. P95 is descriptive: it is omitted below 30 samples. Public P99 is omitted; small cohorts do not support stable tail-latency claims.

Separated clusters are reported with counts and individual medians. The descriptive split rule is in the [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md); it establishes no cause. Engineering A/B percentage claims need a 95% bootstrap interval for the median difference; user pages do not narrate optimization work.

### Isolation and resources {#reference-resources}

Docker writable bind mounts write directly to the host; pVisor staging retains changes until apply. Firecracker/QEMU use private ext4, while pVisor VM uses virtio-fs with different kernels and devices. This is not a pure VMM or production-security ranking. Standalone staging boundaries follow actual records and [isolation checks](isolation-tests.md).

RSS sums sampled process scopes and may double-count shared pages or miss short peaks. Docker must include actual container processes and identify its dedicated daemon. Whole-group memory must include all descendants, backing and charged cache, without double-counting overlapping cgroups. Configured RAM, RSS, macOS RAM proxies and net physical memory are distinct metrics.

## Data and analysis {#results}

### Prepared environments {#reference-env}

[Startup](startup.md) · [Files and tools](filesystem.md) · [Repair tasks and CLIs](agent-tasks.md). Tables retain cohort identities and counts, with pinned artifacts rather than claims about current third-party releases.

### Complete distributions {#full-ubuntu}

Ubuntu uses its distribution kernel, initrd, systemd and private disks; pVisor reuses prepared tool directories. The comparison answers deployment waiting, rather than isolating VMM cost.

### QEMU complete distributions {#full-ubuntu-qemu}

q35 and microvm use one Ubuntu template. Their samples and percentiles remain separate from other cohorts.

### Tasks and resources {#product-v1}

[Application](supervision-cost.md#apply-cost) · [Network](network.md) · [Concurrency](density.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md) · [Review](supervision-cost.md). Unmeasured industry alternatives are marked explicitly, without marketing numbers filling gaps.

### Filesystem engineering experiments {#filesystem-service}

Engineering A/B, instrumented profiles and diagnostic probes remain in [technical analysis](../design/filesystem-performance-analysis.md), outside cross-product main tables.

### Data location and downloads {#evidence-format}

Markdown holds derived user-facing tables, with downloadable CSVs beside each article. Raw reports, individual samples, logs, artifact manifests and frozen harnesses live in the relevant directory’s `.data/`, ignored by Git and excluded from the site. CSV downloads contain derived statistics and source summaries, rather than claiming to contain raw samples.

See the [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md) for reproduction and retention rules.

### Downloads and reproduction {#run}

[Derived table CSV](methodology.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
