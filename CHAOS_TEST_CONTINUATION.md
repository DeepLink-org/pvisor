# Bounded chaos testing continuation

## Campaign

- Requested duration: eight hours of iterative scenarios, investigation and repair.
- Started: 2026-11-07 14:32:01 UTC.
- Deadline: 2026-11-07 22:32:01 UTC.
- Initial revision: `c0cb08d0`.
- Branch: `test/bounded-chaos-20261107-continuation`.
- Previous campaign: `CHAOS_TEST_LOG.md`; its PR #111 has been merged.
- Publication: a separate PR, updated after each verified repair; no main merge.
- Draft PR: https://github.com/DeepLink-org/pvisor/pull/118.
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
fixture changes merged since the previous campaign.

Confirmed defect: cancellation retained the pending stdin pipe during graceful
termination. A cooperative SIGTERM handler draining stdin to EOF could not
finish, was killed with SIGKILL, and lost its final output. Pending stdin now
closes before cancellation signaling, including wall/quota, idle and step paths.
README documents that remaining input is not delivered.

Worker evidence on Linux, 2026-11-07 UTC:

- 14:34:38–14:35:02: regression before repair failed as expected with SIGKILL;
  0 passed, 1 failed, 149 skipped.
- 14:35:28–14:35:45: process suite after repair passed; 16 passed, 134 skipped.
- 14:35:56: targeted rustfmt check passed; `git diff --check` also passed.
- Both test runs used `mise run test -- pvisor-replay -- --offline -E FILTER`,
  respectively filtering the new cancellation regression and `process::tests::`.
- Commands used `nice -n 10 timeout -k 30s 900s`, two build jobs, one test thread,
  offline Cargo and an isolated `target-chaos-033` inside the worker worktree.
- Across pre/post checks: minimum MemAvailable 24,445,900 KiB, maximum load1
  1.21, minimum free disk 371,443,019,776 bytes. No admission limit was crossed.
- Fixtures removed and no matching child processes found at completion.

The new regression directly covers wall and idle cancellation, not quota or
SIGINT step cancellation. Worker did not run the full package, Python, live
agents/models or other operating systems. Initial mise discovery encountered an
untrusted-config prompt; command-local trust resolved it without installing tools.

Parent inspected the merged diff and cancellation paths. An independent reviewer
subagent confirmed the production change on inspection and ran:

- 14:37:35–14:38:13 UTC: `mise run test -- pvisor-replay -- --offline`,
  150 passed, 0 skipped, three binaries.
- 14:38:26–14:38:31 UTC: the new cancellation regression with
  `--stress-count 10`, all ten iterations passed.
- Resource checks remained safe: minimum MemAvailable 24,081,884 KiB,
  maximum load1 1.51, minimum free disk 346 GiB.
- No fixture directories or matching test processes remained.

Review identified an unproven startup-timing assumption: cancellation at 250 ms
can precede the child installing its signal handler on a slow host. This did not
fail locally, but must be addressed rather than relying on the successful
repeats. The original worker is refining the fixture and considering explicit
quota/SIGINT coverage before the repair is published. Other-platform execution
and induced slow startup remain untested.

Follow-up, 14:40–14:41 UTC:

- Extended the fixture to wall, idle, quota and SIGINT step cancellation.
- Wall/idle now tolerate three-second native startup; this reduces but does
  not eliminate startup starvation risk. Quota/step events follow handler setup.
  No production readiness hook or timeout semantic change was introduced.
- The fixture alarm is 40 seconds, beyond its wall bound plus step grace.
- Revised baseline reproduced SIGKILL with all three production closes
  temporarily removed: 0 passed, 1 failed, 149 skipped.
- Restoring the repair passed 16 process tests, 134 skipped, and rustfmt.
- Minimum MemAvailable 23,871,244 KiB; maximum load1 0.76; minimum free disk
  371,151,233,024 bytes. No leftover fixture processes/directories found.
- A follow-up merge conflicted at the regression, duplicating old/new fixture
  text. Parent resolved it by retaining only the new four-trigger fixture;
  production diff remains exactly the three stdin closes.
- A fresh worker is validating the integrated full package before publication.

Integrated-tree validation by the round 034 worker:

- 14:43:31–14:44:14 UTC: `nice -n 10 timeout -k 30s 900s mise run test -- pvisor-replay -- --offline`
  passed all 150 tests, zero skipped (20.289 seconds in nextest).
- 14:44:22–14:44:24 UTC: `nice -n 10 timeout -k 30s 900s cargo fmt --all -- --check`
  passed without edits. This formatting check is not a workspace test run.
- Pre-test MemAvailable 24,730,554,368 bytes, load1 0.45, disk free
  371,155,406,848 bytes; post-format MemAvailable 24,686,600,192 bytes,
  load1 1.54, disk free 371,118,174,208 bytes. All safety gates passed.
- This validates the final resolved fixture, unlike the earlier review run.
  Linux-only evidence; macOS and induced startup starvation remain untested.

### 034 — Exited leader with inherited stdin

Assigned after round 033 workloads completed. After integrated-tree validation,
the worker will test early leader exit with descendants retaining pipe ends,
incomplete stdin delivery, bounded failure and process-group cleanup.

Result: no new production defect. The existing early-exit rule rejects incomplete
stdin delivery before waiting for EPIPE and kills the remaining process group.
Added a Linux-only native-fork fixture for descendants holding stdin alone or
stdin plus output pipes. Readiness uses a pipe handshake; a ten-second alarm
bounds the descendant. Subreaper adoption and waitpid verify SIGKILL and reaping.

- 14:48:29–14:48:33 UTC: targeted regression passed both variants, 1 passed,
  150 filtered (24 ms test time).
- 14:49:14–14:49:38 UTC: `mise run test -- pvisor-replay -- --offline`,
  151 passed, none skipped (20.318 s).
- Initial formatting check found two line-wrap differences, corrected narrowly;
  final `cargo fmt --all -- --check` and `git diff --check` passed.
- Lowest reported MemAvailable 24,509,943,808 bytes; highest load1 1.73;
  lowest disk free 370,907,598,848 bytes. No matching processes remained.
- No macOS, live agents/models, doctests or separate Python-suite testing.

Parent review requested removal of an `.err().expect()` Clippy diagnostic and
stronger fixture cleanup ownership so a stale PID file cannot cause a reused
unrelated PID to be signaled after reaping. Worker is addressing these before
publication, with targeted regression and warnings-denied Clippy.

Review follow-up completed:

- Cleanup now checks `waitpid(..., WNOHANG)` child ownership before signaling;
  a reaped child or stale PID does not receive a signal.
- Replaced `.err().expect()` with `.map(|_| ()).expect_err()` without adding
  a production Debug implementation solely for tests.
- 14:51:29–14:51:45 UTC: `cargo clippy -p pvisor-replay --all-targets --offline -- -D warnings`
  passed with the same nice/timeout/resource limits.
- 14:54:29–14:54:33 UTC: targeted mise regression passed, both variants,
  1 passed (23 ms); final formatting and diff checks passed.
- Final available memory 24,749,355,008 bytes, load1 0.69, disk free
  370,757,128,192 bytes; no matching fixture processes remained.
- Parent merged and reviewed the follow-up. Full 151-test run predates these
  test-only cleanup/lint corrections; targeted regression and Clippy follow them.

At 14:51 UTC, published round 033 revision `1694909b` had two successful checks,
25 pending and none failing. This is not final CI success.

### 035 — OverlayFS writable-handle namespace churn

Assigned to a fresh isolated worker after round 034 finished its workloads.
Explore staged rename/unlink/replacement with open writable handles and host
source preservation, avoiding scenarios already covered in rounds 007 and 011.

Result: no production defect reproduced. A re-executed client, not the FUSE
server, receives a 16-byte RLIMIT_FSIZE. Two detached writable inodes reject
positional write/truncate with EFBIG. Seven native-versus-FUSE checkpoints
compare namespace, contents, EOF and inode equivalence across failed replacement,
ancestor rename, unlink, parent recreation, fsync, checked close/drop and
recovery via a surviving duplicate descriptor. Host lower state stays unchanged.
Actual mountinfo presence and removal establish a real mount and explicit unmount.

Worker commands, 2026-11-07 UTC, with standard resource/nice/timeout limits:

- 14:57:44–14:57:57:
  `mise run test -- pvisor-overlayfs -- --test namespace_sequence_mount --run-ignored ignored-only --no-capture -E 'test(detached_size_fault_close_recovery_matches_native_trace_035)'`,
  1 passed, 2 filtered/skipped.
- 14:58:07–14:58:14: `mise run test -- pvisor-overlayfs`,
  73 passed, 12 skipped. Other ignored FUSE tests were not run.
- 14:58:14–14:58:20:
  `mise exec -- cargo clippy --offline --locked -p pvisor-overlayfs --test namespace_sequence_mount -- -D warnings`,
  passed.
- Targeted rustfmt and diff checks passed.
- Minimum available memory 23,955,568 KiB; maximum load1 1.96;
  minimum disk free 362,014,464 KiB. No fixtures/mounts remain.

Parent reviewed and integrated the regression and README. These faults are
client-kernel size-limit rejection, not backing-device errors; successful
fsync/close after rejection does not prove delayed close-error handling.
macOS, VM, concurrent execution, Python and workspace suites were not run.

### 036 — OverlayFS backing I/O recovery

Assigned after round 035 workloads finished. Explore a safe bounded backing-I/O
failure and resource/error recovery, without public test-only APIs or host-wide
fault injection. Distinguish actual server faults from client-side rejection.

Result: no defect found. Used an existing private per-filesystem copy-loop seam
on real Linux FUSE. After a successful staged write, rename/unlink and retained
reader/writer/dup ownership, injected ENOSPC before copying or after three actual
backing bytes. Zero progress returns ENOSPC; partial progress returns three and
advances both offsets consistently. Readers observe the changed prefix. Sync,
checked close and recovery via dup succeed, preserving replacement and lower.
Reopening each stage through public API without injection allows full copy/sync.

This is a synthetic server-loop failure, not physical disk exhaustion, a backing
syscall returning ENOSPC, delayed writeback, or failing fsync/close coverage.

Final worker checks, UTC 2026-11-07, standard nice/timeout/resource limits:

- 15:02:15–15:02:23: `mise run test -- pvisor-overlayfs`,
  73 passed, 13 skipped.
- 15:02:37–15:02:38:
  `mise run test -- pvisor-overlayfs -- --lib --run-ignored ignored-only --no-capture -E 'test(backing_copy_fault_detached_close_recovery_036)'`,
  1 passed, two fault cases.
- 15:02:38–15:02:39: explicitly reran mounted 007/011 traces, both passed
  (24/21 operations).
- `mise run lint-rust -- -p pvisor-overlayfs`, package fmt and diff checks passed.
- Initial test-only owner-type compile error and two formatting differences
  were corrected before these final checks.
- Minimum MemAvailable 24,357,773,312 bytes; maximum load1 1.34033203125;
  minimum disk free 370,215,366,656 bytes. Four test mounts detached, no
  fixture mountinfo entries or fixture-backed descriptors remained.
- No macOS, physical-device failures, delayed writeback or failing sync/close
  tests. Round 035 preserved but not explicitly rerun.

Parent reviewed the private regression and README and merged them cleanly.
At 15:04 UTC CI for published round 035 had no failed checks and multiple
in-progress checks; no final CI success is claimed.

### 037 — Journal persistence failure recovery

Assigned after round 036 workloads ended. Investigate safe deterministic
journal/preimage persistence failure, ownership/error propagation and recovery,
without duplicating round 006 torn-write coverage or injecting host-wide faults.
