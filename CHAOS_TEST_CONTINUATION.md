# Bounded chaos testing continuation

## Campaign

- Requested duration: eight hours of iterative scenarios, investigation and repair.
- Started: 2026-11-07 14:32:01 UTC.
- Deadline: 2026-11-07 22:32:01 UTC.
- Initial revision: `c0cb08d0`.
- Branch: `test/bounded-chaos-20261107-continuation`.
- Previous campaign: `CHAOS_TEST_LOG.md`; its PR #111 has been merged.
- Publication: a separate PR, updated after each verified repair; no main merge.
- Execution: subagents execute scenarios serially; the parent reviews and
  integrates findings and publishes verified fixes.

This log records actual execution, not a claim that eight hours have elapsed.
Interruptions, missing capabilities and failed checks must be recorded explicitly.

## Safety limits

- One active build/test workload, at most two Cargo jobs and one test thread.
- Use `nice -n 10` and a 900-second command timeout with a 30-second kill grace.
- Check memory, load and free disk before and after workloads. Stop launching
  workloads below 6 GiB available memory or 20 GiB free disk, or above load1 8.
- Keep generated data and fault injection in worktree-local temporary fixtures.
- No host network changes, disk filling, unrelated process termination,
  production credentials, paid services, or global cache cleanup.
- Use targeted `mise run test -- PACKAGE` validation. Never alter real semspec
  approval ledgers/snapshots, run approval commands, or weaken claims.
- Do not start new workloads after the deadline; report actual elapsed time
  separately from active test time.

Initial host observation: 16 CPUs, 24,318 MiB available RAM, no used swap,
347 GiB free disk, load1 0.65. `/dev/kvm` and `/dev/fuse` exist; end-to-end
capabilities remain to be verified.

## Rounds

### 033 — Replay stdin and subprocess supervision

Assigned to an isolated worker subagent. Investigate cancellation, early stdin
closure, child exit and bounded process cleanup, taking account of the native
fixture changes merged since the previous campaign. Results pending.
