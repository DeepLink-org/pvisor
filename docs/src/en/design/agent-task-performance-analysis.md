# Agent tool tasks: technical evidence

Complete repair/testing takes about **4.61 s** in image-free pVisor VM and **8.51 s** on Firecracker / complete Ubuntu; staging takes **0.72 s**. Ubuntu boot affects short jobs, while pVisor retains substantial tool-execution costs: fast startup does not establish leading tools. Claude/VM: initialization timeout / N=0; real Codex tool-loop results follow below.


## SWE trajectory: pVisor VM and AgentENV {#swe-trajectory}

**Results on my server:** the 44-command SymPy replay takes a median **35.898 s / 24.611 s** in pVisor VM / AgentENV. A separate filesystem diagnostic records **13.682 s / 3.474 s** of file-call intervals under tracing, with the largest gaps in open/close and metadata queries. The two cohorts have different measurement conditions and are not subtracted or combined.

This uses the fixed `sympy__sympy-15599` trajectory from [PR #23](https://github.com/DeepLink-org/pvisor/pull/23) with the immutable-lower caching merged in [PR #24](https://github.com/DeepLink-org/pvisor/pull/24), compared with AgentENV v0.2.0. PR #23 supplies the replay method, not the AgentENV implementation. The purpose is to locate costs in a real tool sequence; it does not measure model inference, task success rates, or the full SWE-bench suite. Registered as **B-FS-DIAG**.

### Overall tool-call time {#swe-tool-time}

Seconds for all 44 command subprocesses, including shell setup, computation, I/O and waiting; three complete replays per backend. VM startup/shutdown, `stage::seal`, result export, model waits and pause/resume/offload are outside this timer.

| Run | pVisor VM + PR #24 | AgentENV |
| --- | ---: | ---: |
| 1 | 34.037 | 27.289 |
| 2 | 37.223 | 24.611 |
| 3 | 35.898 | 22.351 |
| Median | **35.898** | **24.611** |

The observed medians differ by 11.287 s (ratio 1.46). The descriptive paired-bootstrap 95% interval for the median paired difference is [6.747, 13.547] s; three pairs do not establish stable uncertainty or a general ranking. All 264 steps passed output/source/final-patch checks; no valid slow run was removed.

The explicit environment variables match, but inherited Conda state differs: pVisor's shell setup executes two additional `dirname` commands per step. These are end-to-end observations of the configurations as run, not an attribution of the whole gap to the filesystem. The independent diagnostic below clears inherited variables and applies the same canonical environment to both backends.

### Filesystem-call time {#swe-file-time}

One traced replay per backend, using identical private strace binaries and libraries. Seconds are the union of file-syscall intervals across threads within each command, summed over 44 commands. Category intervals may overlap; this is observed interval coverage, not a removable critical-path contribution.

| File operation | pVisor VM + PR #24 | AgentENV |
| --- | ---: | ---: |
| All identified file-call intervals | **13.682** | **3.474** |
| Open / close | 6.450 | 0.553 |
| Metadata / directory queries | 4.810 | 1.861 |
| Read | 1.044 | 0.539 |
| FD operations (`lseek`, `fcntl`, `ioctl`) | 1.361 | 0.519 |
| Write | 0.007 | 0.002 |
| Namespace / attribute changes | 0.013 | 0.001 |
| Explicit synchronization calls | 0 | 0 |

The traced interval totals differ by 10.208 s (ratio 3.94). Open/close and metadata/directory queries have the largest observed gaps: 5.897 s and 2.949 s. File-call counts are similar overall: 435,990 / 434,746. `openat` counts are 35,534 / 35,533, `read` counts 50,137 / 50,137, and `stat` counts 112,805 / 112,805. This supports investigating the latency of common operations, rather than assuming a large overall request-count increase. Guest syscall counts are not virtio-fs request counts.

Rootfs-path interval coverage is 8.147 / 2.251 s; workspace-path coverage is 5.535 / 1.223 s. These are pathname classes, not separate devices. Close latency does not establish `fsync` cost: pVisor's `flush_leased` uses dup/close, while explicit synchronization is a separate path. This diagnostic does not attribute time between host checks, VM transport, guest scheduling and required access-control/preimage work.

Tracing calibration uses one untraced and one traced replay per backend:

| Backend | Untraced command total (s) | Traced command total (s) | Observed increase |
| --- | ---: | ---: | ---: |
| pVisor VM | 39.237 | 46.024 | 17.3% |
| AgentENV | 24.974 | 33.104 | 32.6% |

Both exceed the predefined 5% perturbation gate. The file-call table is therefore an **instrumented diagnostic**, not an estimate of untraced filesystem time or of the earlier end-to-end gap. File-backed `mmap` setup and `exec` are recorded separately; mapped-file demand faults are not timed. Pseudo-files, pipes/sockets and measurement-log paths are excluded; unresolved-path intervals (0.026 / 0.022 s) are also excluded. All 176 steps across the four calibration/traced runs passed checks, and all 88 traces parsed without unresolved errors.

### Configuration and evidence {#swe-evidence}

Measured on 2026-10-13: 2 vCPUs / 2 GiB per guest; host CPUs 0,1, two-core quota, 24 GiB and zero swap. pVisor uses Linux 6.12.109; AgentENV uses 6.1.175 and official cold start. Both use the same source OCI, Python 3.9.20, repository baseline and original commands, with fresh writable state per replay. Preflight warms some files; host caches are not cleared. Measurement logs use guest tmpfs and are exported outside the command timer. End-to-end and diagnostic tmpfs capacities are 32 and 384 MiB respectively.

pVisor uses merged commit `b5e27a6670c37120592409bf3fd6ec36cdf9b38c`. The driver declares its private physical lower immutable through `OverlayHint`; `root-data` caching and default metadata caching are enabled. All guest writes enter a private upper; no apply occurs. The whole-root stage includes `/testbed`, so its initial lower files are eligible for this contract too. No new no-open, long-TTL or DAX experiment is included. This comparison does not measure the incremental speedup from PR #24.

[Processed tables and syscall summaries](swe-trajectory-comparison.csv) · [Configuration, artifact and evidence hashes](../assets/benchmarks/swe-trajectory-20261013-provenance.json).
Frozen runners, commands, per-step outputs, raw traces and verification scripts are retained in the local evidence archives under `benchmark/.data/swe-comparison-20261013`; these are not public download links. The original replay setup is documented in [PR #23's README](https://github.com/DeepLink-org/pvisor/blob/2cdefa8d38f78960fc5cdec5c65bb59348c3f883/benchmark.new/trajectory/README.md); the archived cross-backend harness additionally pins the immutable lower, tmpfs logging and diagnostic environment. This documentation change adds no performance samples or product changes.


## Motivation

To detect interference, first fix tool actions and remove inference/Internet variability, then measure real-model tasks. This supplies a repeatable tool-loop baseline.

## Experiment design {#interpretation}

Pin client versions, project inputs and local model responses to compare environment/tool costs across native, staged, container and VM paths. Require correct repairs, zero-exit tests, returned real tool results and client completion; staging must preserve the original faulty file. Fake credentials, no inference or paid calls. The complete Ubuntu comparison uses 10 samples per available case; the historical shared-tool environment uses 30, both with 3 warmups and randomized order. Historical arithmetic tasks use six inputs, three repetitions, no warmups and fixed ordering. Report them separately without pooling distributions.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](../benchmarks/startup.md) and [VM memory](../benchmarks/vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

### Image-free pVisor and complete Ubuntu: full Agent Env {#full-ubuntu}

pVisor reuses installed host Python/Node/Rust/Git/rg/Claude/Codex without making an OS image. Firecracker/QEMU install distribution tools and the same Rust/Agent CLIs in complete official Ubuntu 26.04.1 LTS. Each trial creates a new environment, then runs the same repair plan and tests. VMs use 2 vCPU / 16 GiB; all groups share the two-core budget and warm host caches, with 10 planned samples, 3 warmups and randomized order; cells state effective N when below 10. Ubuntu Python/Node/Git versions differ from the host; full versions and kernel differences are in the [method](../benchmarks/methodology.md#full-ubuntu).

![Complete Ubuntu and image-free pVisor tool loops](../assets/benchmarks/full-ubuntu-qemu-20261004/ubuntu-workflows.svg)

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native / Fedora | 0.16 / 0.17 | 0.52 / 0.55 | 0.92 / 1.07 | 2.03 / 3.12 |
| pVisor staged | 0.18 / 0.19 | 0.72 / 0.78 | 1.15 / 1.58 | 2.33 / 2.44 |
| pVisor VM / host | 1.21 / 1.77 | 4.61 / 5.09 | FAILED / N=0 | 11.39 / 13.73 |
| Firecracker / Ubuntu | 7.79 / 10.13 | 8.51 / 9.11 | 9.78 / 9.87 | 10.81 / 13.49 |
| QEMU q35 / Ubuntu | — | 8.12 / 8.23 | 9.50 / 10.78 | 10.20 / 11.63 |
| QEMU microvm / Ubuntu | — | 10.33 / 10.47 | 11.59 / 14.07 | 13.04 / 14.06 |

The QEMU rows use the same complete Ubuntu template; all 60/60 new formal task samples pass, with N=10 and 3 warmups per case. Standalone version self-check timing is unmeasured (—). QEMU and the earlier pVisor/Firecracker cohorts are separate; the chart does not pool distributions.

**Repair/testing has lower launch-to-result time; tools after boot do not have the same advantage.** Repair/testing is 4.61 s end to end in pVisor VM versus 8.51 s on Firecracker/Ubuntu; internal tool/grading time is 4.02 / 2.30 s respectively. Avoiding complete OS boot reduces waiting; worker time exposes tool/storage costs. If a VM executes multiple tasks, boot is amortized and worker time matters more. Resident pools and sustained throughput are unmeasured.

| Backend | Repair worker P50/P95 s | Peak tree RSS P50/P95 MiB |
|---|---|---|
| Native / Fedora | 0.48 / 0.50 | 169.28 / 190.85 |
| pVisor staged | 0.67 / 0.72 | 209.52 / 248.59 |
| pVisor VM / host | 4.02 / 4.49 | 729.46 / 756.93 |
| Firecracker / Ubuntu | 2.30 / 2.69 | 1743.02 / 1766.67 |
| QEMU q35 / Ubuntu | 2.75 / 2.79 | 1862.13 / 1876.98 |
| QEMU microvm / Ubuntu | 2.81 / 2.88 | 1844.82 / 1881.06 |

| Phase | Native P50 ms | staged P50 ms | pVisor VM P50 ms | Firecracker Ubuntu P50 ms | QEMU q35 P50 ms | QEMU microvm P50 ms |
|---|---|---|---|---|---|---|
| inspect | 3.1 | 18.6 | 74.4 | 17.1 | 16.4 | 18.7 |
| search | 2.2 | 2.4 | 20.8 | 12.9 | 7.6 | 7.4 |
| python-tests | 28.0 | 34.2 | 149.7 | 45.3 | 47.2 | 47.8 |
| rust-tests | 96.5 | 206.7 | 822.3 | 981.6 | 1299.9 | 1327.5 |
| node-install | 221.9 | 260.9 | 2210.9 | 876.7 | 956.7 | 975.9 |
| node-tests | 126.3 | 140.3 | 595.2 | 389.6 | 421.9 | 423.1 |
| diff | 0.9 | 2.8 | 18.6 | 1.2 | 1.7 | 1.4 |

The largest pVisor VM phase is `node-install`, at about 2.21 seconds P50. This identifies a concrete optimization path. Phase medians do not sum to the total median, and ext4/virtio-fs differences do not independently establish the entire cause.

Repair covers inspection, rg search, Python repair, Python/Rust/Node tests, installing 32 local npm dependencies and a diff. CLI columns use real clients with fixed local model responses and require passing test output in the returned model request and normal completion, without simulated CLIs or real inference. Staged/VM trials also verify the unchanged source workspace, completed Bundle and requested executor. All groups use private workspace temporary files to accommodate hostroot read-only `/tmp`. Initial linker failures from the incorrect configuration are retained as diagnostics outside formal distributions.

Claude/VM preflight still exceeds 90 seconds during initialization, with formal N=0. This is not a completed loop or evidence of full compatibility. Firecracker/Ubuntu Claude/Codex values use a separate ten-sample follow-up after correcting serial prompt and terminal control interference, with identical parameters. They are not pooled with the original client samples; collection failures remain archived.

Codex uses uniform inner `danger-full-access`, with the outer runtime supplying its stated boundary; default nested sandbox compatibility is unmeasured. N=10 gives first-version budgets and repeatability, not long-term tail or real-model success guarantees. RSS sums the launcher tree every 20 ms, including separate Firecracker DNS/NAT support; QEMU uses built-in user networking. Shared pages may be counted twice and short peaks missed. Configured 16 GiB is not 16 GiB resident, and short-task RSS does not establish concurrency capacity.

Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-20261004/samples.csv` · Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-20261004/summary.tsv` · [Method and reproduction](../benchmarks/methodology.md#full-ubuntu) · Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-20261004/ubuntu-workflows-private-tmp-20261004/evidence.tar.gz` · Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-20261004/ubuntu-clients-console-v2-20261004/evidence.tar.gz`

Complete-Ubuntu repair/testing takes **8.12 s** on q35 and **10.33 s** on microvm, with internal tools taking **2.75 / 2.81 s**. Image-free pVisor reduces waiting for fresh jobs, while its 4.02 s worker time remains above these complete-Ubuntu paths, a priority when environments are reused. Selecting microvm does not automatically improve complete tasks in this configuration. Networking, devices and exposed CPU features differ; the entire difference cannot be assigned to block devices or FUSE.

Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-qemu-20261004/manifest.tsv` · Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-qemu-20261004/summary.tsv` · Local raw record `docs/src/assets/benchmarks/.data/full-ubuntu-qemu-20261004/ubuntu-qemu-complete-20261004/evidence.tar.gz` · [QEMU method](../benchmarks/methodology.md#full-ubuntu-qemu)

### Historical controlled environment: shared tools and trimmed reference VMs {#reference-env}

This batch shares tool artifacts to control some version differences. Firecracker/QEMU use trimmed Linux 6.12.109 and static init, without a complete distribution boot. pVisor shares a tool directory through virtio-fs without an image. These results retain Docker and minimal-VM context; see the [complete Ubuntu and image-free hostroot deployment](#full-ubuntu) above.

The deployed environment contains Python 3.14.7, Node 24.18.0/npm, Rust/Cargo 1.98.1, Git, rg, Claude Code 2.1.128, and Codex 0.160.0: about 3.58 GiB and 76,000 files. Native, Docker and VMs use the same tool artifacts, projects and checks. Each case has 30 samples and 3 warmups, a two-core budget, and 2 vCPU / 16 GiB for complete VMs. Full [parameters and boundaries](../benchmarks/methodology.md#reference-env) are stated separately.

The workflow inspects a project, searches for the bug, repairs Python, runs Python/Rust/Node tests, installs 32 local dependencies, and generates a diff. Fixed model responses request that plan. Real CLIs must return actual passing tool results and exit normally. This covers environment deployment and tool execution, not large repositories, real model success rates or Internet dependency installation.

![P50/P95 complete environment and real CLI workflows](../assets/benchmarks/reference-env-20261004/reference-workflows.svg)

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native | 0.15 / 0.17 | 0.50 / 0.78 | 0.82 / 0.88 | 1.97 / 2.49 |
| pVisor host | 0.16 / 0.17 | 0.50 / 0.61 | 0.85 / 0.91 | 1.94 / 2.25 |
| pVisor staged | 0.17 / 0.19 | 0.70 / 1.10 | 1.07 / 1.27 | 2.25 / 2.44 |
| pVisor VM | 1.40 / 1.52 | 3.97 / 6.79 | FAILED / N=0 | 10.93 / 12.93 |
| Docker rootless | 0.46 / 0.53 | 0.90 / 1.45 | 1.23 / 1.33 | 6.26 / 6.59 |
| Firecracker PCI | 1.48 / 1.59 | 2.25 / 3.13 | 3.03 / 3.21 | 7.83 / 8.47 |
| QEMU q35 | 0.92 / 1.09 | 1.98 / 3.11 | 2.67 / 3.67 | 7.69 / 8.46 |
| QEMU microvm | 0.85 / 1.07 | 1.85 / 2.48 | 2.71 / 3.29 | 7.67 / 8.34 |


Version checks establish that tools launch. The repair column includes environment startup, repair, testing and result validation. CLI columns add client initialization, local protocol requests and returned tool output. The endpoint is the host receiving a validated result, followed by checks of zero exit, Run Bundle and files. Exit/unmount time is recorded separately. Bars show P50 and lines P95; failed cases have no latency sample. Phase medians do not sum to the total median.

**The staged path fits an interactive tool budget; the VM path still needs substantial work.** Staged repair adds about 0.20 s over native and takes about 0.20 s less than Docker. It supplies a separate change view and Run Bundle; Docker uses a writable mount, so the workflows are not identical. The VM's 3.97 s is about 4.4 times Docker and 2.1 times QEMU microvm. Roughly 86 ms startup does not imply equally fast task execution.

| Phase | Native ms | Docker ms | pVisor staged ms | pVisor VM ms | QEMU microvm ms |
|---|---|---|---|---|---|
| inspect | 2.9 | 4.2 | 18.4 | 60.3 | 20.4 |
| search | 2.3 | 2.3 | 2.4 | 18.9 | 8.6 |
| python-tests | 25.5 | 53.3 | 31.9 | 118.3 | 57.6 |
| rust-tests | 99.5 | 99.1 | 213.2 | 835.8 | 533.8 |
| node-install | 183.6 | 230.3 | 219.1 | 1736.6 | 603.6 |
| node-tests | 135.8 | 125.4 | 145.7 | 466.3 | 167.8 |
| diff | 1.0 | 0.9 | 2.9 | 16.2 | 1.1 |


VM npm installation takes about 1.74 s, its largest phase here; Rust compilation takes about 0.84 s. Together with the [file-operation comparison](../benchmarks/filesystem.md#reference-fs), this identifies dense small-file operations and offline installation as priorities for investigation. Guest configuration, filesystems and initialization also differ; the table locates costs rather than establishing a single cause.

**Compatibility is a measured result.** Claude passes 30/30 on native/staged/Docker/Firecracker/QEMU. Its pVisor VM preflight exceeds 90 s during initialization without a completed tool loop; formal N=0. Logs and diagnostics are retained; the root cause remains unresolved. Codex passes 30/30 on all eight backends with uniform inner `danger-full-access`, leaving the declared boundary to the outer runtime. This does not establish default `workspace-write` support everywhere. Native and staged retain access outside the workspace.

Do not rank Claude against Codex by absolute time. Codex also has seconds of client waiting in this path: roughly 6.26 s in Docker and 10.93 s in pVisor VM. These numbers describe the complete client experience more closely than bare startup, for these versions and controlled tasks, not model speed.

**Deployment is separate.** Offline tool copying, image import and ext4 creation take about 109 s; adding the CPU affinity helper takes another 5.3 s. Downloads and kernel compilation are excluded. Per-run workspace/private-disk preparation is recorded as `prepare_ms`, outside the task table. These are prepared-environment task budgets, not first-install-to-completion costs. See the [memory audit](../benchmarks/methodology.md#reference-resources); 16 GiB configured RAM does not mean each Agent consumes 16 GiB resident memory.

Local raw record `docs/src/assets/benchmarks/.data/reference-env-20261004/samples.csv` · Local raw record `docs/src/assets/benchmarks/.data/reference-env-20261004/summary.tsv` · Local raw record `docs/src/assets/benchmarks/.data/reference-env-20261004/evidence.tar.gz` · Local raw record `docs/src/assets/benchmarks/.data/reference-env-20261004/compatibility.tsv`

### First-edition arithmetic tasks: separate historical batch

The following 72/72 results use a smaller task, another GNU artifact, and fixed ordering. Their statistics remain separate from the complete environment.

| CLI | Backend | Passed/planned | Wall P50/P95/P99 ms | P50 vs native |
|---|---|---|---|---|
| claude | native | 18/18 | 302.15 / 318.58 / 340.44 | +0.0% |
| claude | staged | 18/18 | 345.69 / 365.95 / 366.29 | +14.4% |
| codex | native | 18/18 | 1433.75 / 1543.04 / 1546.18 | +0.0% |
| codex | staged | 18/18 | 1401.23 / 1521.71 / 1521.86 | -2.3% |

### Analysis

Checks require the corrected file, passing grading observation in the next model request, normal CLI completion, and the original faulty file retained under staging. Compare native/staged within each CLI; client startup differences are not model-speed differences. Codex staged P50 is -2.3%, but native/staged order is fixed and the desktop has background work. This does not establish acceleration or an equivalence confidence interval. Inputs and server source are archived.

A deterministic 100% pass rate does not establish unchanged SWE-bench performance or a statistical success-rate interval. Coverage is these client versions and this Bash/exec_command repair path.
## Limits and next measurements {#acceptance}

Real-model SWE-bench Lite, actual tokens, complex multi-tool tasks and output variation remain unmeasured; paid accounts were not authorized. Follow-up should pin task IDs, commits, models, budgets and permissions, randomize native/staged order and retain failures/native trajectories.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites agent --samples 3 --warmups 0
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](../benchmarks/methodology.md#product-v1) · Local raw record `docs/src/assets/benchmarks/.data/product-v1-20261004/manifest.tsv` · Local raw record `docs/src/assets/benchmarks/.data/product-v1-20261004/samples.csv` · Local raw record `docs/src/assets/benchmarks/.data/product-v1-20261004/evidence.tar.gz`. Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
