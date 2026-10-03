# Replay fidelity: native prefixes and zero-execution preparation

The first version checks six native trajectory formats, prefix boundaries, exact tool arguments and zero-execution prepare-only behavior. It found and fixed Codex/OpenCode executing historical commands in prepare-only; regression checks cover both reported counts and workspace side effects.

## Motivation

Replay depends on correct boundaries, arguments and historical observations as well as parseable structure. Preparation must not silently execute commands.

## Experiment design {#interpretation}

Twenty synthetic two-tool-batch trajectories per adapter, after-step=1, three repetitions: 60 per adapter. Require one batch/one call, exact command arguments, replayed_tool_calls=0 and an empty workspace. Format profiles are pinned and differ from installed CLI versions. No model requests or tool execution; timing covers prefix preparation.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

| Adapter | Pinned format profile | Passed/planned | Preparation P50/P95/P99 ms |
|---|---|---|---|
| claude-code | claude-code/2.1.220/native-resume-v1 | 60/60 | 5.03 / 5.62 / 6.13 |
| codex | codex/0.149.0/native-responses-jsonl-v1 | 60/60 | 5.05 / 5.49 / 6.49 |
| opencode | opencode/1.17.7/native-events-jsonl-v1 | 60/60 | 5.16 / 5.42 / 6.02 |
| mini-swe-agent | mini-swe-agent/2.4.6/native-messages-v1 | 60/60 | 5.48 / 6.76 / 7.28 |
| openhands | openhands/0.53.0/native-replay-v1 | 60/60 | 5.18 / 6.10 / 7.97 |
| pi-agent | pi-agent/0.83.0/native-rpc-events-v1 | 60/60 | 5.25 / 6.08 / 7.41 |

### Finding and repair

The old generic path executed historical tools before checking PrepareOnly while reporting zero. The workspace side-effect assertion caught it. Mode is now checked before the tool loop. New Codex/OpenCode JSONL regression also preserves historical observations. **128/128** tests pass through `just test pvisor-replay`. The repaired replay executable has its own archived hash, separate from the pvisor CLI used for the other measurements.

Prefixes pass only if structure, boundaries and arguments all validate; model reply similarity is not used as a substitute.
## Limits and next measurements {#acceptance}

This does not measure next-action or reward agreement. Synthetic prefixes do not cover all real sessions, new client versions or long conversations. Model branching, reconstructed-environment reward, actual token cost and long-prefix latency remain unmeasured. Existing design/history remains in [replay design](../design/replay.md); these pass counts are not model fidelity.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites replay --samples 3 --warmups 0
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
