---
status: todo
search:
  exclude: true
---

# Exit codes and errors

!!! warning "Planned"
    The complete reference is pending. `pvisor run` preserves command exit codes; `--strict` admission rejects missing enforcement evidence with `UnsupportedPolicy`.

## Question

How do workload exit codes differ from pVisor errors?

## Requirements

- List command exit behavior.
- Cover unsupported policy, sandbox setup, apply conflict, missing Job.
- Explain CI classification of agent failure versus admission refusal.

## Acceptance criteria

- Map behavior to implementation and tests.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [CI](../guides/ci.md)

## Current exit behavior

| Situation | CLI behavior |
| --- | --- |
| Workload exits normally | `run` returns its exit code; success is usually 0 |
| Run cancelled | 130 |
| Execution fails without a workload exit code | 1 |
| Internal host sandbox setup fails | Launcher uses 125; outer errors may return 1, so inspect diagnostics |
| Argument parsing error | clap returns 2 |
| Successful status/apply/drop/kill | 0 |
| Runtime error in those commands | anyhow returns 1 with stderr diagnostics |
| Command under inspect | Returns inspected command's exit code |
| Companion command | Dispatch preserves its code; also inspect replay result phase/quality |

Apply conflicts, missing Jobs, and UnsupportedPolicy do not currently have distinct numeric codes. Workloads may themselves return 1, 2, 125, or 130; numbers alone cannot distinguish agent failure from pVisor refusal.

## Automation

Save stderr and the return code, then look for the Bundle at an explicit stage path. `run.state`, `run.exit_code`, and `run.failure` describe execution. Admission/preparation can fail before a complete Bundle exists; classify this as startup/infrastructure failure rather than successful no-op.

Do not apply automatically just because the agent returned 0. Apply has independent conflicts/recovery and its result needs checking. Timeouts, cancellations, and nonzero exits can leave reviewable changes. See [CI](../guides/ci.md).
