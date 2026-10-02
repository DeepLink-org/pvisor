---
status: todo
search:
  exclude: true
---

# Exit codes and errors

!!! warning "Planned"
    The complete reference is pending. `pvisor run` preserves command exit codes; `--strict` admission rejects missing enforcement evidence with `UnsupportedPolicy`.

## Question

Which exit code does each `pvisor` subcommand return, and when? How do pVisor's own errors differ from the exit code of the command it runs?

## Requirements

- List each subcommand's exit codes and meanings.
- List the main error types (unsupported policy, sandbox setup failure, apply conflict, missing Job) with their exit codes and messages.
- Explain how CI distinguishes "agent failure" from "pVisor refused to run".

## Acceptance criteria

- The exit-code table maps one-to-one to error types in the code and has test coverage.

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
| Successful `status`/`apply`/`drop`/`kill` | 0 |
| Runtime error in those commands | `anyhow` returns 1 with stderr diagnostics |
| Command under `inspect` | Returns the inspected command's exit code |
| Companion command | Dispatch preserves its code; also inspect replay result phase/quality |

Apply conflicts, missing Jobs, and UnsupportedPolicy do not currently have distinct numeric codes. Workloads may themselves return 1, 2, 125, or 130; numbers alone cannot distinguish agent failure from pVisor refusal.

## Automation

Save stderr and the return code, then look for the Bundle at an explicit stage path. `run.state`, `run.exit_code`, and `run.failure` describe execution. Admission/preparation can fail before a complete Bundle exists; classify this as startup/infrastructure failure rather than successful no-op.

Do not apply automatically just because the agent returned 0. Apply has independent conflicts/recovery and its result needs checking. Timeouts, cancellations, and nonzero exits can leave reviewable changes. See [CI](../guides/ci.md).
