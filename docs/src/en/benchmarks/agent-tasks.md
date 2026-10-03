# Agent tool loop: real CLIs, controlled responses

All 72 controlled tasks passed with real Claude Code/Codex CLIs. Claude staging adds about 44 ms; the Codex difference is within this batch's variation. This measures tool-loop compatibility and overhead. Real model problem-solving differences remain unmeasured.

## Motivation

To detect interference, first fix tool actions and remove inference/Internet variability, then measure real-model tasks. This supplies a repeatable tool-loop baseline.

## Experiment design {#interpretation}

Claude Code 2.1.128 and Codex 0.160.0 use a same-host deterministic response server. It requests repairing adder.py/running grade.py, then ends after receiving GRADE_PASS. Six arithmetic-input fixtures, three independent repetitions; 18 planned per CLI/backend, no warmups. Each has ten assertions, not six distinct bug classes. Fake credentials, no inference or paid calls. Synthetic usage fields test protocol only and are not real tokens/billing.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

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
