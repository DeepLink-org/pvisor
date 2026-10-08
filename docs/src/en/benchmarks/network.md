# What does network policy add compared with ordinary OCI?

## Main conclusions {#conclusions}

**For eight-thread local HTTP small requests, native, pVisor host proxy and VM median request times are about 0.69, 10.09 and 2.10 ms. A 32 MiB VM transfer takes about 193.62 ms. Host-proxy small-request cost warrants attention; local measurements do not establish public model-response latency.**

| Need | Selection implication |
|---|---|
| Frequent local small requests | Host proxy adds noticeable waiting; check your request rate |
| Dependency downloads inside a VM | Budget transmission separately from environment startup |
| Blocking direct TCP connections | Host/VM deny-all positive and negative controls are tested; latency is not isolation evidence |

## Motivation {#motivation}

Agents request models, download dependencies and consume streams. These workloads respond differently to proxy overhead. Separate request latency, first byte, full transmission and startup to understand the waiting involved.

## Experiment design {#interpretation}

Linux/x86_64, one local HTTP origin on the same host, without Internet, TLS or DNS. Compare native, pVisor host proxy, VM auto TCP, Podman host network and pVisor OCI proxy. Every payload uses two fixed CPUs (host 0/1, two VM vCPU); memory is not identically capped and the HTTP origin sits outside the payload budget. This is not a resource-density ranking.

Small requests use 256 fresh TCP connections per batch, eight threads and 1 KiB responses. Take the median within each batch, then summarize 30 independent batches; 7,680 correlated requests are not independent samples. Bulk sends 32 MiB per batch in 1 KiB writes. Stream sends ten 13-byte events, 2 ms apart. First byte means the first HTTP body byte, not model TTFT. Transfer time includes connection and reading, excluding later digest validation; worker total time includes validation.

Three warmups and 30 formal batches per condition; 17 conditions are randomized within each round. Full content length/SHA-256, CPU affinity, retained command output and Run boundaries must pass. Host/VM deny-all independently test direct-socket rejection and Bundle enforcement evidence. Failed batches cannot contribute latency; all slow valid samples remain without retrospective exclusions. P95 is descriptive only, with no P99.

## Data and analysis {#results}

Measured on 2026-10-06 local time. All 510 formal batches across 17 conditions pass; inputs remain unchanged. N=30 per performance cell. Times are ms; P50 is the median across independent batch statistics.

### Small requests and streaming first byte

| Mode | P50 of within-batch request medians | Difference from native, 95% interval | Stream first-byte P50 | Complete stream P50 |
|---|---:|---:|---:|---:|
| Native | 0.69 | — | 1.12 | 19.71 |
| pVisor host proxy | 10.09 | +9.40 [9.36, 9.45] | 4.23 | 22.83 |
| pVisor VM auto TCP | 2.10 | +1.40 [1.37, 1.43] | 7.88 | 26.36 |
| Podman host network | 0.71 | +0.01 [−0.001, 0.019] | 3.79 | 22.48 |
| pVisor OCI proxy | 10.23 | +9.54 [9.49, 9.61] | 6.87 | 25.32 |

Differences use paired rounds and 5,000 bootstrap resamples. Podman's interval includes zero: no difference from native is detected for small requests. Other paths add waiting. Host/OCI proxy and VM auto use different network paths; their differences cannot all be attributed to virtualization or containers.

### 32 MiB transfers

| Mode | Cluster median or P50 transfer, ms | Corresponding cluster median or P50 rate, MiB/s |
|---|---|---|
| Native | 32.61 (17/30); 74.90 (13/30) | 981.37; 427.21 |
| pVisor host proxy | 35.65 (20/30); 76.28 (10/30) | 897.55; 419.50 |
| pVisor VM auto TCP | 193.62 (30/30, unsplit) | 165.27 |
| Podman host network | 36.45 (17/30); 76.63 (13/30) | 877.90; 417.58 |
| pVisor OCI proxy | 38.84 (20/30); 80.02 (10/30) | 823.83; 399.94 |

All transfer distributions except VM have two separated clusters. Show their counts and medians rather than ranking one P50 or giving one VM/native ratio. Clustering is descriptive; its cause is unverified. The origin's 1 KiB writes and Python HTTP implementation influence throughput, so these values are not VMM bandwidth limits.

### Rejection checks and scope {#acceptance}

Host deny-all and VM deny-all each block direct sockets to the same local service in 30/30 batches, with `network_non_bypassable` confirmed by the Bundle. Complete allow responses provide positive controls. This does not replace a comprehensive network-security audit.

### Comparison with familiar options {#baseline-meaning}

Podman host network supplies an ordinary OCI control. pVisor OCI additionally uses a policy proxy, so boundaries differ. Docker bridge, Firecracker/QEMU networking, macOS, TLS and public APIs are unmeasured and receive no numeric ranking. Complete-task Docker/Firecracker/QEMU comparisons are in the [end-to-end tasks](agent-tasks.md).

### Downloads and reproduction {#run}

[Network statistics CSV](network-summary.csv) · [Paired comparisons CSV](network-comparisons.csv) · [Artifacts and validation summary](network-provenance.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

Raw reports, response timings, command logs, input/build receipts and per-invocation output audits stay in local `.data/`.
