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
- After round 15 the testing worker conservatively stopped new launches when
  swap became full, despite the original RAM/disk/load gates passing. Its final
  cleanup audit was at 16:01:17 UTC. The interval until the parent's 16:41:28 UTC
  resource observation is unobserved safety-pause time, not active testing.
- At 16:41:28–31 UTC, memory PSI some/full averages were zero. Three one-second
  vmstat interval samples had zero swap-out, CPU 94–99% idle, and swap-in of
  1468, 4 and 16 KiB/s. Available RAM was approximately 18.29 GiB and free swap
  approximately 184 MiB. These samples show no current sustained memory stall;
  they do not establish the cause of earlier swap use or guarantee future safety.
- The worker was authorized to resume only warm-cache, small, serial functional
  workloads at one Cargo build job and one test thread. New admission/stop
  thresholds are at least 10 GiB available RAM, 64 MiB free swap and 20 GiB free
  disk; load1 at most 8; memory PSI full avg10 at most 0.5%; and no sustained
  swap-out above 16 MiB/s over ten seconds. Long commands sample every five
  seconds. No new cold builds or broad package expansion; no host swap changes.
  A failed safety condition stops owned work and requires a new assessment.
  The original 22:14:45.971695 UTC deadline remains unchanged.
- At 16:54:48.192917 UTC, the resumed worker crossed three tightened stop
  thresholds: free swap 10,280,960 bytes, memory full PSI avg10 0.62%, and
  swap-out 20,911,470 bytes/s over at least ten seconds. The supervisor stopped
  and reaped its owned workload. The interrupted namespace run is incomplete,
  not a pass or a product assertion failure, and receives no coverage count.
- At 16:56:35.863 UTC, final owned-process, temporary-fixture and mount audits
  were empty. Only the proven-owned interrupted fixture was removed after
  ownership/type/device/no-symlink/no-mount checks. Evidence was retained.
- No automatic restart is pending. The user was asked to confirm after freeing
  resources before another assessment. The original deadline was not reached
  or extended, and this campaign is paused, not an eight-hour completion.
