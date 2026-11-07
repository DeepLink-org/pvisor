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

Result: no product defect reproduced. After one durable append, the private test
substitutes a read-only descriptor while retaining the original locked writer.
The next emit fails with a downcastable AppendError::Unknown, leaves committed
bytes unchanged and emits no notification. Poison applies to all clones, even
exact committed retries, and survives restoring the writable descriptor. The
writer lock lasts until the final owner drops. Two reopens preserve the first
receipt and support quiet idempotent retries and contiguous offsets.

This exercises a real descriptor write error, not ENOSPC, fsync failure, actual
staged mutation/preimage durability, process crash or power loss.

Worker checks, UTC 2026-11-07, standard workload limits:

- 15:07:37: `mise run test -- pvisor-journal`, 30 passed, zero skipped.
- 15:07:47:
  `mise exec -- cargo clippy --locked -p pvisor-journal --all-targets -- -D warnings`,
  passed.
- 15:07:51: `mise exec -- cargo fmt -p pvisor-journal -- --check`, passed.
- 15:07:52:
  `RUSTDOCFLAGS='-D warnings' mise exec -- cargo doc --locked -p pvisor-journal --no-deps`,
  passed. API boundary guards and diff checks passed unchanged.
- Minimum MemAvailable 24,522,612,736 bytes; maximum load1 1.8755;
  minimum free disk 370,143,096,832 bytes. No mounts/background processes;
  fixtures cleaned up. No other platforms, affected consumer compilation,
  Python or doctests were run.

Parent reviewed and merged the regression and README cleanly.

### 038 — Staged preimage publication recovery

Assigned after round 037 workloads ended. Investigate an owned destination
obstruction around preimage/metadata publication, preserving candidate ownership,
accepted upper bytes and host source state across failure and retry/reopen.

Result: no product defect reproduced. The private scheduling seam inserts a small
owned nonempty directory at the legacy preimage final name after missing-entry
check, after another staged file has succeeded. Both nondurable no-replace rename
and durable hardlink publication reject the invalid winner with EINVAL.
Repeated public copy-up attempts preserve accepted candidate bytes/inode, host
sources and obstruction sentinel, without publishing a new upper or leaking
pending journal temporaries. Removing the obstruction allows retry, sync and
owner reopen with original baselines.

Worker checks, UTC 2026-11-07, standard workload limits:

- 15:10:53–15:11:03: targeted mise overlay-core library regression passed,
  1 passed, 175 filtered; two publication variants.
- 15:11:54–15:12:00: `mise run test -- pvisor-overlay-core`,
  238 passed, 5 skipped.
- 15:12:09–15:12:14: warnings-denied all-targets package Clippy passed.
- Two new formatting differences from initial check corrected; final package
  formatting and diff checks passed.
- MemAvailable 24,030,216–24,267,324 KiB; load1 1.07–1.31;
  free disk 369,826,463,744–369,928,208,384 bytes. No mounts/children;
  worktree-local fixtures cleaned up.
- No CLI/FUSE/VM, process crash/power loss, compact log, device/fsync failure,
  macOS, Python, workspace-wide or doctest execution.

Parent inspected the publication/copy-up/reopen assertions and integrated the
private regression and README.

### 039 — Managed stage seal publication failure

Assigned after round 038 workloads ended. Explore managed sealed-v1 publication
obstruction, completion refusal, retained candidates and admission/retry behavior,
avoiding prior round 029 completion-publication coverage.

Result: no confirmed defect. An external contract test covers checkpoint and
strict policies, using a nonempty owned `sealed-v1` directory. Repeated atomic
marker replacement failures after journal drain preserve bytes, inode/UID/GID,
directory identities, policy and observations, without temporary leaks.
Invalid seal blocks writable reopen/reuse. Removing obstruction alone does not
complete the stage; exclusive unsealed reopen can mutate, completion retry
succeeds, sealed reopen rejects mutation with EROFS, and explicit reuse clears seal.

Worker checks within 15:13:27–15:17:07 UTC, standard resource limits:

- `mise run test -- pvisor-overlay-core -- --test seal_publication`,
  1 passed, zero skipped.
- Selected stage library tests plus seal-publication binary, 13 passed,
  164 skipped. Existing apply-executing cases excluded to avoid global apply-lock
  effects; this was not a full package run.
- `mise run lint-rust -- -p pvisor-overlay-core`, package fmt and diff passed.
- An initial argument-separator mistake failed before compilation, then was
  corrected. New-file format differences fixed before final passing checks.
- Minimum MemAvailable 23,748,448 KiB; maximum load1 1.68;
  minimum free disk 360,972,212 KiB. No mounts/children; fixtures cleaned.
- No device/power-loss/runtime-receipt/FUSE/VM/macOS/workspace validation.

Parent reviewed the public-contract assertions and integrated the new test/README.
At 15:17 UTC published round 038 CI had no failures, with several checks still
in progress; final CI success remains unconfirmed.

### 040 — OverlayNet malformed-fragment isolation

Assigned after round 039 workloads ended. Explore a new bounded packet/state
boundary and healthy-flow recovery after malformed input, avoiding round 008/015
coverage. No host networking changes or live external traffic.

Confirmed defect: SYN listener admission checked raw flags/checksums but not
semantic TCP validity. Malformed options could allocate the only flow slot even
though smoltcp rejected the packet, making a healthy SYN to a different destination
receive RST. Production change now reuses smoltcp IPv4/TCP semantic parsers before
allocation and retains explicit fragment rejection.

Public framed-link regression uses 15 fixed hostile frames (seven malformed TCP
options, invalid IP version, three fragment cases, four truncations), one flow
slot, UDP DNS barriers and a final unrelated SYN requiring SYN-ACK. It sends
33 input frames and receives 18 replies, without external traffic.

Worker checks, UTC 2026-11-07, standard resource limits:

- 15:23:56–15:24:17: initial probe failed on unexpected flow-denial counters.
- 15:25:10–15:25:11: strengthened distinct-destination reproduction failed
  because the healthy SYN received RST.
- 15:25:38–15:25:40: related VM/library plus vm_dns tests passed after fix,
  15 passed, 76 skipped.
- All-targets package warnings-denied Clippy and formatting/diff checks passed.
- Minimum MemAvailable 24,426,827,776 bytes; maximum load1 1.719727;
  minimum disk free 369,281,327,104 bytes.
- Worker did not run full package/peer/workspace/Python suites, consumer builds,
  real VM boot, other platforms, sustained fuzzing or full TCP application flow.

Parent integrated the fix/test/README. Independent reviewer is checking actual
stack admission semantics, VM/peer callers, test synchronization, full OverlayNet
tests and targeted affected-consumer compilation before repair publication.

Independent review:

- Full `mise run test -- pvisor-overlaynet`: 115 passed, zero skipped across
  four binaries; targeted owning-consumer `cargo check --locked -p pvisor --lib`
  passed. No real VM boot or optional-feature consumer validation.
- Resource checks: minimum MemAvailable 24,680,710,144 bytes, maximum load1
  2.25, minimum disk free 368,803,643,392 bytes.
- Confirmed compatibility of checksums, version/fragment rejection, flag parsing,
  peer routing and deterministic UDP barriers on source inspection.
- Found two additional source-traced admission gaps, not yet reproduced:
  parsed zero-MSS SYN rejected by the actual listener and Ethernet wrong-unicast
  destination rejected by the interface after allocation. Both can leave LocalDns
  in Listen outside current handshake reaping.
- Original worker is reproducing/addressing those before publication, preserving
  the stack's broadcast/multicast acceptance rather than imposing router-only MAC.
  No listener-reaping change is assumed necessary without investigation.

Follow-up blocked: the model service rejected the network-validation assignment
with a cybersecurity-risk error before execution; no new source changes resulted.
No rephrased retry or safeguard bypass was attempted. The two source-traced gaps
remain unresolved and unverified by reproducer tests. Publish only the separately
reproduced malformed-option repair, which passed independent full tests/consumer
compilation, without claiming complete stack-admission parity.

### 041 — Compact journal writer recovery

Assigned after the blocked round 040 follow-up. Return to non-network storage
reliability: compact-log write failure, poison/ownership propagation and recovery
after accepted staged data, avoiding the preceding journal/preimage/seal cases.

Result: no product defect reproduced. A distinct private sync-error seam uses an
O_PATH descriptor on the owned compact log to produce EBADF after Core accepted
staged bytes and a later complete observation awaits sync. Original writer remains
writable and locked. Poison blocks repeated sync, promotion, observation and
consumption; frames, baselines and candidate bytes/inode remain unchanged.
Drop/reopen drains complete frames; Core retry publishes the missing candidate,
and another reopen preserves both candidates and original baselines.

Worker checks, UTC 2026-11-07, standard resource limits:

- 15:34:00–15:34:10: targeted mise overlay-core library regression,
  1 passed, 176 skipped.
- 15:34:37–15:34:41: selected compact-log/core/publication tests,
  16 passed, 161 skipped.
- All-targets package Clippy warnings denied, package fmt and diff passed.
- Minimum MemAvailable 24,078,484 KiB; maximum load1 1.47;
  minimum free disk 368,315,019,264 bytes. Fixtures cleaned.
- No device/hardware fault, power loss, partial writes, fault while Core remains
  live, full package, Python, doctests, macOS, FUSE or VM validation.

Parent inspected the private fault/poison/reopen assertions and integrated test
and README. At 15:35 UTC published round 040 CI had no failures, but many checks
remained in progress.

### 042 — Runtime receipt recovery admission

Assigned after round 041 workloads ended. Investigate receipt-publication failure
after successful storage sealing and subsequent restart/reuse admission, avoiding
duplicate round 029 runtime and round 039 storage-only publication checks.

Result: no confirmed defect. The checkpoint-policy fixture seals storage and
publishes Bundle, then an owned nonempty completion.json directory makes receipt
preflight fail. This is a destination-type preflight error, not rename/fsync.
After removing only obstruction, two reopen cycles retain read-only storage,
Unknown runtime status, blocked apply/drop/checkpoint/new-Run admission, candidate
bytes/identity, host contents, journal/seal and Bundle. No receipt is synthesized.
Explicit completion-owner retry commits the original result and confirms stopped.

Worker checks, UTC 2026-11-07, standard workload limits:

- 15:39:10–15:40:16: exact new lifecycle test passed, 1 passed, 765 skipped.
- 15:40:40–15:41:01: five focused completion lifecycle tests passed,
  5 passed, 761 skipped. Five distinct tests, six executions total.
- 15:41:05–15:41:35: package library/tests Clippy warnings denied passed.
- Package formatting/diff checks passed.
- Minimum MemAvailable 24,476,504,064 bytes; maximum load1 2.333;
  minimum free disk 367,773,474,816 bytes. Fixtures removed, no mounts or
  surviving workload children.
- Reopen fixture only, not process crash/restart or managed execution. No CLI,
  FUSE/VM, strict policy, receipt rename/fsync, power loss, macOS, full package,
  Python or workspace validation.

Parent inspected the receipt/admission/preservation assertions and integrated
the private lifecycle regression and README.

### 043 — Runtime completion handoff recovery

Assigned after round 042 workloads ended. Inspect valid receipt versus failed
Session completion publication and successor Execution ordering, avoiding existing
lease/stale completion coverage. No-change evidence is acceptable if covered.
