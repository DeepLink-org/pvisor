# Agent tool loop: real CLIs, controlled responses

In a complete Python/Node/Rust environment, repair/testing takes **0.50 s** natively, **0.70 s** with pVisor staged, **0.90 s** in Docker, and **3.97 s** in pVisor VM at P50. Staging adds about 0.20 s; the VM still trails containers and reference VMs. Real Codex loops pass, while Claude Code initialization times out on this pVisor VM artifact.

## Motivation

To detect interference, first fix tool actions and remove inference/Internet variability, then measure real-model tasks. This supplies a repeatable tool-loop baseline.

## Experiment design {#interpretation}

Pin client versions, project inputs and local model responses to compare environment/tool costs across native, staged, container and VM paths. Require correct repairs, zero-exit tests, returned real tool results and client completion; staging must preserve the original faulty file. Fake credentials, no inference or paid calls. The complete environment uses 30 samples, 3 warmups and randomized order per case; historical arithmetic tasks use six inputs, three repetitions, no warmups and fixed ordering. Report them separately without pooling distributions.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

### Complete Agent Env: same host, tools, and familiar baselines {#reference-env}

The deployed environment contains Python 3.14.7, Node 24.18.0/npm, Rust/Cargo 1.98.1, Git, rg, Claude Code 2.1.128, and Codex 0.160.0: about 3.58 GiB and 76,000 files. Native, Docker and VMs use the same tool artifacts, projects and checks. Each case has 30 samples and 3 warmups, a two-core budget, and 2 vCPU / 16 GiB for complete VMs. Full [parameters and boundaries](methodology.md#reference-env) are stated separately.

The workflow inspects a project, searches for the bug, repairs Python, runs Python/Rust/Node tests, installs 32 local dependencies, and generates a diff. Fixed model responses request that plan. Real CLIs must return actual passing tool results and exit normally. This covers environment deployment and tool execution, not large repositories, real model success rates or Internet dependency installation.

![P50/P95 complete environment and real CLI workflows](../../assets/benchmarks/reference-env-20261004/reference-workflows.svg)

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


VM npm installation takes about 1.74 s, its largest phase here; Rust compilation takes about 0.84 s. Together with the [file-operation comparison](filesystem.md#reference-fs), this identifies dense small-file operations and offline installation as priorities for investigation. Guest configuration, filesystems and initialization also differ; the table locates costs rather than establishing a single cause.

**Compatibility is a measured result.** Claude passes 30/30 on native/staged/Docker/Firecracker/QEMU. Its pVisor VM preflight exceeds 90 s during initialization without a completed tool loop; formal N=0. Logs and diagnostics are retained; the root cause remains unresolved. Codex passes 30/30 on all eight backends with uniform inner `danger-full-access`, leaving the declared boundary to the outer runtime. This does not establish default `workspace-write` support everywhere. Native and staged retain access outside the workspace.

Do not rank Claude against Codex by absolute time. Codex also has seconds of client waiting in this path: roughly 6.26 s in Docker and 10.93 s in pVisor VM. These numbers describe the complete client experience more closely than bare startup, for these versions and controlled tasks, not model speed.

**Deployment is separate.** Offline tool copying, image import and ext4 creation take about 109 s; adding the CPU affinity helper takes another 5.3 s. Downloads and kernel compilation are excluded. Per-run workspace/private-disk preparation is recorded as `prepare_ms`, outside the task table. These are prepared-environment task budgets, not first-install-to-completion costs. See the [memory audit](methodology.md#reference-resources); 16 GiB configured RAM does not mean each Agent consumes 16 GiB resident memory.

[Per-sample CSV](../../assets/benchmarks/reference-env-20261004/samples.csv) · [Distributions and phase timing](../../assets/benchmarks/reference-env-20261004/summary.json) · [Runtime evidence](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [Compatibility matrix](../../assets/benchmarks/reference-env-20261004/compatibility.json)

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

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
