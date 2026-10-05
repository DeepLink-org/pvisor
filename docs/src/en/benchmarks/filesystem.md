# Filesystem and development-tool overhead

Same-host Docker comparisons put 64 MiB reads at **33 ms** native/Docker versus **49 ms** staged, and traversal of 2,048 files at **5 ms** native/Docker versus **180 ms** staged. Individual reads add tens of milliseconds; dense small-file paths remain costly. VM offline npm takes **1.73 s**, versus **0.23 s** in Docker.

## Motivation

Agents repeatedly list, search and modify files. Tool time and full job time together show the cost of choosing staging or a VM.

## Experiment design {#interpretation}

Identical inputs across native, host, staged, safe, libkrun VM, rootless Podman/crun and pVisor OCI. Main cells have 3 warmups and 30 measurements with warm host caches. Image/input preparation is excluded. Worker time includes tool execution and validation; wall time includes launch and teardown. metadata/git/rg use 2,048 files in 32 directories; read validates a 64 MiB hash; write creates 256×60 KiB files. cargo builds 64 dependency-free modules and verifies 2016. npm installs 32 local packages offline, without registry access.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-05, real VM/FUSE A/B baseline {#e2e-baseline}

This batch boots actual KVM VMs and mounts actual host FUSE, completing the
end-to-end check missing from the adapter experiment. Host devices are available;
the initial sandbox hid `/dev/kvm` and `/dev/fuse`. Both GNU/Linux release binaries
come from the same `a1020d4b` source archive and differ only in OverlayCore's
`core.rs`. Both include the same guest stdio readiness fix. Binary, firmware,
source patch and harness hashes are retained.

Each round shuffles five cells: native and both versions of FUSE staged and VM.
Each cell has three warmups and 30 measurements: 150 measured jobs and 1,050 tool
measurements, all correct. Every job has a fresh workspace/upper and executes the
seven workloads in order in one environment. Fixture copying is excluded. Host
caches are warm; all launch trees use physical host cores `0,1`; VMs have 2 vCPU
and 16 GiB. The pinned historical fixture writes 256×64 KiB (16 MiB), unlike the
current product-v1 60 KiB files. Historical percentiles are not pooled. Concurrent
host activity remained: one-minute load average fell from 10.38 to 3.06. Affinity
is a shared execution budget, not exclusive CPUs. This is one screening batch.

Tool operation and validation P50 in ms; negative changes mean less elapsed time:

| Workload | Native | FUSE before→after | Change | VM before→after | Change |
|---|---:|---:|---:|---:|---:|
| metadata | 4.93 | 92.38 → 78.25 | -15.3% | 224.36 → 195.24 | -13.0% |
| read | 32.56 | 68.30 → 67.82 | -0.7% | 116.98 → 117.90 | +0.8% |
| write | 3.90 | 201.36 → 198.92 | -1.2% | 243.10 → 257.16 | +5.8% |
| git | 15.11 | 190.36 → 170.46 | -10.5% | 484.14 → 494.86 | +2.2% |
| rg | 7.66 | 100.80 → 90.58 | -10.1% | 476.92 → 464.97 | -2.5% |
| cargo | 52.92 | 112.30 → 110.11 | -1.9% | 513.46 → 512.74 | -0.1% |
| npm | 173.19 | 221.67 → 217.47 | -1.9% | 1570.05 → 1503.67 | -4.2% |

Launch-to-exit P50 for the seven-workload job is **454.50 ms** native,
**1288.69 → 1230.93 ms (-4.5%)** FUSE staged and
**4829.04 → 4777.55 ms (-1.1%)** VM. Metadata savings reproduce in the actual
paths. This does not establish improvements for VM git, writes or overall jobs:
VM write and git became slower in this batch. Small changes and write behavior
need a quieter-host rerun; a single P50 cannot establish their cause.

An earlier warmup exited zero and completed writes without any workload stdout
markers. The harness stopped and retained the failure. The ordinary VM runner now
declares the named ports required by its actual non-terminal standard descriptors.
The guest waits for their names before launching tools, failing after at most five
seconds. Regression tests cover delayed names, later port creation and absent
ports; there is no unconditional startup delay. All 68 VM runs in the formal batch,
including preflight and warmups, passed output checks. The harness also verifies
observed isolation, no writes in lower and all 256 upper files with correct sizes.
Identical binaries, a mismatched build manifest, missing samples or interruption
cannot produce a successful A/B summary.

A separate profiled batch does not enter the timing table. The candidate VM's two
nonempty Core instances still record **126,034 / 168,027** parent metadata calls
and **57,547 / 53,161** mount identity attempts at their last checkpoints.
Dispatch checkpoints show **20,545 inline / 424 pool** and
**24,398 inline / 100 pool**, with two workers each. The complete candidate host
Core profile records 341 journal fsync calls totaling about 110 ms. Repeated Core
physical path checks, first-observation/write journal costs and inline dispatch
of small requests warrant separate experiments. Neither more workers nor
virtiofsd can be assumed to remove these costs. VM checkpoints lack final records
and are partial; nested inclusive spans must not be added, and admission time is
not queue waiting time.

Validation passed: five guest tests, 292 full VM-package tests (two skipped),
24 executor tests excluding control and 24 benchmark tests, plus guest Clippy and
benchmark Ruff. The wider executor subset in the current worktree has nine
VM-control test failures, retained separately in the validation archive. This
change does not modify control implementation or count those checks as passed.

Build both GNU/Linux binaries from the same source, applying only the proposed
change between builds. Use a fresh output directory and start with
`--samples 1 --warmups 0` for preflight.

```bash
python3 benchmark/pvisor/filesystem_ab.py \
  --assets target/reference-env-final-20261004 \
  --baseline /absolute/path/to/pvisor-before \
  --candidate /absolute/path/to/pvisor-after \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output target/filesystem-ab-new \
  --cpu-affinity 0,1 --samples 30 --warmups 3
```

[Raw sample CSV](../../assets/benchmarks/filesystem-ab-20261005/samples.csv) ·
[Protocol, binaries and P50/P95/P99](../../assets/benchmarks/filesystem-ab-20261005/summary.json) ·
[Separate diagnostics](../../assets/benchmarks/filesystem-ab-20261005/profiles.json) ·
[Reports, harness, failure and validation logs](../../assets/benchmarks/filesystem-ab-20261005/evidence.tar.gz)

## Linux: 2026-10-05, OverlayCore resolution optimization {#resolution-optimization}

This release-mode experiment measures only the virtio-fs OverlayFs adapter,
without a VM, host FUSE mount or preimage journal. Fixtures live on `/tmp`
tmpfs: 32 directories with 64 files of 18 B each; deep paths have eight parent
components. Each trial has fresh inode tables and warm host caches. Fixture and
adapter construction are excluded from timing.

Preserved baseline and candidate test binaries were run through nextest binaries
metadata in three alternating batches: old→new, new→old, old→new. Each case and
batch has two warmups, eight unprofiled samples and one diagnostic sample. P50
uses only the 24 unprofiled samples per version. CPU affinity was not pinned;
binary digests and individual batch medians are retained in the raw summary.

| Adapter operation, 2,048 files | Before P50 ms | After P50 ms | Elapsed reduction |
|---|---:|---:|---:|
| lookup + getattr | 35.15 | 26.35 | 25.0% |
| lookup + open + getattr + release | 37.61 | 28.07 | 25.4% |
| Deep lookup + open + getattr + release | 170.51 | 96.85 | 43.2% |
| opendir + readdirplus + releasedir | 29.00 | 20.24 | 30.2% |

The shared OverlayCore now skips whiteout/opaque probes when this candidate's
physical upper parent was just found missing or non-directory. Attributes and
absence are not cached across requests; later components and final physical
ancestors are still checked afresh. Deep-path diagnostic counts fell from 38,016
to 4,352 for each marker probe. Markers behind upper ancestor symlinks cannot
hide lower entries. New tests also cover upper directories and whiteouts appearing
within a walk and upper content changes between requests.

All 106 Core/host-adapter tests and 57 virtio-fs/descriptor/filesystem-snapshot
tests passed, as did Clippy for all targets of the three relevant packages. The initial
sandbox hid `/dev/kvm` and `/dev/fuse`, so that full VM-package attempt failed
at KVM initialization. This microbenchmark did not measure actual VM/FUSE jobs,
journaling, payload I/O or macOS. Subsequent host validation and end-to-end runs
appear in the [P0 baseline above](#e2e-baseline). Unrelated VM refactors
also occurred between builds; this adapter case does not execute UART/VMM/CPU
initialization. Binary digests identify the measured artifacts. These reductions
do not replace new measurements of the historical tool workloads below.

Reproduce one version's adapter measurement:

```bash
cargo nextest run --locked --release -p pvisor-vm \
  --run-ignored only --no-capture -E 'test(small_file_adapter_benchmark)'
```

[Samples and diagnostic counts](../../assets/benchmarks/overlay-resolution-20261005/samples.json) · [Artifacts, protocol and summary](../../assets/benchmarks/overlay-resolution-20261005/summary.json)

## Linux: 2026-10-04 {#results}

### Tool execution inside complete Ubuntu {#full-ubuntu}

The table excludes environment boot, measuring operations and grading. This new batch uses N=10 with 3 warmups, the same fixture and two-core budget, and 16 GiB VMs. pVisor uses host directories and staged virtio-fs; Ubuntu uses its vendor generic kernel, distribution tools and private ext4. The old Docker N=30 matrix remains separate without pooling percentiles.

| Workload | Native P50/P95 ms | pVisor staged P50/P95 ms | pVisor VM P50/P95 ms | Ubuntu P50/P95 ms |
|---|---|---|---|---|
| metadata | 4.85 / 5.11 | 177.80 / 195.52 | 291.82 / 359.75 | 18.64 / 19.90 |
| read | 33.29 / 48.97 | 48.12 / 61.21 | 88.83 / 127.21 | 115.51 / 117.69 |
| write | 3.75 / 4.43 | 186.95 / 208.86 | 134.37 / 154.03 | 36.07 / 36.35 |
| git | 14.66 / 17.07 | 172.18 / 194.52 | 613.69 / 807.73 | 136.50 / 140.81 |
| rg | 7.58 / 10.90 | 140.52 / 145.52 | 521.65 / 554.49 | 20.40 / 22.89 |
| cargo | 52.79 / 57.17 | 104.21 / 131.18 | 563.24 / 658.37 | 969.09 / 977.41 |
| npm | 218.82 / 245.85 | 256.29 / 292.67 | 2260.17 / 2408.63 | 1079.89 / 1110.93 |

These values locate waiting in traversal, search, compilation and installation. A single read does not establish that block devices always outperform FUSE: kernels, tool versions, storage and staging semantics differ together. For longer tasks, combine worker time with the [complete loop](agent-tasks.md#full-ubuntu) rather than startup alone.

[Per-sample CSV](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [Distributions and phases](../../assets/benchmarks/full-ubuntu-20261004/summary.json) · [Method and reproduction](methodology.md#full-ubuntu)

### Docker baseline in the complete tool environment {#reference-fs}

This adds a same-host Docker Engine measurement rather than relabeling Podman. Same two-core budget and inputs, 30 samples and 3 warmups per case; fixtures match the first edition, with seven operations executed sequentially in each fresh environment. Timings cover operations and validation, excluding environment startup. See the [complete environment](agent-tasks.md#reference-env) for the repair workflow. The two batches remain separate; percentiles are not pooled.

| Workload | Native P50/P95 ms | Docker P50/P95 ms | pVisor staged P50/P95 ms | pVisor VM P50/P95 ms |
|---|---|---|---|---|
| metadata | 5.04 / 6.78 | 5.06 / 7.50 | 180.13 / 202.88 | 310.54 / 351.59 |
| read | 33.07 / 41.44 | 33.24 / 45.06 | 48.77 / 61.60 | 89.27 / 99.34 |
| write | 3.96 / 5.09 | 3.95 / 5.65 | 189.28 / 204.67 | 144.66 / 164.67 |
| git | 15.75 / 20.50 | 16.07 / 20.07 | 177.81 / 195.97 | 456.21 / 786.35 |
| rg | 7.98 / 11.81 | 8.03 / 9.63 | 144.61 / 159.00 | 545.82 / 673.25 |
| cargo | 58.71 / 70.16 | 56.40 / 73.75 | 112.80 / 140.81 | 549.57 / 659.23 |
| npm | 183.46 / 197.57 | 231.45 / 288.60 | 222.97 / 256.33 | 1727.04 / 1978.79 |


This establishes a concrete position: Docker metadata/read/write stay close to native. Staged small-file traversal adds about **175 ms** over Docker, and a 64 MiB read about **16 ms**. An individual read costs tens of additional milliseconds; repeated small-file scans accumulate. VM offline npm installation takes about **1.73 seconds**, against Docker's **0.23 seconds**, a clear remaining gap. The roughly 86 ms VM startup figure cannot stand in for this tool budget.

Tasks verify file counts/sizes, SHA256, clean Git state, search matches, compiled output, and installed package counts. Docker uses a writable bind mount, pVisor a staged view, and Firecracker/QEMU private ext4. Different file paths are part of actual deployment cost; this is not a causal experiment changing only the VMM over an identical filesystem. The summary also contains each operation's distribution for other runtimes.

[Configuration and reproduction](methodology.md#reference-env) · [逐样本 CSV](../../assets/benchmarks/reference-env-20261004/samples.csv) · [分布与阶段计时](../../assets/benchmarks/reference-env-20261004/summary.json) · [原始证据](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [兼容性矩阵](../../assets/benchmarks/reference-env-20261004/compatibility.json)


### First-edition Linux matrix: separate batch

| Workload | Backend | N | Worker P50 ms | vs native | Wall P50 / P95 / P99 ms |
|---|---|---|---|---|---|
| metadata | native | 30 | 4.84 | +0.0% | 23.44 / 29.17 / 30.71 |
| metadata | host | 30 | 4.94 | +2.0% | 34.02 / 44.90 / 58.18 |
| metadata | staged | 30 | 149.60 | +2988.5% | 225.03 / 295.08 / 314.55 |
| metadata | safe | 30 | 171.26 | +3435.6% | 288.73 / 346.47 / 363.59 |
| metadata | vm | 30 | 263.08 | +5331.3% | 636.44 / 828.38 / 892.94 |
| metadata | podman | 30 | 4.95 | +2.1% | 138.99 / 158.58 / 159.34 |
| metadata | container | 30 | 4.83 | -0.3% | 320.21 / 419.26 / 479.51 |
| read | native | 30 | 32.66 | +0.0% | 52.81 / 125.98 / 143.36 |
| read | host | 30 | 31.80 | -2.6% | 64.65 / 213.37 / 232.88 |
| read | staged | 30 | 40.76 | +24.8% | 95.24 / 338.92 / 378.04 |
| read | safe | 30 | 41.38 | +26.7% | 153.98 / 386.69 / 390.96 |
| read | vm | 30 | 100.58 | +208.0% | 476.56 / 1379.77 / 1436.40 |
| read | podman | 30 | 32.68 | +0.1% | 170.64 / 408.95 / 452.05 |
| read | container | 30 | 33.57 | +2.8% | 405.55 / 945.63 / 1092.93 |
| write | native | 30 | 3.81 | +0.0% | 22.95 / 26.54 / 55.29 |
| write | host | 30 | 3.81 | -0.1% | 34.16 / 46.23 / 93.69 |
| write | staged | 30 | 192.46 | +4945.6% | 252.61 / 444.20 / 812.30 |
| write | safe | 30 | 199.56 | +5131.6% | 312.83 / 549.31 / 762.48 |
| write | vm | 30 | 114.68 | +2906.4% | 476.94 / 694.88 / 1003.73 |
| write | podman | 30 | 3.76 | -1.4% | 139.68 / 151.53 / 340.87 |
| write | container | 30 | 3.82 | +0.1% | 356.89 / 406.97 / 627.19 |
| git | native | 30 | 14.72 | +0.0% | 36.14 / 79.21 / 165.50 |
| git | host | 30 | 14.51 | -1.4% | 54.34 / 65.11 / 92.63 |
| git | staged | 30 | 203.33 | +1281.7% | 333.16 / 458.84 / 1316.66 |
| git | safe | 30 | 225.99 | +1435.7% | 394.38 / 491.36 / 960.78 |
| git | vm | 30 | 535.06 | +3536.0% | 929.22 / 1558.97 / 2447.31 |
| git | podman | 30 | 15.55 | +5.7% | 158.87 / 209.35 / 215.16 |
| git | container | 30 | 15.78 | +7.2% | 375.18 / 434.76 / 490.77 |
| rg | native | 30 | 6.72 | +0.0% | 26.54 / 28.70 / 29.90 |
| rg | host | 30 | 6.64 | -1.2% | 44.01 / 44.79 / 45.02 |
| rg | staged | 30 | 167.90 | +2399.9% | 288.07 / 312.76 / 320.23 |
| rg | safe | 30 | 190.82 | +2741.2% | 349.80 / 380.69 / 389.61 |
| rg | vm | 30 | 340.98 | +4976.9% | 717.00 / 769.33 / 780.18 |
| rg | podman | 30 | 5.90 | -12.2% | 141.36 / 154.73 / 160.82 |
| rg | container | 30 | 6.39 | -4.8% | 355.33 / 390.96 / 395.69 |
| cargo | native | 30 | 49.09 | +0.0% | 67.21 / 84.93 / 86.36 |
| cargo | host | 30 | 47.07 | -4.1% | 83.86 / 94.88 / 95.49 |
| cargo | staged | 30 | 90.31 | +84.0% | 146.28 / 167.31 / 168.42 |
| cargo | safe | 30 | 96.00 | +95.6% | 188.83 / 209.83 / 224.90 |
| cargo | vm | 30 | 541.56 | +1003.1% | 897.61 / 959.14 / 959.43 |
| npm | native | 30 | 157.56 | +0.0% | 175.60 / 205.84 / 434.16 |
| npm | host | 30 | 156.38 | -0.7% | 184.73 / 204.95 / 552.20 |
| npm | staged | 30 | 187.87 | +19.2% | 235.76 / 256.13 / 608.48 |
| npm | safe | 30 | 232.30 | +47.4% | 318.58 / 432.25 / 708.12 |
| npm | podman | 30 | 208.41 | +32.3% | 339.64 / 474.65 / 857.37 |
| npm | container | 30 | 195.29 | +23.9% | 515.88 / 552.03 / 891.62 |

### Analysis

Staged sequential reads cost about +25% and offline npm +19%. Metadata is about 31× native and small-file writes about 50×, adding roughly 150–190 ms to native operations lasting only milliseconds. Host worker time is near native; full jobs include CLI/recording overhead. Safe adds namespace/policy/proxy setup. Most measured VM jobs take roughly 0.5–1 second.

This historical batch uses Podman as the OCI control; Docker daemon access was unavailable then. A later batch adds rootless Docker Engine bind-mount measurements, shown in the complete-environment comparison above. Docker overlay2 and Docker Desktop remain unmeasured. pVisor OCI prepares a private rootfs per Job; wall time includes this while worker time isolates tool execution.

### Compatibility follow-up

Container cargo initially lacked linker startup files, a fixture-image error. After adding glibc/GCC files, Fedora linker scripts still required /lib64/libmvec.so.1. The final cargo-ready batch supplies that path and passes every sample, listed separately; both setup failures remain in reports. VM Node failed to reserve V8 address space with the 1 GiB shape; follow-up uses a **16 GiB address-space configuration**, kept separate from the main 1 GiB batch.

| Workload | Backend | N | Worker P50 / P95 / P99 ms | Wall P50 / P95 / P99 ms |
|---|---|---|---|---|
| cargo | native | 30 | 52.90 / 65.79 / 76.17 | 71.96 / 89.41 / 103.04 |
| cargo | host | 30 | 52.60 / 70.49 / 75.30 | 84.38 / 111.79 / 124.56 |
| cargo | staged | 30 | 116.07 / 139.93 / 233.90 | 166.65 / 210.57 / 397.55 |
| cargo | safe | 30 | 119.77 / 138.77 / 228.23 | 209.92 / 242.16 / 342.22 |
| cargo | vm | 30 | 567.42 / 1365.43 / 2047.17 | 1271.15 / 2683.43 / 3889.27 |
| npm | native | 30 | 162.86 / 433.44 / 488.04 | 181.47 / 472.60 / 531.93 |
| npm | host | 30 | 163.56 / 430.53 / 475.14 | 205.34 / 522.04 / 595.23 |
| npm | staged | 30 | 213.17 / 437.03 / 612.15 | 266.48 / 544.87 / 780.57 |
| npm | safe | 30 | 256.70 / 717.56 / 777.67 | 339.63 / 983.34 / 1085.49 |
| npm | vm | 30 | 985.29 / 3238.12 / 4617.06 | 1661.91 / 5172.18 / 5755.11 |
| npm | podman | 30 | 217.06 / 547.44 / 833.45 | 365.17 / 883.42 / 1239.59 |
| npm | container | 30 | 202.48 / 587.65 / 601.74 | 550.89 / 1368.06 / 1469.75 |


| Cargo corrected /lib64 image | N | Worker P50/P95/P99 ms | Wall P50/P95/P99 ms |
|---|---|---|---|
| native | 30 | 51.52 / 59.70 / 62.44 | 70.85 / 80.25 / 82.02 |
| podman | 30 | 49.29 / 54.35 / 55.75 | 181.18 / 187.71 / 189.70 |
| container | 30 | 51.99 / 57.94 / 59.96 | 405.08 / 425.10 / 425.46 |

## Limits and next measurements {#acceptance}

Main VM jobs use host rootfs `/`, 2 vCPU/1 GiB and a host read view. This is a tool compatibility profile; host-secret protection is tested separately in [isolation](isolation-tests.md). These small offline warm-cache tasks do not establish full-repository build performance or cold-disk throughput. macFUSE/FSKit, actual registry installs and Docker/overlay2 await comparable measurements.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites filesystem --samples 30 --warmups 3
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
