# Exit codes and errors

Scripts should preserve the exit code, then read the Run Bundle. The code tells you whether the command succeeded; the recorded failure type helps distinguish workload errors, setup failures, and policy rejection.

A workload can return `1`, `2`, or `125` itself, so a number alone is insufficient for deciding whether to retry, switch executors, or grade an agent.

## Current exit behavior

| Situation | CLI behavior |
| --- | --- |
| Workload exits normally | `run` returns its exit code; success is usually 0 |
| Wall-time timeout | 1; `run.failure.kind = "deadline_exceeded"`, no workload exit code |
| Run cancelled | 130 |
| Execution fails without a workload exit code | 1 |
| Internal host sandbox setup fails | Launcher uses 125; outer errors may return 1, so inspect diagnostics |
| Argument parsing error | clap returns 2 |
| Successful `status`/`apply`/`drop`/`kill` | 0 |
| Runtime error in those commands | `anyhow` returns 1 with stderr diagnostics |
| Command under `inspect` | Returns the inspected command's exit code |
| Companion command | Dispatch preserves its code; also inspect replay result phase/quality |

Apply conflicts, missing Jobs, and UnsupportedPolicy do not currently have distinct numeric codes. Workloads may themselves return 1, 2, 125, or 130; numbers alone cannot distinguish agent failure from pVisor refusal.

## Daemon process and API errors {#daemon}

The standalone `pvisor-daemon` process returns 0 for successful `protocol` or normal server shutdown, 2 for clap argument errors, and 1 for startup/runtime errors reported on stderr. These are service process statuses, not sandbox workload exit codes. Companion dispatch preserves the companion's code when installed.

Lifecycle clients must instead inspect HTTP status, `{code, message}` and `X-Request-ID`. Create returns 202 JSON; pause/resume return 202 with empty bodies; confirmed deletion returns 204. None establishes workload command success. Command results come from the prepared image's real execd data plane, not the native Job Run Bundle or retired Cluster aggregate result. Unsupported creation options are rejected rather than silently applied. See [daemon operations](../guides/daemon/operations.md).

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

## Classify the recorded failure {#failures}

`run.failure` contains `kind`, `message`, and `retryable`. Treat `retryable` as the executor's hint, then apply your task's retry policy. A timeout is a failed execution with no normal process exit code; it is different from a command that itself returns `1`.

| Failure kind | Meaning / action |
| --- | --- |
| `invalid_spec` | Invalid resolved request; correct configuration before retrying |
| `unsupported` | Executor cannot satisfy a requested capability; change the request or executor |
| `spawn` | Workload or VM launch failed; inspect paths, executables and device diagnostics |
| `process_exit` | Workload returned a nonzero status; preserve its `exit_code` |
| `workload` | Executor reported task-level failure beyond a simple process status |
| `deadline_exceeded` | Task exceeded its wall-clock deadline; inspect progress and retained files |
| `infrastructure` | Execution machinery failed, such as sandbox setup or I/O; inspect diagnostics |

Parse errors and pre-admission failures can occur before `run.failure` or a Bundle exists. Missing Job selection, apply conflicts, and management-command errors are reported on stderr with exit 1, rather than being fabricated as workload failures. `UnsupportedPolicy` diagnostics describe a rejected capability request; `CAPABILITY_UNSUPPORTED` is used for ordinary execution-checkpoint/suspend requests. Do not grade these as failed model answers.

The downloadable [nonzero-exit sample](../../assets/examples/json/failed-run.json) records `state = "failed"`, `exit_code = 7`, and `failure.kind = "process_exit"`. The [timeout sample](../../assets/examples/json/timeout-run.json) records `state = "failed"`, omits `exit_code`, and uses `failure.kind = "deadline_exceeded"`; its CLI returned 1. Both retain staged candidate files. [Collection provenance](../../assets/examples/json/provenance.json) includes the commands and observed return codes.
