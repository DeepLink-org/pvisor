# Supervision cost: measured review workflow

Reviewing 20 changed files, selectively applying 10 and dropping the other 10 takes about 25 ms P50 of machine time. The workflow is validated; human reading and decision time remain unmeasured.

## Motivation

Agent speed is only part of the experience: approvals, diff reading and conflict resolution also matter. Machine overhead and human supervision need separate evidence.

## Experiment design {#interpretation}

30 independent stages, no warmups. Each run checks all 20 review items, applies 10 paths and verifies target contents, then drops the remaining 10 and verifies unchanged lower files. Wall time is the three machine steps summed; it excludes stage generation, user waiting and reading.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

| Step | N | P50 / P95 / P99 ms |
|---|---|---|
| review_ms | 30 | 3.21 / 7.95 / 10.81 |
| apply_ms | 30 | 17.62 / 43.66 / 51.07 |
| drop_ms | 30 | 4.07 / 12.83 / 16.68 |
| wall_ms | 30 | 24.94 / 64.74 / 75.67 |

### Decisions and interpretation

Per-tool approval count depends on tool requests. Stage review can decide multiple changes together, while still requiring diff reading, conflict handling and path selection. Docker/Git can also batch review. There are no human participants here; batching files does not establish a 90% reduction in human time.

These timings support a low machine cost for an automated review flow. They do not establish user satisfaction, decision accuracy or an optimal approval policy.
### Baseline and interaction budget {#baseline-meaning}

The familiar reference workflow is inspecting changes with Git/diff and selecting files to keep. An equivalent Git review workflow was not timed in this batch. The measured roughly 25 ms covers machine work for listing, filtering, and committing, which fits within an interaction. It does not mean a person can review changes in 25 ms or establish saved human time. Human review performance still requires a separate experiment.

## Limits and next measurements {#acceptance}

A human study should fix tasks/diffs, cross over per-tool approvals, staged review and Docker+Git, and record waiting, actual decisions, incorrect accepts/rejects and conflict resolution. Publish anonymized data and intervals. Human minutes remain unmeasured until then.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites supervision --samples 30 --warmups 3
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
