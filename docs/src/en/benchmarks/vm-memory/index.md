# Reclaiming cold VM memory: benefits and usage guidance

[Complete results and evidence](results.md) · [Methodology](methodology.md) · [Mechanism design](../../design/memory-sharing/index.md)

**The pool reduces idle VM RAM residency, with substantial reclamation observed for 2 GiB VMs too.** It suits tasks that can wait for cold-page scanning and tolerate restoration on first access. Tasks requiring consistently fast responses should prefer leaving it disabled.

This report measures the cold-memory mechanism: each VM owns 64 MiB of data on the frozen signed version from 2026-10-03. It includes 40 runs across 10 configurations and four extended 2 GiB runs. Content checks and exit cleanup pass throughout. It does not measure end-to-end Ubuntu or agent performance.

## Core measurements {#measurements}

The **RAM proxy** combines resident guest RAM, pending reclamation, temporary snapshots and encoded pool data to track cold-page reclamation. It is not whole-host physical memory. Each row aggregates two VMs with 2 vCPUs each, the default pool budget and the same repeated cold data.

| Configured memory per VM | Observation time after all guests ready | RAM proxy: pool off → on | Approximate reduction |
|---|---|---|---|
| 256 MiB | 18–33 seconds | 231 → 27 MiB | **89%** |
| 512 MiB | 18–33 seconds | 243 → 61 MiB | **75%** |
| 2 GiB | 60–90 seconds | 309 → 124 MiB | **60%** |

Capacities need different reclamation times. These rows show observed benefits, not a ranking at equal task duration. Values are rounded for readability; [results](results.md) retain exact measurements, paired ranges and other configurations.

**Costs to account for:** in the extended 2 GiB experiment, the first complete 64 MiB read increases from about 20 ms to 164 ms, with additional CPU consumption over the run. macOS footprint ledgers also increase. Cold RAM residency reduction is established, while whole-host physical-memory or pressure improvements remain unproven. See [access costs](results.md#performance) and [physical accounting](results.md#physical) for the evidence.

## The reclamation process for 2 GiB VMs {#long-idle-2048}

The pool begins publishing cold pages around 34 seconds after ready, then RAM proxy falls to about 124 MiB. Another decline occurs near three minutes. Both enabled runs show similar curves; disabled baselines stay near 309 MiB. **Larger VMs need longer observation, and short tasks may finish before substantial reclamation starts.**

![Reclamation and access costs for 2 GiB VMs](assets/2048-long-idle.svg)

Blue is pool enabled and brown disabled; solid and dashed lines show both repetitions. Top: RAM proxy; middle: pool occupancy; bottom: footprint. Green marks comparison windows; purple marks reads and exit around 180 seconds. The intermediate section fluctuates, and the final lower level is observed briefly, leaving final steady state unconfirmed. The [extended experiment and criteria](results.md#long-idle-2048) retain every phase.

## Choosing a configuration {#decisions}

| Scenario | Recommendation |
|---|---|
| Repeated compressible data with long idle periods | Trial the pool while measuring restoration latency and host pressure for your own tasks |
| 2 GiB or larger capacity | Allow longer scan time; a few dozen seconds cannot determine final benefit. Larger capacities remain unmeasured |
| Interactive or short tasks, continuously hot access | Prefer disabling the pool to avoid scan and restoration effects on responses |
| Random or incompressible content | Expect smaller benefits; first check application RAM needs and pool capacity |
| Single VM | Cold compression can help; single-instance benefits do not establish cross-VM deduplication |

Explicit RAM files do not inherently compress or share memory. FUSE-compressed RAM is a separate, mutually exclusive path with no performance data in this run.

## Minimal opt-in {#usage}

In one terminal, create a new owner-private directory and start the service; if the directory already exists, choose a new path or verify its permissions. Keep Unix socket paths short.

```bash
mkdir -m 700 /tmp/pvisor-pool-demo
pvisor memory-pool /tmp/pvisor-pool-demo/p
```

Run VMs in other terminals sharing the same socket. This rootfs image differs from the static guest measured here, so its actual benefits require separate measurement.

```bash
pvisor run --vm --memory 256MiB --cpu 2 \
  --vm-memory-pool /tmp/pvisor-pool-demo/p \
  --rootfs image=ubuntu:24.04 -- /bin/sh
```

Omit `--vm-memory-pool` to disable this experimental path. Do not stop the service before tasks finish: pool loss fails dependent VMs, and recovery after service restart is unsupported. The default 16 MiB budget limits encoded payload, not all physical memory used by the service. `--max-bytes 1048576` limits payload to 1 MiB and can increase capacity rejections.

`--vm-ram-backing FILE` requires a different, nonexistent file for each VM; it does not automatically share live RAM. `--vm-ram-compression` selects a separate FUSE path and cannot be combined with the pool.


## Usage boundaries {#limits}

Guest RAM must still cover application peaks. Validate first access, CPU, footprint, host pressure and swap for your workload. Lower cold residency does not establish how many additional instances a machine can run. Two pairs establish a direction for this workload, not long-term steady state, larger concurrency, real agent tasks or a production SLA.

For verification, consult [results and evidence](results.md) for the full matrix, diagnostic ledgers and failure records, then reproduce with the [methodology](methodology.md#reproduce).
