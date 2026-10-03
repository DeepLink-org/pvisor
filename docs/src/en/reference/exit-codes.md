# Exit codes and errors

Scripts should preserve the exit code, then read the Run Bundle. The code tells you whether the command succeeded; the recorded failure type helps distinguish workload errors, setup failures, and policy rejection.

A workload can return `1`, `2`, or `125` itself, so a number alone is insufficient for deciding whether to retry, switch executors, or grade an agent.

## Current exit behavior

| Situation | CLI behavior |
| --- | --- |
| Workload exits normally | `run` returns its exit code; success is usually 0 |
| Run cancelled | 130 |
| Execution fails without a workload exit code | 1 |
| Internal host sandbox setup fails | Launcher uses 125; outer errors may return 1, so inspect diagnostics |
| Argument parsing error | clap returns 2 |
| Successful `status`/`apply`/`drop`/`kill` | 0 |
| Runtime error in those commands | `anyhow` returns 1 with stderr diagnostics |
| Command under `inspect` | Returns the inspected command's exit code |
| Companion command | Dispatch preserves its code; also inspect replay result phase/quality |

Apply conflicts, missing Jobs, and UnsupportedPolicy do not currently have distinct numeric codes. Workloads may themselves return 1, 2, 125, or 130; numbers alone cannot distinguish agent failure from pVisor refusal.

## Automation

Save stderr and the return code, then look for the Bundle at an explicit stage path. `run.state`, `run.exit_code`, and `run.failure` describe execution. Admission/preparation can fail before a complete Bundle exists; classify this as startup/infrastructure failure rather than successful no-op.

Do not apply automatically just because the agent returned 0. Apply has independent conflicts/recovery and its result needs checking. Timeouts, cancellations, and nonzero exits can leave reviewable changes. See [CI](../guides/ci.md).

## A shell pattern that retains failure evidence {#shell}

```bash
set +e
pvisor run --safe --overlaynet-deny-all --stdio capture \
  --stage ../stage-exit-001 -- /bin/sh -c 'printf "candidate\n" > result.txt; exit 7'
run_code=$?
pvisor status --review --json ../stage-exit-001 > ../stage-exit-001.review.json
review_code=$?
printf 'run=%s review=%s\n' "$run_code" "$review_code"
```

The command returns `7` and may still leave a reviewable `result.txt`. Retain execution and review-reading statuses separately; after collecting artifacts, CI should end the step with the original `run_code`. Apply changes after review, or drop them if discarded.
