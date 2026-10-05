# How much waiting do network controls add compared with ordinary OCI?

## Main conclusions {#conclusions}

**Local 1 KiB HTTP request P50 is 0.95 ms native, 1.24 ms through the pVisor host proxy and 3.83 ms in the VM. A 32 MiB transfer reaches about 869, 417 and 155 MiB/s, respectively. Small-request proxy overhead is modest; VM bulk-transfer overhead is more pronounced. These are not Internet model-response timings.**

| Need | Selection implication |
|---|---|
| Small local requests | Proxy overhead is modest |
| Bulk downloads or fast local transfer | Check VM throughput |
| Direct sockets must be blocked | Choose an enforceable boundary |

## Motivation {#motivation}

Models can stream for seconds while tools issue many short requests and downloads. Separate request latency, throughput and process startup to assess the networking cost.

## Experiment design {#interpretation}

Same-host IP HTTP origin; no Internet/TLS/DNS. Each path has 3 warmups and 30 batches. Small: 256×1 KiB, concurrency 8, a new connection per request, 7,680 requests per path. Bulk: 32 MiB. Stream: 10 chunks, 2 ms apart. Lengths/hashes are validated. First body byte is not model TTFT. VM uses auto TCP; host/OCI use proxy; Podman uses host networking.

These results are from Linux/x86_64; matching macOS workloads are unmeasured. Linked reports pin artifacts, cache conditions and samples.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

Measured on 2026-10-04; configurations retain separate samples. P50 is the median.

| Network configuration | Backend | Batches | 1 KiB P50/P95 ms | 32 MiB P50 MiB/s | Stream first body P50/P95 ms |
|---|---|---|---|---|---|
| proxy / VM | native | 30 | 0.95/1.42 | 869.4 | 1.10/1.20 |
| proxy / VM | host | 30 | 1.24/2.42 | 416.8 | 1.37/1.63 |
| proxy / VM | vm | 30 | 3.83/8.63 | 154.7 | 4.57/5.19 |
| host-network OCI | native | 30 | 0.99/1.44 | 861.4 | 1.07/1.18 |
| host-network OCI | podman | 30 | 1.01/1.43 | 853.7 | 3.53/3.60 |
| host-network OCI | pVisor OCI | 30 | 1.30/2.30 | 811.0 | 3.78/3.98 |

### Analysis and denial checks

Host adds about 0.29 ms to small-request P50; VM adds about 2.88 ms. Main-batch bulk P50 is roughly 869 MiB/s native, 417 host and 155 VM. Bulk timing includes connect/read, with hashing after transfer; worker time also includes hashing. This describes a local HTTP path, not public Internet capacity.

Host deny-all with a private network namespace and VM deny-all each blocked direct sockets **30/30**, with non-bypassable networking confirmed in the Bundle. Ordinary host proxy enforcement is cooperative and direct sockets can bypass it. Fast denials are correctness checks rather than throughput results.
### Read the numbers as requests and downloads {#baseline-meaning}

Native HTTP is the baseline without the pVisor path; Podman/crun is a measured ordinary OCI path. Host proxy adds about 0.29 ms per small request, and VM about 2.88 ms.

Bulk downloads differ: at the measured rates, transferring 32 MiB takes about 37 ms natively and 207 ms through VM. Many local requests, dependency downloads, and model streams are different workloads. The [complete Docker tool-environment comparison](agent-tasks.md#reference-env) uses network none; its task timings do not establish Docker bridge or Internet performance.

### Scope {#acceptance}

Internet, TLS, DNS and real-model latency are unmeasured. Local first byte is not model TTFT; network paths differ from host-network OCI boundaries.

### Downloads and reproduction {#run}

[Derived table CSV](network.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
