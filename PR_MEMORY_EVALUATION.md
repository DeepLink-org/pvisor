# Memory evaluation: runtime page sharing and snapshot instance costs

## Architecture

pVisor places the filesystem backend, directory merging, and per-instance filesystem state management on the host, exposing file access through virtio-fs. In the evaluated path, the host also writes control files and reads results directly, while the guest runs a lightweight supervisor and the application. AgentENV performs the corresponding operations through in-guest envd and a block-device-backed filesystem.

pVisor can additionally scan for identical runtime pages on the host, consolidate them into a physical sharing pool, and preserve instance isolation through copy-on-write (COW). We evaluate these benefits separately: runtime deduplication of newly written content, and instance memory costs when both systems already share prewarmed snapshots. A subsequent mapping-level study attributes the static memory difference.

## Experimental setup

Both systems run on the same host, pinned to CPUs 8 and 9 under identical resource budgets, with 2 vCPUs and 512 MiB configured per VM and the same OCI userspace image. Each condition is repeated three times with data and state validation. Both retain their native execution paths; host KSM and swap are disabled. This compares complete implementations under matched tasks and resource constraints.

- **Runtime sharing:** four VMs each generate 64 MiB of data in three patterns: highly repetitive content, random content identical across VMs, and independent random content per VM. We compare pVisor with its pool disabled and enabled against AgentENV, including idle, mutation, and isolation checks.
- **Static snapshots:** both systems prewarm the same Python standard-library workload, a 4,096-row SQLite database, and 64 MiB of deterministic data. Each creates its own native snapshot, terminates the producer, and restores 1, 2, 4, 8, or 16 instances. pVisor's runtime pool is disabled; AgentENV's native reclamation remains enabled. The attribution study repeats this setup and accounts for the complete guest RAM mappings, including pVisor's additional approximately 27.3 MiB boot/kernel region and AgentENV's anonymously remapped regions.

## Results

### 1. Runtime sharing: savings depend on content duplication

Total product-process PSS after 60 seconds of idle time, in MiB. Values are medians of three repetitions.

| Data pattern | pVisor, pool off | pVisor, pool on | AgentENV | Reduction vs. AgentENV, pool on |
|---|---:|---:|---:|---:|
| Highly repetitive | 423.91 | 151.44 | 523.54 | 71.1% |
| Random within each VM, identical across VMs | 425.11 | 223.72 | 523.91 | 57.3% |
| Independent random content per VM | 423.60 | 406.41 | 521.49 | 22.1% |

The last column is `(AgentENV PSS − pVisor pool-on PSS) / AgentENV PSS`. It measures the system-level difference, which includes implementation costs beyond deduplication. The pool-on/off comparison isolates the pool's benefit within pVisor: 64.3%, 47.4%, and 4.1%, respectively.

Scanning has a CPU cost: across these patterns, the 60-second window consumes 9.64–16.92 CPU-seconds with the pool enabled, versus 1.76–1.77 with it disabled. Savings shrink substantially after all payloads are overwritten with different content per VM.

### 2. Static snapshots: lower instance costs with sharing enabled on both systems

Total product-process PSS after snapshot restore and 60 seconds of idle time, in MiB; medians of three repetitions.

| Instances | pVisor | AgentENV |
|---|---:|---:|
| 1 | 103.47 | 204.04 |
| 2 | 112.10 | 232.52 |
| 4 | 127.64 | 277.52 |
| 8 | 158.73 | 371.19 |
| 16 | 220.36 | 557.74 |

At 16 instances, pVisor uses 60.5% less process PSS with its runtime pool disabled. This reflects the cost of the native snapshot execution paths. Idle CPU favors AgentENV: the same 60-second window consumes 14.36 CPU-seconds for pVisor and 9.17 for AgentENV. The application still polls its control file every 100 ms during this interval.

### Attribution follow-up: lower guest private-memory growth

These measurements come from a separate mapping-level run and are not pooled with the preceding samples. Values are medians [minimum, maximum] after 60 seconds of idle time, in MiB.

| Metric | pVisor | AgentENV | Reduction |
|---|---:|---:|---:|
| Guest COW resident memory, 16 instances total | 47.73 [47.56, 47.85] | 234.16 [232.12, 235.25] | 79.6% |
| Guest RAM PSS, 16 instances total | 140.22 [140.16, 140.46] | 400.71 [398.20, 403.10] | 65.0% |
| Guest RAM PSS per additional VM, 8→16 instances | 2.95 [2.91, 3.02] | 17.81 [17.40, 18.75] | 83.4% |
| Total process PSS per additional VM, 8→16 instances | 7.62 [7.58, 7.65] | 22.69 [22.68, 23.84] | 66.4% |

Marginal costs are calculated within each repetition as `(PSS at 16 − PSS at 8) / 8`, then summarized. Dividing the 16-instance COW totals by 16 gives **2.98 MiB/VM for pVisor versus 14.63 MiB/VM for AgentENV**. The advantage therefore includes lower guest private-memory growth, beyond differences in fixed service overhead.

Host-side costs remain included: at 16 instances, non-guest VMM PSS is 79.26 MiB for pVisor versus 17.85 MiB for AgentENV; AgentENV additionally uses 137.44 MiB of backend-service PSS. Total process PSS is 219.49 versus 555.92 MiB in this follow-up. pVisor's guest-memory savings remain beneficial after accounting for the host processes in this deployment. Independently calculated component medians need not sum exactly to the total median.

## Conclusions and scope

The experiments demonstrate two distinct benefits: host-side physical sharing consolidates duplicate runtime content, and pVisor retains lower guest private-memory and instance growth costs when both systems use prewarmed snapshot sharing. Moving filesystem and evaluated control operations to the host, with less mutable state maintained in the guest, is an architectural explanation consistent with the latter result. Individual contributions have not been isolated through component ablations.

PSS covers process mappings, including each system's deployed backend services, but excludes some unmapped file cache and host kernel resources. COW is reported as resident anonymous memory within private snapshot-file mappings; it is a guest RAM subcomponent, not an additional amount to add to guest PSS. Reclaimed regions remapped anonymously remain included in guest RAM and are tracked separately from snapshot-file COW. The two workloads' savings must not be added together or generalized to compression, sustained write-heavy workloads, or uniformly lower CPU costs.

## Validation and reproducibility

The runtime and companion lifecycle campaign passed 39 conditions and 1,650 checks. The static campaign and attribution follow-up each completed 30 measured conditions, two preflights, and 1,108 functional checks. All attribution steady-state samples passed stability checks. One `ready`-phase sampling fluctuation is retained in the audit and does not affect the steady-state results above. Each run retains all three repetitions; reported ranges describe observed variation, not confidence intervals.

- pVisor base: `22603811a9d030d4206903a488067ce801257513`
- AgentENV: `3e33057d9a2dc8cd8e14dc34838d97debeaf8e86`
- Shared OCI userspace: `sha256:7c87e89271049d850435e87a3a41f671e87c85bd083c541789ed6b928265832b`
