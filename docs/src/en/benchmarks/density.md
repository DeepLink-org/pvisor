# Concurrency density and resource cost

Native, host, staged and Podman completed all jobs at concurrency 128 in an idle probe. Safe had occasional port races at 128. Main-batch VM reached 8; 32/128 were guarded by available host memory. Follow-up completed 32 VMs and 128 minimal-shell OCI jobs with an observed-RSS budget; VM 128 remains unmeasured. These are observed lower bounds on capacity.

## Motivation

With multiple agents, memory, startup and environment preparation accumulate. Publish completion rates with resources rather than only successful fast samples.

## Experiment design {#interpretation}

Each Job prints ready then holds for one second. Concurrency 1/8/32/128, five batches per cell, no warmups. Sample owned process-tree peak RSS every 20 ms and collect child CPU and per-Job wall time. VM uses 2 vCPU/128 MiB. This is an occupancy probe, not active agent/build throughput. Only wholly successful batches enter timing/resource summaries; completion denominators include failed batches.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

| Rootfs batch | Backend | Concurrency | Completed/attempted | Full batches | RSS P50 MiB | CPU P50 ms/job | Job P50/P95/P99 ms |
|---|---|---|---|---|---|---|---|
| main | native | 1 | 5/5 | 5 | 2.0 | 1.01 | 1002.3/1002.5/1002.6 |
| main | native | 8 | 40/40 | 5 | 16.5 | 0.78 | 1001.5/1002.6/1002.7 |
| main | native | 32 | 160/160 | 5 | 66.4 | 0.71 | 1002.1/1002.9/1003.3 |
| main | native | 128 | 640/640 | 5 | 266.4 | 0.68 | 1002.2/1003.2/1010.6 |
| main | host | 1 | 5/5 | 5 | 12.1 | 7.98 | 1018.9/1021.6/1022.1 |
| main | host | 8 | 40/40 | 5 | 95.3 | 6.75 | 1019.3/1019.9/1019.9 |
| main | host | 32 | 160/160 | 5 | 383.4 | 7.21 | 1019.9/1021.1/1021.5 |
| main | host | 128 | 640/640 | 5 | 1533.1 | 7.72 | 1032.1/1055.7/1059.0 |
| main | staged | 1 | 5/5 | 5 | 12.4 | 12.80 | 1039.8/1127.7/1137.4 |
| main | staged | 8 | 40/40 | 5 | 99.5 | 11.96 | 1075.5/1101.6/1110.4 |
| main | staged | 32 | 160/160 | 5 | 398.8 | 12.83 | 1132.2/1211.5/1214.7 |
| main | staged | 128 | 640/640 | 5 | 1597.2 | 13.96 | 1204.6/1248.6/1315.3 |
| tools | safe | 1 | 5/5 | 5 | 26.0 | 61.04 | 1194.1/1238.4/1238.7 |
| tools | safe | 8 | 40/40 | 5 | 205.2 | 53.25 | 1300.5/1411.6/1416.2 |
| tools | safe | 32 | 160/160 | 5 | 821.6 | 37.05 | 1135.1/1156.4/1158.8 |
| tools | safe | 128 | 638/640 | 3 | 3245.0 | 39.97 | 1446.3/1490.5/1504.6 |
| tools | vm | 1 | 5/5 | 5 | 99.5 | 213.12 | 1219.2/1235.1/1238.2 |
| tools | vm | 8 | 40/40 | 5 | 786.5 | 274.22 | 1281.2/1323.2/1326.8 |
| tools | vm | 32 | guard | 0 | — | — | — |
| tools | vm | 128 | guard | 0 | — | — | — |
| tools | podman | 1 | 5/5 | 5 | 49.1 | 31.66 | 1054.4/1056.4/1056.8 |
| tools | podman | 8 | 40/40 | 5 | 391.9 | 36.69 | 1114.2/1181.3/1190.5 |
| tools | podman | 32 | 160/160 | 5 | 1570.6 | 45.42 | 1407.4/1679.2/1740.7 |
| tools | podman | 128 | 640/640 | 5 | 6011.3 | 51.42 | 6469.9/8542.5/9251.6 |
| tools | container | 1 | 5/5 | 5 | 26.2 | 245.13 | 1269.6/1393.1/1415.9 |
| tools | container | 8 | 40/40 | 5 | 210.1 | 493.64 | 1521.9/2512.8/2516.0 |
| tools | container | 32 | 45/160 | 0 | — | — | — |
| tools | container | 128 | 55/640 | 0 | — | — | — |
| shell | vm | 1 | 5/5 | 5 | 98.5 | 217.11 | 1239.3/1272.2/1278.7 |
| shell | vm | 8 | 40/40 | 5 | 789.8 | 283.13 | 1292.1/1306.1/1318.2 |
| shell | vm | 32 | 160/160 | 5 | 3093.4 | 370.44 | 1939.0/2115.2/2132.7 |
| shell | vm | 128 | guard | 0 | — | — | — |
| shell | podman | 1 | 5/5 | 5 | 48.8 | 36.72 | 1060.1/1085.2/1090.0 |
| shell | podman | 8 | 40/40 | 5 | 389.2 | 38.50 | 1132.4/1189.4/1237.3 |
| shell | podman | 32 | 160/160 | 5 | 1559.6 | 45.99 | 1497.0/1969.4/2071.9 |
| shell | podman | 128 | 640/640 | 5 | 5929.0 | 50.74 | 6473.4/8242.6/8752.4 |
| shell | container | 1 | 5/5 | 5 | 26.1 | 18.34 | 1037.9/1054.4/1057.6 |
| shell | container | 8 | 40/40 | 5 | 208.1 | 20.36 | 1048.4/1048.9/1049.1 |
| shell | container | 32 | 160/160 | 5 | 834.2 | 22.05 | 1060.1/1066.9/1071.4 |
| shell | container | 128 | 640/640 | 5 | 3338.9 | 28.43 | 1300.2/1478.3/1528.9 |

### Analysis

Main safe concurrency 128 completed **638/640** jobs. Failures reported `Address already in use`, a race between free-port probing and actual listening. Successful samples do not establish stable concurrency 128. Idle VM tree RSS is roughly 100 MiB at one and 789 MiB at eight, not configured RAM or a maximum active working set.

The tools rootfs is about 749 MiB. pVisor OCI copies a private environment per Job into default `/tmp`; concurrency 32/128 hit the tmpfs user quota (`Disk quota exceeded`). Failures remain visible. A minimal-shell follow-up is separate, distinguishing runtime from tool-environment preparation. Podman uses a prebuilt shared image rather than the same full per-Job copy.
## Limits and next measurements {#acceptance}

This shared desktop has editors and background work; it is not a dedicated maximum-capacity experiment. Main VM guarding was conservative. Follow-up budgets max(128 MiB, 1.5×observed single-VM RSS) per Job plus 2 GiB host reserve. Summed RSS counts shared pages repeatedly and is not PSS/system memory. Podman daemons may fall outside the tracked ancestry, so CPU/RSS do not support a strict whole-system ranking.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites density --samples 30 --warmups 3
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
