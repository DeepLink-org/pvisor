# Can recorded trajectories prepare a faithful execution starting point?

## Main conclusions {#conclusions}

**Prefix preparation passes for six pinned trajectory formats at P50 of about **5–5.5 ms**. Prepare-only executes no tools and leaves the workspace unchanged. This supports binding tasks to historical observations; it does not restore arbitrary remote connections or establish identical model next actions or compatibility with every newer CLI format.**

| Need | Selection implication |
|---|---|
| Bind recorded tool history | Use prefix preparation for pinned formats |
| Restore actual file state | Restore the task environment separately |
| Predict next actions or rewards | No corresponding fidelity measurement |

## Motivation {#motivation}

Replay depends on correct boundaries, arguments and historical observations as well as parseable structure. Preparation must not silently execute commands.

## Experiment design {#interpretation}

Twenty synthetic two-tool-batch trajectories per adapter, after-step=1, three repetitions: 60 per adapter. Require one batch/one call, exact command arguments, replayed_tool_calls=0 and an empty workspace. Format profiles are pinned and differ from installed CLI versions. No model requests or tool execution; timing covers prefix preparation.

These results are from Linux/x86_64; matching macOS workloads are unmeasured. Linked reports pin artifacts, cache conditions and samples.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

| Adapter | Pinned format profile | Passed/planned | Preparation P50/P95 ms |
|---|---|---|---|
| claude-code | claude-code/2.1.220/native-resume-v1 | 60/60 | 5.03 / 5.62 |
| codex | codex/0.149.0/native-responses-jsonl-v1 | 60/60 | 5.05 / 5.49 |
| opencode | opencode/1.17.7/native-events-jsonl-v1 | 60/60 | 5.16 / 5.42 |
| mini-swe-agent | mini-swe-agent/2.4.6/native-messages-v1 | 60/60 | 5.48 / 6.76 |
| openhands | openhands/0.53.0/native-replay-v1 | 60/60 | 5.18 / 6.10 |
| pi-agent | pi-agent/0.83.0/native-rpc-events-v1 | 60/60 | 5.25 / 6.08 |

### Scope {#acceptance}

Synthetic trajectories validate prefix structure, boundaries and arguments, not identical model next actions or rewards. Real sessions, newer CLIs, long prefixes, token costs and remote-connection recovery are unmeasured.

