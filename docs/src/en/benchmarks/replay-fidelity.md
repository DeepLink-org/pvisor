# Can a recorded trajectory start a new run?

## Motivation {#motivation}

Training and reproduction need a task to continue from an accurate historical boundary. Missing tool observations or including a future action changes the model's context, and executing tools during preparation may add side effects. Use the validated pinned-format adapters to prepare context, restore file state separately, and do not rank native resume or RL pipeline speed from these numbers.

Each of seven adapters passes 60/60 checks for fixed task 0, with whole prepare-only CLI medians of 5.56–5.74 ms; no tools run and workspaces stay unchanged. Historical clusters across twenty trajectories use a different input cohort and keep their original scope, and consistency of real model continuation remains unmeasured.

## Experiment design {#interpretation}

Check whether each adapter reconstructs the recorded boundary exactly — digests, arguments, observations, no tool execution — as replay fidelity. Fidelity decides whether a recorded trajectory can serve as a real task input; higher fidelity carries more of the original context into the new run.

Each adapter receives twenty synthetic native-format trajectories, each with two tool batches, using `after-step=1` and `prepare-only`. Three repetitions shuffle adapter/task order with a fixed seed, giving 420 attempts. The table pins format profiles separately from installed Agent CLI versions.

Valid samples must preserve the source digest, exact command arguments and historical observations for the complete first batch, exclude the second action, report `replayed_tool_calls=0` with no Agent started, and retain both the pre-existing workspace file's contents and the file inventory. An independent publication audit rechecks every retained command, native prefix and workspace. Failures are counted separately rather than assigned zero latency.

The environment is Linux/x86_64 on an AMD Ryzen 7 9700X with kernel 7.2.8-200.fc44. Processes are pinned to CPUs 0 and 1; no benchmark-specific host memory cap is imposed. Host caches are not cleared and replay has no warmups. All valid slow samples remain, with no post-hoc interference exclusion. Timing spans CLI launch through prepare-only exit, including launch and teardown but excluding input generation and post-run audit. There are no model requests or tool executions.

Native Agent resume and RL pipeline prefix preparation are relevant alternatives, but neither is a matched-trajectory, matched-budget timing control here. This experiment validates preparation for pinned formats, without measuring full session restoration, identical model next actions or rewards.

## Data and analysis {#results}

Measured on 2026-10-06. N=60 per adapter, all passing; units are ms. Separated clusters have their own shares and medians.

| Adapter | Pinned format profile | Passed / planned (failed) | Lower cluster: share; median ms | Higher cluster: share; median ms | Reference P95 ms |
|---|---|---:|---|---|---:|
| claude-code | claude-code/2.1.220/native-resume-v1 | 60/60 (0) | 52/60 (86.7%); 6.51 | 8/60 (13.3%); 16.24 | 17.07 |
| codex | codex/0.149.0/native-responses-jsonl-v1 | 60/60 (0) | 49/60 (81.7%); 6.59 | 11/60 (18.3%); 14.91 | 17.82 |
| opencode | opencode/1.17.7/native-events-jsonl-v1 | 60/60 (0) | 51/60 (85.0%); 6.52 | 9/60 (15.0%); 15.20 | 15.31 |
| mini-swe-agent | mini-swe-agent/2.4.6/native-messages-v1 | 60/60 (0) | 60/60 (100%); 6.75 | — | 14.84 |
| openhands | openhands/0.53.0/native-replay-v1 | 60/60 (0) | 45/60 (75.0%); 6.22 | 15/60 (25.0%); 11.44 | 15.01 |
| pi-agent | pi-agent/0.83.0/native-rpc-events-v1 | 60/60 (0) | 47/60 (78.3%); 6.07 | 13/60 (21.7%); 11.15 | 15.07 |
| swe-agent | swe-agent/1.1.0/replay-then-live-v1 | 60/60 (0) | 51/60 (85.0%); 6.55 | 9/60 (15.0%); 14.97 | 15.20 |

Mini-SWE-Agent does not trigger the split rule, so its row reports the full-sample P50. Splitting is descriptive: each cluster contains at least 10% of samples; the largest adjacent gap is at least 20% of the overall median and exceeds three times the median adjacent gap; cluster medians differ by at least 1.5×. This does not establish the cause of slower samples. All valid slow samples remain, so preparation should not be budgeted as a fixed few milliseconds.

### Hyperfine: direct prepare-only command {#hyperfine}

This independent cohort measures one fixed synthetic trajectory (task 0) per adapter, 60 runs without warmups. Hyperfine directly launches `pvisor-replay` with `--shell=none`; Python reset and fidelity checks run outside timing for every execution. All runs must preserve exact arguments, source digests and historical observations, execute zero tools and leave the workspace unchanged. Results cover full command completion, not the twenty-fixture historical distribution above. The binary digest identifies the artifact; its correspondence to current source is unverified. Host caches are retained, CPU affinity and artifact identities are recorded in the batch provenance summary. No paired speedup or tail claim is made.

{{ table("../../../overrides/assets/benchmarks/replay-semspec.render.json") }}

[Hyperfine data CSV](../../../overrides/assets/benchmarks/data/replay/replay-semspec.csv) · [Batch provenance](../assets/benchmarks/replay-semspec.provenance.json) · [JSON benchmark manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/README.md#replay)

### Scope {#acceptance}

These results support structure, argument and observation fidelity for pinned short prefixes. Real sessions, newer CLIs, long prefixes, token costs, macOS, remote-connection recovery and subsequent model execution remain unmeasured. Tool replay and actual environment restoration need separate correctness and cost checks.

### Sources and reproduction {#run}

Complete CSV/JSON and raw records remain in their local cohorts or the documentation data archive.

[Comparison method](index.md#interpretation) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
