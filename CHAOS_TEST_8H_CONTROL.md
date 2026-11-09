# Eight-hour chaos campaign control record

## Requested window

This is a new eight-hour window requested by the user. Earlier rounds do not
count toward it.

- Start: 2026-11-09T14:14:45.971695Z.
- Deadline: 2026-11-09T22:14:45.971695Z.
- Start Linux boot ID: `504c40ca-6c67-41ab-8079-892a76aad3df`.
- Start `CLOCK_BOOTTIME`: 175338.010742897 seconds.
- Deadline `CLOCK_BOOTTIME`: 204138.010742897 seconds.
- Start monotonic clock: 175338.010741735 seconds.

The deadline is a stopping bound, not evidence of eight hours of execution.
Record session interruptions, clock discontinuities and unobserved gaps
separately. Do not silently extend the window or count gaps as active tests.

## Execution and publication

The testing subagent owns one serial build/test workload and writes its
per-round evidence to `CHAOS_TEST_8H_20261109.md`. The parent coordinates review
and publication on branch `chaos/20261109-eight-hour`. Each confirmed defect
requires a reproducer, a focused repair and regression validation before its
PR update. No merge into `main` is authorized.

Earlier `CHAOS_TEST_NEW*` reports, the half-close evidence patch, and the
inherited journal contract-test addition are prior work, not new-window
results. The Gateway half-close policy question remains unresolved; no global
transport-policy change is part of this campaign. Plugin changes are excluded.

## Resource and containment limits

- One active build/test workload, two Cargo build jobs, one nextest test thread.
- Workloads run at niceness 10 with a per-command watchdog of at most 600 seconds.
- Admission requires at least 6 GiB available RAM, 20 GiB free worktree disk,
  and load1 at most `min(8, logical CPU count)`.
- Monitor resources during long commands. Stop the owned workload if safety
  limits deteriorate; report failed admission rather than launch anyway.
- Use small owned disposable fixtures and bounded logs. Reap owned subprocesses
  and audit fixture mounts and temporary files after each round.
- No host OOM, full-disk injection, unrelated process termination, privileged
  configuration changes, real-user stages, or external service mutations.
- Use the repository's `mise run test -- <package>` runner. Record selection
  skips and coverage gaps; do not weaken assertions or semantic approvals.

Initial observation: 16 logical CPUs, load1 2.36084, available RAM
15,004,868,608 bytes, free disk 269,485,883,392 bytes. Swap was already heavily
occupied (8,589,930,496 total bytes, 1,825,083,392 free bytes).

## Completion criteria

The final report must distinguish actual supervised workload time, scenario
investigation time, and any unknown gaps. It must list executed scenarios,
failures, fixes, exact validation commands, default or selected skips, safety
measurements and remaining cleanup or platform limitations. A requested
eight-hour window alone must never be reported as a completed eight-hour test.

## Coordinator checkpoints

- By 14:31 UTC, two validated repairs were published to PR #129: replay receipt
  admission (`340b4d85`) and scoped network approval narrowing (`90d4921f`).
  Each received independent read-only review with no blocking findings.
- The first testing worker then ended with a provider content-safety error.
  This was an execution interruption, not a product test result. Its last
  captured report and fixes were preserved.
- At 14:31:28.296788 UTC, the boot ID still matched and `CLOCK_BOOTTIME` was
  176340.335829089. A command-line process audit found only the audit command's
  own matches, no matching supervisor or test survivors. This is a limited
  process audit, not proof of absence in every namespace.
- A successor worker was assigned independent, non-destructive functional
  recovery scenarios. The provider-blocked scenario was not retried. The
  original deadline remains unchanged, and any unobserved handoff interval
  must not count as active testing.
- Handoff resource sample: available RAM 11,071,672,320 bytes, free disk
  268,266,602,496 bytes, load1 2.65088, swap free 864,407,552 bytes.
