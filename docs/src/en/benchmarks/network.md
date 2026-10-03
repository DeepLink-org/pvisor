# Proxy and VM TCP overhead

Local HTTP request P50 is about 0.95 ms native, 1.24 ms through the host proxy and 3.83 ms in the VM. Streaming first-body-byte P50 is 1.10, 1.37 and 4.57 ms. Latency costs are milliseconds; bulk-transfer throughput falls more substantially.

## Motivation

Models can stream for seconds while tools issue many short requests and downloads. Separate request latency, throughput and process startup to assess the networking cost.

## Experiment design {#interpretation}

Same-host IP HTTP origin; no Internet/TLS/DNS. Each path has 3 warmups and 30 batches. Small: 256×1 KiB, concurrency 8, a new connection per request, 7,680 requests per path. Bulk: 32 MiB. Stream: 10 chunks, 2 ms apart. Lengths/hashes are validated. First body byte is not model TTFT. VM uses auto TCP; host/OCI use proxy; Podman uses host networking.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

| Batch | Backend | Batches | 1 KiB P50/P95/P99 ms | 32 MiB P50 MiB/s | Stream first body P50/P95/P99 ms |
|---|---|---|---|---|---|
| main | native | 30 | 0.95/1.42/4.67 | 869.4 | 1.10/1.20/1.40 |
| main | host | 30 | 1.24/2.42/7.16 | 416.8 | 1.37/1.63/1.69 |
| main | vm | 30 | 3.83/8.63/17.36 | 154.7 | 4.57/5.19/6.44 |
| OCI follow-up | native | 30 | 0.99/1.44/4.85 | 861.4 | 1.07/1.18/1.18 |
| OCI follow-up | podman | 30 | 1.01/1.43/9.07 | 853.7 | 3.53/3.60/3.64 |
| OCI follow-up | container | 30 | 1.30/2.30/10.06 | 811.0 | 3.78/3.98/4.05 |

### Analysis and denial checks

Host adds about 0.29 ms to small-request P50; VM adds about 2.88 ms. Main-batch bulk P50 is roughly 869 MiB/s native, 417 host and 155 VM. Bulk timing includes connect/read, with hashing after transfer; worker time also includes hashing. This describes a local HTTP path, not public Internet capacity.

Host deny-all with a private network namespace and VM deny-all each blocked direct sockets **30/30**, with non-bypassable networking confirmed in the Bundle. Ordinary host proxy enforcement is cooperative and direct sockets can bypass it. Fast denials are correctness checks rather than throughput results.
## Limits and next measurements {#acceptance}

Nested requests may be correlated; no independent-request confidence interval is claimed. OCI follow-up is kept separate. Host networking does not measure Docker bridge/CNI. UDP/IPv6/QUIC, real model SSE, TLS interception and public API variability are outside this batch.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites network --samples 30 --warmups 3 --network-backends native,host,vm
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
