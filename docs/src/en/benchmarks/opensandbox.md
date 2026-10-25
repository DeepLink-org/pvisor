# How long does a local OpenSandbox interface wait, and how much memory does it charge?

## Motivation {#motivation}

pVisor's native guest backend suits local SDK flows that frequently create sandboxes and execute short commands; its smaller fixed service charge also benefits small deployments. Each VM still has kernel and runtime costs. An explicit physical pool reduces charges for identical working sets while increasing scanning CPU and readback waiting.

Steady creation medians are 100.7 ms for pVisor and 149.7 ms for OpenSandbox/Docker; creation through the first checked command takes 120.9 ms and 358.8 ms. Service plus one sandbox charges 92.83 MiB and 278.29 MiB, while amortized increments at four sandboxes are 88.46 MiB and 55.54 MiB. A smaller service total does not mean every VM uses less memory.

## Experiment design {#interpretation}

Measure SDK creation through return and first checked command completion as availability waiting; measure complete product-group cgroup charges as deployment memory cost. They represent the wait before a short task and the budget of a local service. Lower values mean shorter waits and less charged memory to reserve.

The comparison uses pVisor native guests and official OpenSandbox server/SDK/execd 1.1.0 with the same Ubuntu 24.04 base on Linux x86_64/KVM, requesting 1 CPU/512 MiB per sandbox. pVisor uses a frozen performance/musl artifact, with its physical pool disabled for startup. VM limits include supervisor/VMM while Docker limits cover the container; isolation and available guest RAM differ. Images and services are prepared, host page caches remain warm, and image pull and API startup are excluded.

Startup uses an existing rootful Docker/runc service. First-after-API-restart and steady fresh-sandbox scenarios each use 20 seeded randomized pairs. All 80 formal instances and two preparation instances check stdout, private files and HTTP 404 deletion. All valid slow samples remain; intervals use 10,000 paired bootstrap resamples. N=20 does not establish P99.

Memory uses a separate cohort: default pVisor, the same artifact with an explicit physical pool, and the official service with private rootless Docker/containerd overlayfs. Three configurations alternate in randomized order for five rounds, using fresh services and four sandboxes per group. Product groups include API, VMM/supervisor/pool or dockerd/containerd/shims/containers and forwarding helpers. Each has a four-CPU quota, affinity 0–3, 4 GiB and zero swap; the SDK coordinator and observer are outside. Empty service and one/four idle instances are observed separately, followed by four identical 64 MiB allocations for 30 seconds, then a complete rewrite to instance-unique data for ten seconds. Full content, private files, resource ownership, OOM/swap and deletion are checked.

Sampling occurs every 500 ms. Each phase uses its final two seconds' median, then the median and range across five fresh groups; differences use 10,000 round-paired bootstrap resamples. Prepared caches may be charged outside the group: anon/file/kernel charges are not whole-host additional physical memory. Complete Docker PSS is unreadable and is not replaced by a partial process sum. Background tasks remain and are recorded. Startup and memory cohorts remain separate; pool memory cannot be combined with default-mode startup into one score.

## Data and analysis {#results}

### Creation and first command {#startup}

| Scenario and metric | OpenSandbox Docker/runc | pVisor default | Latency reduction, 95% CI |
| --- | --- | --- | --- |
| First after API restart: SDK create completion | 388.6 | 101.1 | 74.0% [73.4%, 75.4%] |
| First after API restart: Create → first checked command | 598.1 | 121.0 | 79.8% [78.8%, 80.6%] |
| Steady fresh sandbox: SDK create completion | 149.7 | 100.7 | 32.8% [30.5%, 36.7%] |
| Steady fresh sandbox: Create → first checked command | 358.8 | 120.9 | 66.3% [65.7%, 67.3%] |

Milliseconds, P50, N=20 per arm/scenario; percentages include paired 95% CIs. First means the first request after API restart with images and services prepared, not first download or cold disk.

![All paired creation and first-command samples](../assets/benchmarks/opensandbox-startup-distribution.svg)

Creation and command protocols jointly reduce waiting in this fixed-version short-task flow. All points remain; the create-to-command advantage is not a same-factor speedup for compilation or inference and does not cover other OpenSandbox backends.

### Deployment memory and amortization {#memory}

| Phase | OpenSandbox + private Docker | pVisor default | pVisor explicit physical pool |
| --- | --- | --- | --- |
| Empty service, no sandboxes | 177.90 [176.04–178.50] | 4.34 [3.91–4.50] | 4.21 [3.97–4.40] |
| Service + 1 idle sandbox | 278.29 [277.11–279.32] | 92.83 [92.41–93.09] | 83.39 [82.89–83.70] |
| Service + 4 idle sandboxes | 399.65 [399.27–402.51] | 357.98 [357.61–358.36] | 278.02 [275.18–280.08] |
| 4 × identical 64 MiB, after 30 s | 742.94 [742.32–744.55] | 631.51 [631.46–632.25] | 422.93 [419.99–426.77] |
| 4 × unique 64 MiB, after 10 s | 748.71 [745.64–749.91] | 634.17 [633.76–634.80] | 4/5 groups: 631.08 [628.50–633.34]; 1/5: 646.11 |

MiB, complete product-group charges; N=5 per configuration, median [minimum–maximum]. Unique-data pool readings retain the 4/5 and 1/5 split, without treating five groups as established bimodality.

Default service plus one sandbox is 66.6% lower [66.4%, 66.8%] than the measured private OpenSandbox/Docker deployment. This includes the service, initialization and caches; a deployment sharing an existing Docker daemon cannot charge its entire baseline again to each new container.

![Phase memory and ranges across five fresh product groups](../assets/benchmarks/opensandbox-memory.svg)

<details markdown="1">
<summary>Service baselines, instance increments and every group</summary>

| Metric | OpenSandbox + private Docker | pVisor default | pVisor explicit physical pool |
| --- | --- | --- | --- |
| Mean deployment increment per sandbox at N=1 | 100.39 | 88.49 | 79.02 |
| Mean deployment increment per sandbox at N=4 | 55.54 | 88.46 | 68.41 |

MiB per sandbox: calculate `(phase total − empty-service baseline) / instance count` within each group before taking five-group medians. Subtracting displayed medians is not equivalent. These amortized increments include initialization and caches; they are neither process RSS nor the next instance's marginal cost.

![Complete deployment and baseline-subtracted instance increments](../assets/benchmarks/opensandbox-memory-deployment.svg)

At four instances the container increment is below the default VM's. Only counts 0, 1 and 4 were measured; no capacity curve, maximum density or crossover count follows.

</details>

### Whether to enable the physical pool {#pool-tradeoff}

With identical working sets the explicit pool reduces default pVisor charges by another 208.58 MiB [204.74, 211.48], or 33.0% [32.4%, 33.5%]. Rewriting to unique data does not retain the same benefit. Physical sharing and disk image compression are different mechanisms, and the pool is optional.

| Configuration | Group CPU seconds in 30 s | Four full readbacks, ms |
| --- | --- | --- |
| OpenSandbox + private Docker | 0.059 [0.054–0.061] | 899.3 [897.4–901.8] |
| pVisor default | 0.162 [0.156–0.174] | 108.6 [106.1–117.7] |
| pVisor explicit physical pool | 2.861 [2.577–3.223] | 443.9 [419.4–445.8] |

N=5 medians [minimum–maximum]. Whole-group CPU over the 30-second window is in seconds; four full readbacks are in ms, including SDK, content checks and stream completion rather than pure page-fault time.

![CPU and readback costs of physical sharing](../assets/benchmarks/opensandbox-memory-tradeoffs.svg)

Choose a pool by content similarity, residency time and access frequency. Identical-content memory savings accompany extra CPU and readback waiting. This fixed workload does not predict net gains for real agents; long-term stability and production capacity need separate tests.

### Image disk and reproduction {#provenance}

Image inventory is a separate storage experiment; see the [lazy-image disk data](lazy-image-startup.md#storage). It excludes API state, runtime writable layers and the whole OpenSandbox service directory, so it is not a complete disk comparison of the same SDK configuration.

The startup and memory figures are **fixed-version historical evidence from 2026-10-24**. This is documentation migration without new sampling, not a performance retest of current main. Daemon SHA-256 is `74461c7ad588633228d44a8ada5d8335de50ea55ca9c5c543886c6533540f575`; the memory host is a Ryzen 7 9700X with kernel 7.2.11-200.fc44.

| Evidence | Samples and checks | summary SHA-256 |
| --- | --- | --- |
| First/steady SDK requests | 40 pairs, 80 formal instances; zero failures/exclusions | `8fc9fd74d6c4450dd9be07af58fbdc80156dbc6cacf4691c97daa416951b5e12` |
| Complete memory groups | 15 groups, 60 instances; zero failures/exclusions | `13d437b59e56bc4534d48b24999ab44c1dd5ef562ea005a4f0a66664a9ebe067` |

Raw records, frozen source, build and input receipts remain locally in `benchmark/.data/opensandbox-native-docker-20261024T020535Z/` and `benchmark/.data/opensandbox-memory-formal-20261024/`. [Evidence and retest entry](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/startup/README.md#opensandbox-comparison) · [Burst and memory-pool diagnostics](../lab-notes/opensandbox-performance-analysis.md). These results do not establish full protocol conformance, equivalent security, sustained throughput or production density.
