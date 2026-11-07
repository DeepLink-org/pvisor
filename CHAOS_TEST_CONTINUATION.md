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

Result: no confirmed defect. Extended an existing regression rather than adding
a duplicate. Valid receipt commits, owned Session destination directory makes
replacement fail with EISDIR, restoring saved stale Session permits repeated
receipt-authoritative reconciliation. Root selectors fail closed while metadata
is unreadable. Four old-Execution reconciliations preserve restoring/running
successor Session bytes; receipt/Bundle stay unchanged and no temporary leaks.
This directly exercises private publication, not live Session::finish callback.

Worker checks, UTC 2026-11-07, standard workload limits:

- Initial test expectation incorrectly assumed readable Session metadata during
  obstruction; corrected to require fail-closed reads. Not a product fix.
- 15:47:19–15:47:41: 22 selected completion/Session/registry/lifecycle tests
  passed, 744 filtered/skipped.
- 15:47:53–15:48:23: `mise run lint-rust -- -p pvisor`, all targets,
  warnings denied, passed. Final package fmt/diff checks passed.
- Minimum MemAvailable 24,213,229,568 bytes; maximum load1 2.2144;
  minimum free disk 366,887,735,296 bytes. Fixtures absent and local TMPDIR empty;
  no runtime children/mounts created.
- No live CLI/VM completion, process crash/power loss, failing sync acknowledgement,
  other platforms, Python, doctests or workspace validation.

Parent inspected the incremental existing-test diff and integrated it with README.

### 044 — Repeated VM teardown recovery

Assigned after round 043 workloads ended. Inspect new VM lifecycle ordering and
bounded cleanup/restart resources, reusing existing KVM tests where sufficient;
one conservative-memory VM at a time, no host configuration changes.

In progress: actual Linux KVM API version 12 verified. Initial targeted suite
passed 26 tests, including two real KVM microtests. A new bounded ready-callback
abort probe with caller-retained RAM/network aliases indicated retained vCPU,
device descriptors and network peer ownership after aliases were released.
The worker is investigating private Linux owner cleanup and all vCPU phases;
this is not yet a reviewed or published repair.

Two initial harness assumptions were corrected, not treated as product defects:
RAM backing includes firmware reserved space, and network NONBLOCK setup occurs
after guest queue activation, not at ready. The chosen repair direction is
coherent owner teardown, not a callback-only special case or knowingly detaching
leaked resources. Any synchronous thread join cannot guarantee a hard bound under
arbitrary kernel/device stalls; external command watchdogs remain in force.
At 15:58 UTC published round 043 CI had no failures but several pending checks.

At 16:06:56 UTC all 33 reported checks for published revision `d4e76456` were
successful, including aggregate CI and benchmark report. This is remote CI
evidence for rounds through 043, not evidence for the unmerged VM repair.

Confirmed worker reproduction at 15:56:02–15:56:03 UTC: baseline 3 fds /
1 thread / 0 KVM fds became 57 / 3 / 1 after callback rejection, and the owned
network peer did not see EOF within one second after caller aliases dropped.
Runner exited/reaped before later workloads.

Repair: private Linux VcpuHandle Drop disconnects both channels, kicks KVM_RUN
and joins; exited waits for owner disconnection instead of permanent barrier.
Queued pause/resume replies tolerate a vanished owner. Common Drop replaces the
test-only hook, with pre-resume/exited/queued-response private KVM tests and an
external API ready-abort hardware regression.

Final-source worker real acceptance at 16:07:02.129–16:07:02.394 UTC:
four invalid-firmware rejections interleaved with four ready aborts, then a healthy
guest prints VM_EXIT_SOURCE_OK and produces authenticated exit 0. Every clean
fd/TID set matches 3 fds / 1 thread / 0 KVM fds; intentional caller aliases alone
retain 6 / 1 / 0. One VM, one vCPU and 256 MiB configured RAM at a time.
This is real KVM, not mock execution. Whole round used 33 serial KVM constructions
and 15 pre-VMM loader rejections; no concurrent VM pressure.

Worker validation, standard limits:

- Targeted `mise run test -- pvisor-vm` with selected library KVM tests, external
  ready_abort_kvm, api_contract, api_boundary and repository_boundary:
  30 passed, 642 skipped. Five selected microtests and integration use actual KVM;
  API/notification tests do not establish guest execution.
- Package lib/tests Clippy and rustdoc with warnings denied passed.
- `cargo check --locked -p pvisor-cli -p pvisor-daemon -p pvisor-shim` passed.
- Targeted rustfmt/diff checks passed; final workload ended 16:07:29 UTC.
- All 42 pre/post gates passed: minimum MemAvailable 23,146,237,952 bytes,
  maximum load1 3.912109375, minimum free disk 364,507,107,328 bytes.
- A handwritten healthy-probe guest config initially omitted env and exited125;
  corrected using owning GuestConfig serialization, not a product fix.
- Existing local firmware was read, not modified. Build-script hardcoded guest
  output was redirected via a temporary worktree-local symlink into isolated
  output; symlink removed, no source-checkout output written. Evidence remains
  under ignored target/chaos044-evidence in the worker.
- All probe runners gone, owned RAM/rootfs fixtures removed, no fixture mounts.
- No SMP, concurrent VMs, cold-RAM pager, other platforms, full package/workspace
  or Python tests. Kernel/device stall can still make synchronous teardown wait.

Parent integrated and inspected the patch; independent review is checking channel
ownership, signal ordering, deadlocks, lifecycle phases and hardware regression.
Separate source-inspection finding: public readiness ordering says callback
precedes CPU execution, but private build resumes CPUs first. Not changed here.

Independent review requests changes: idle software parking checks channel
nonemptiness, which does not reveal disconnected empty channels. A parked CPU
can ignore the one-shot kick and leave synchronous Drop waiting indefinitely
without any host kernel/device stall. Idle PIO completion also clears immediate
exit before checking owner loss. This source-established path has not yet been
reproduced dynamically. Worker is fixing cancellation visibility in every phase
and adding bounded parked-owner coverage before publication.

Reviewer independently passed one real-KVM integration (two abort cycles),
27 idle tests and 15 vstate tests during 16:10:34–16:12:41 UTC. One initial filter
missed the intervening linux module; corrected filter ran vstate tests separately.
Minimum MemAvailable 23,170,170,880 bytes, maximum load1 2.52, minimum disk free
364,163,973,120 bytes. Hardware integration is ignored by default and not enabled
in CI; it is manual hardware evidence, not an automated CI gate.

Review follow-up reproduced parked-owner hang at 16:17:00–16:17:22 UTC:
event-only KVM case failed its one-second teardown receipt; disabling only the
fixture's idle gate recovered/reaped it without external timeout. Final fix
checks try_recv at the top of every host iteration and dispatches queued events
or disconnection, making cancellation independent of queue nonemptiness and kick.
No new cancellation flag or public API.

At 16:18:10–16:18:34 UTC, targeted tests passed 60, skipped 615: 27 idle units,
eight KVM microtest VMs and two serial production-library integration VMs.
New bounded cases cover event-only, claimed-after-wake and one-VM/two-vCPU dual
standby with sequential handle destruction; the latter is not broad SMP coverage.
Each has one-second receipt and one-second fixture-gate recovery on failure.

Final relinked real guest acceptance at 16:20:25.001–16:20:25.258 UTC repeated
four loader failures/four ready aborts with exact clean resource baseline, followed
by VM_EXIT_SOURCE_OK and authenticated exit 0. Final Clippy/rustdoc/consumer checks
and formatting/diff checks passed. Whole round totals now 49 serial VM
constructions, 30 workload invocations, all 60 resource gates passed; minimum
MemAvailable 23,058,665,472 bytes, maximum load1 3.912109375, minimum free disk
363,633,881,088 bytes. Final workload ended 16:21:08 UTC.
Owned fixtures/symlink removed, probe/red-test processes absent.

Parent merged final follow-up cleanly. Fresh independent reviewer is checking the
integrated cancellation correction and rerunning hardware cases before publication.

Final independent review: no blocking findings. Verified durable channel
cancellation, FIFO/control dispatch, PIO return ordering, paused/exited ownership
and no new busy loop on source review. During 16:23:21–16:25:57 UTC:

- Lifecycle/parked selectors: 23 passed, 659 skipped.
- Explicit production ready-abort hardware integration: 1 passed (two serial VMs).
- Remaining idle/deadline/dual/RAM controls: 16 passed, 666 skipped.
- Total 40 distinct tests, all 27 idle units and all three parked-drop regressions.
- Minimum MemAvailable 22,580,512 KiB; maximum load1 2.62;
  minimum disk free 363,095,670,784 bytes; all pre/post gates passed.
- Exact signal/PIO race supported by control-flow review, not deterministic race
  injection. Worker final guest/Clippy/rustdoc/consumer evidence not rerun by reviewer.

Parent accepts the focused Linux cleanup correction for publication. Kernel/device
stall, manual-only ignored integration, and broader platform limits remain explicit.

### 045 — VM readiness publication ordering

Assigned after round 044 final review workloads ended. Investigate documented
callback-before-CPU ordering versus private startup with bounded actual KVM
evidence and caller analysis. Architectural/cross-platform tradeoffs require
discussion before broad implementation; do not silently weaken the API contract.

Source/caller analysis: fresh Unix build resumes CPUs before on_ready, restored
starts paused; WHP starts paused and resumes after callback. Existing public API
also permits explicit restored resume, and a runtime caller resumes restored
before later fallible setup. Therefore transactional "Err means no guest side
effects" is incompatible with explicit-resume behavior without a larger change.

Bounded actual KVM probe at 16:30:19–16:31:17 UTC: two serial one-vCPU/32 MiB
custom long-mode guests write marker 0x45 in owned guest RAM then HLT. First
callback never calls resume: entry reports not paused, marker initially zero;
after bounded 100 ms callback delay and pause, marker is 0x45. Returning Interrupted
preserves the marker. Explicit-resume second variant behaves likewise.
This proves execution before callback returns, not strict dynamic observation of
execution before callback invocation; source lacks an ordering barrier.

No architectural repair or contract weakening made. Parent asked user to decide:
prepared/paused at callback entry, fresh auto-start only on success, explicit
resume side effects not rolled back, explicit pause honored. Independent storage
testing continues while this decision is pending. Diagnostic source/logs stay
under ignored isolated output, not a defect-expecting normal regression.

Final report: one diagnostic nextest passed (0.246 s, build 57.44 s), not a
passing-contract claim. Own tiny payload, not full firmware/Linux boot. Resource
pre/post: available memory 23.802/23.814 GB, load1 0.807/1.934, disk free
363.060/362.935 GB. No surviving fixture/vCPU processes observed; exact fd/TID
accounting not repeated. No Clippy/docs/consumer/platform checks because no repair.
Diagnostic/evidence artifacts remain only in child target-chaos-045 and are not
merged into the campaign PR.

Compatibility refinement reported to user: existing WHP smoke explicitly resumes
then pauses during callback and relies on successful return auto-resuming. Parent
therefore recommends preserving that automatic-success behavior while guaranteeing
prepared callback entry; honoring explicit pause persistently is a separate
behavior change. No implementation while user decision remains pending.

### 046 — Selective apply failure and retry

Assigned after round 045 hardware workload ended. Explore bounded owned
filesystem failure after partial selected application, retained candidates,
unrelated-path isolation and retry/review consistency, avoiding prior apply cases.

Result: no defect reproduced, no tracked code changes. Two tiny fixtures cover
legacy/compact journal. Existing private publication seam removes owned target
directory write permission before second replacement: real UID1000 gets EACCES
after one selected file succeeds. Failure preserves failed host path and all
candidate identities/bytes, preimages and Prepared ledger. Restoring permissions
permits completion of original intent; only selected candidates/preimages consumed.
Repeated recovery preserves a later host edit and unrelated remaining candidate.
This is logical idempotent commit, not exactly-once physical Prepared writes.
An empty owned temp directory may survive denied cleanup, per non-sweeping rules.

Worker window 16:32:06–16:37:28 UTC, standard workload limits:

- Existing apply recovery/stage tests: 17 passed, 160 skipped.
- Temporary diagnostic: 1 passed, two journal-format cases.
- Targeted lib/tests warnings-denied Clippy passed.
- Final restored-source apply/stage plus seal integration: 18 passed,
  160 skipped; formatting/diff checks passed.
- Minimum MemAvailable 21.73 GiB; maximum load1 1.58;
  minimum free disk 337.56 GiB. TMPDIR empty; no gate failures.
- Probe bypasses global apply locks; ordinary stage tests use documented fixed
  /tmp namespace. No shared locks removed or user mounts/processes touched.
- Temporary probe removed to avoid duplicate test bloat; audit patch remains
  child-local target-chaos-046/round046-probe.patch, not merged into PR.
- No crash/device/power-loss/concurrent-writer/public CLI/FUSE/VM/other-platform/
  full-package/workspace/Python coverage.

### 047 — Post-commit apply cleanup recovery

Assigned after round 046 workloads ended. Explore backup cleanup failure after
ledger commit, repeated recovery preserving later host edits and refusal to
consume unowned backup contents. No-change evidence acceptable if already covered.

Confirmed cleanup-only retry liveness defect, not data loss: after durable
Committed, a fixture-owned parent made nonwritable permits backup contents and
ownership receipt removal but denies final directory removal. Cleanup propagates
EACCES. Restoring permissions still leaves retry failing ENOENT because receipt
was removed. Ledger stays committed/no pending; later host edits, unrelated
candidate and journal remain intact. Replacing leftover with unowned sentinel
directory is refused without modification.

Initial diagnostic plus four existing tests passed (five tests, both formats).
Worker retained diagnostic evidence locally. Parent selected a focused live-error
repair rather than a new durable authority protocol: hold original verified
directory fd/receipt and attempt exact, exclusive, no-follow receipt restoration
through that fd if cleanup fails. Preserve original cleanup error and never
overwrite receipt collisions or authorize rebound pathname. Failed restoration,
process-death/power-loss gap remain explicit fail-closed limitations. Worker is
implementing and testing this before independent review/publication.

Worker repair integrated: verified directory fd retained; exclusive no-follow
openat creates exact receipt, then file/directory sync. Original cleanup error
remains returned; failed restoration adds explicit context with original kind.
Regression covers legacy/compact permission retry preserving later host edits and
unselected journal data; separate collision/moved-directory cases preserve file/
symlink collisions and do not authorize rebound unowned pathname.

At 16:45:06–16:45:10 UTC, targeted cleanup/recovery selectors passed 6 tests,
173 skipped; lib/tests warnings-denied Clippy, package fmt/diff passed. All round
workload gates: minimum MemAvailable 23,397,367,808 bytes, maximum load1 0.9297,
minimum free disk 362,231,959,552 bytes. Independent reviewer now checks authority,
error states and full overlay-core suite before publication.

Independent review accepted live-error scope without blocking findings.
At 16:46:21–16:46:35 UTC, exact `mise run test -- pvisor-overlay-core` passed
242 tests, 5 skipped, 12 binaries; effective UID1000 exercised both DAC cases.
MemAvailable 23,805,968,384→23,459,532,800 bytes, load1 0.575→1.026, disk free
362,235,666,432→362,173,673,472 bytes; gates passed.

Review caveats clarified before publication:

- Existing receipt collisions remain untouched; restoration context can be noisy
  if a valid receipt survived, but retry remains possible.
- Successful restoration preserves original cleanup error; wrapped restoration
  failure preserves error kind and text, not structured original raw errno.
- Incomplete write can leave invalid receipt; failed sync can leave complete
  readable bytes accepted by later recovery, not persistent poison. No new
  crash-durability protocol.
- Held directory secures restoration destination only; existing pathname-based
  cleanup is not made safe against every concurrent namespace mutation.
- Non-Linux, injected receipt write/sync failure and crash/power-loss not tested.

At 16:46:13 UTC published VM revision `6e02bfe5` had all 33 checks green,
including aggregate CI. This predates the cleanup repair.

### 048 — Core resource admission cancellation

Assigned after round 047 test workloads ended (documentation clarification only
remains). Explore bounded in-memory admission/cancellation/owner ordering and
quota recovery, avoiding earlier resource tests. No live network or host faults.

Round 047 publication note: comment-only follow-up merge duplicated the helper
with conflict markers. Parent resolved by retaining exactly one helper with the
clarified comments; final production/test diff matches independently tested code,
and `git diff --check` passed. No tests rerun after comment-only resolution.

Round 048 scope corrected before execution: Core has no lease/admission ledger.
Worker reported this without running tests; parent authorized owning
`pvisor-daemon` in-memory admission/cancellation tests instead. Do not interpret
Core contracts as resource-accounting coverage.

Result after scope correction: no product defect reproduced. New focused daemon
test covers independent CPU/memory/slot saturation crossed with original-owner
drop before/after caller abort (six scenarios). Native-entry oneshot establishes
durable Pending admission before completion. Cancellation correctly retains
capacity; confirmed deletion restores exactly one admission, stale delete cannot
release replacement. No sleeps, service listener, VM, subprocess or mount.

Worker checks, UTC 2026-11-07, standard limits:

- 16:50:29–16:51:34: three selected daemon tests passed, 132 skipped.
- 16:52:19–16:52:39: new test stress-count3 passed all repetitions.
- 24 new scenario instances total; 48 successful admissions, capacity rejections
  and confirmed deletions each, 24 stale-delete rejections.
- Targeted library/tests Clippy warnings denied, rustfmt and diff passed.
- MemAvailable 22,894,592–23,455,180 KiB; load1 0.50–2.14;
  disk free 361,647,357,952–362,082,021,376 bytes. Fixtures removed.
- No full daemon/workspace/Python, native runtime, cgroup, network, other-platform
  or parent-integrated-tree validation in this worker.

Parent inspected and merged focused test/README. Next worker will include its
integrated validation before a separate uncertain-create scenario.

### 049 — Daemon uncertain-create cancellation

Assigned after round 048 workloads ended. Explore caller cancellation combined
with runtime creation failure and uncertain cleanup, keeping quota fenced until
confirmed release, without live runtime/network services.

Result: no production defect reproduced. Extended the same inherited test with
35 net lines, not a duplicate. Twelve cases now cover CPU/memory/slots × two
owner/caller drop orders × successful/uncertain failed creation. Aborted caller
does not release quota. Failed creation plus Missing reconciliation keeps
reservation; two cleanup failures retain Stopping and 429 admission without a
second runtime create. Confirmed cleanup admits one replacement; stale deletion
cannot release it. Final counters 2 creates / 4 deletes in uncertain variants.

Worker checks, UTC 2026-11-07, standard limits:

- 16:54:51–16:55:55: inherited integrated048 test passed first.
- 16:56:40–16:57:00: six selected daemon cancellation/uncertainty tests passed,
  129 skipped.
- Package lib/tests warnings-denied Clippy, package fmt/diff passed.
- Minimum MemAvailable 22,869,300 KiB, maximum load1 2.53,
  minimum free disk 352,203,872 KiB. Local FakeRuntime only.
- No full package/native runtime; startup restart not combined with new matrix.

Parent inspected incremental matrix extension and merged it cleanly. Fresh
worker will run integrated full daemon suite before publication.

### 050 — Daemon control acknowledgement loss

Assigned after round 049 workloads ended. First full daemon integrated validation,
then bounded pause/resume acknowledgement/caller-cancellation and quota/intention
reconciliation ordering, reusing coverage where already sufficient.

Initial integrated full daemon run 16:58:39–17:00:05 UTC failed: 83 passed,
1 failed, 4 skipped, 99 not run under fail-fast. The inherited checkout path plus
campaign TMPDIR made a native Unix socket pathname exceed SUN_LEN in
downloads_honor_byte_ranges_across_guest_frames. Not attributed to a product
reliability defect; worker will retry once with shortest unique worktree-local
TMPDIR, retaining the failed run as evidence. Pre/post available memory
23,440,164/23,665,324 KiB, load1 1.46/2.27, disk free
360,292,823,040/359,707,897,856 bytes; gates passed.

Integrated rerun 17:00:50–17:01:51 UTC: exact `mise run test -- pvisor-daemon`
passed 183, skipped 4 across six binaries (36.168 s nextest). Only campaign setup
changed: unique short worktree-local `.t050` TMPDIR, representative socket bind
checked at 95-byte path first. No code changes for path limitation.
Pre/post MemAvailable 23,749,040/23,741,484 KiB, load1 1.56/2.68,
free disk 359,695,151,104/359,769,837,568 bytes; gates passed. This validates
final integrated rounds 048/049; initial failed run remains disclosed.

Round 050 result: no product defect reproduced. New eight-case matrix covers
pause/resume × lost/inconsistent acknowledgement × retry/newer queued control,
reusing a private bounded gate after simulated native change. Caller abort retains
intention, quota and exclusive ownership. Failed inspection preserves intent;
retry observes without repeating control. Newer opposite control wins through
GET and reopened disk records. Queue proof explicitly polls lifecycle write lock,
then invokes private locked control; not merely assuming task scheduling.

Worker final checks, UTC 2026-11-07, standard resource limits:

- 17:05:18–17:05:24: six selected regressions passed, 130 skipped.
- 17:05:24–17:05:25: final new test stress-count3 passed. Final-version total
  40 scenario instances; preliminary-version executions tracked separately (72
  including those, not 72 final-version cases).
- Targeted Clippy warnings denied and package fmt/diff passed.
- 17:05:42–17:06:19: final exact `mise run test -- pvisor-daemon`,
  184 passed, 4 skipped after FakeRuntime helper changes.
- MemAvailable 23,124,296–23,857,500 KiB; load1 1.02–2.68;
  disk free 359,161,151,488–360,292,823,040 bytes. Short fixture dir removed.
- New tests use FakeRuntime only; full package additionally runs existing local
  IPC/loopback fixtures. No VM/cgroup/cloud/Python/workspace/ignored/other-platform
  coverage. New cases saturate slots; CPU/memory covered by inherited matrix.

Parent reviewed the lifecycle lock, gate ownership and final-state assertions
and integrated test/README.

### 051 — Renewal versus expiry cancellation

Assigned after round 050 workloads ended. Explore accepted renewal cancellation
and stale TTL expiry fencing with deterministic in-memory fixtures, not real-time
waiting or live service/VM workloads.

Result: no product defect reproduced. A focused test blocks disk commit after
renewal validation, aborts public caller, waits lifecycle handoff, then invokes
stale selected expiry cleanup. Committed extension survives. Same-deadline retry
returns INVALID_REQUEST without reverting it. Directly modeled expired deadline
returns SANDBOX_EXPIRED; failed/unconfirmed delete each retain Stopping/quota
across two retries, confirmed delete admits replacement.
It does not enqueue actual maintenance during blocked commit or cross a real clock.

Worker checks, UTC 2026-11-07, standard limits:

- 17:09:27: seven selected daemon tests passed, 130 skipped.
- One initial new-line formatting difference corrected.
- 17:11:04: final package fmt and lib/tests warnings-denied Clippy passed.
- 17:11:33–17:11:53: same seven tests passed on final source (0.391 s nextest).
- MemAvailable >=22.48 GiB, load1 <=2.22, disk free >=333.96 GiB.
  Short 72-byte worktree-local TMPDIR empty and removed.
- FakeRuntime/disk-barrier only, no real wait/VM/service/cgroup, no full package.

Parent reviewed the lock-handoff/deadline/capacity assertions and integrated
regression/README.

### 052 — Registry uncertain-commit recovery

Assigned after round 051 workloads ended. Explore existing Store failure seams,
fail-stop mutation fencing and restart disk-winner recovery with local records,
avoiding duplicate coverage and physical-fault claims.

Result: no-code evidence, existing seven-case disk-winner test plus seven adjacent
tests satisfy scenario. Hooks fail before/after intention, native create/pause,
and unlink. Stale memory stays unpublished when disk wins; disabling injection
still leaves fail-stop fencing further operations/native calls/disk mutation.
Reopen chooses persisted state, preserves identity/credentials/resource quantities,
retains capacity and allows replacement only after confirmed cleanup.

At 17:13:35–17:14:41 UTC, targeted exact mise daemon selection passed 8,
129 filtered (0.947 s nextest, 66 s total fresh build). MemAvailable
24,114,960→23,893,164 KiB, load1 0.94→2.29, disk available
349,838,668→349,712,564 KiB; gates passed. Fixtures/short TMPDIR cleaned.
No source/docs edits; no Clippy/fmt because no edits. Private hooks/FakeRuntime,
not full suite/native VM/device/power-loss coverage.

### 053 — Malformed registry reopen recovery

Assigned after round 052 workloads ended. Explore tiny malformed/truncated owned
registry fixtures, non-destructive refusal and recovery after restoring exact
bytes, reusing existing coverage rather than duplicating it.

Initial selected storage checks at 17:16:19–17:17:24 UTC passed 4, skipped133:
malformed/missing metadata, owner/version/record-ID rejection, mismatch byte
preservation and separate healthy restart. No code changes. MemAvailable
24,087,820→23,802,424 KiB, load1 0.92→2.58, disk free
349,353,656→349,209,884 KiB; gates passed, fixtures/short TMPDIR cleaned.

Gap identified explicitly: existing tests neither truncate a valid record nor
restore exact original bytes and reopen that same fixture; malformed cases lack
direct byte-preservation assertion. Parent requested bounded follow-up to complete
this named recovery sequence rather than equating separate healthy-open evidence.
No Clippy/fmt run on no-change initial selection.

At 17:15 UTC published revision `5e0fa3da` CI had no failed checks; a few CLI/
report jobs remained in progress. Not final CI success.

Follow-up closes named recovery gap with 48 lines added to existing regression:
record/ownership metadata each truncated at zero/one/missing-final-delimiter bytes.
All six variants fail closed without modifying damaged bytes, counterpart,
neighbor record or header. Exact original-byte restoration allows the same fixture
to reopen with identity/version/record contents and exclusive lock intact.
No product defect reproduced or implementation change.

At 17:18:42 UTC selected two store tests passed, 135 skipped. Bare-shell Clippy
initially used rustc1.90 and failed dependency minimum1.91; rerun through
`mise exec` selected project toolchain and passed warnings-denied Clippy plus
formatting/diff by 17:19:45 UTC. Earlier bare format passed but is not project
toolchain evidence. MemAvailable 23,531,668–24,070,168 KiB, load1 1.11–1.62,
disk free 348,950,716–349,053,704 KiB; gates passed, fixtures/TMPDIR cleaned.
No physical crash/power-loss/sync durability/native VM/broader-corruption claim.

Parent reviewed incremental same-fixture recovery assertions and merged test/README.

### 054 — Real Host stagebox lifecycle recovery

Assigned after round 053 workloads ended. Diversify into actual low-load
spawn/review/selective apply/drop, guest failure/cancellation and later successful
run, with source preservation and owned process/mount cleanup checked.

Result: three real serial safe Host Sessions, no FakeRuntime or defect reproduced.
At 17:25:44.392–17:25:45.545 UTC, 24 CLI operations (23 success, expected guest
exit23 once) test failed stage A with background child, newer successful B
recreating/accepting A-deleted path, then healthy C. Selective apply/drop of older
failed stage preserves newer accepted file; unrelated scratch candidates remain
until dropped. Original host contents unchanged before apply and previews inert.
Final edit=accepted-from-failure, obsolete=reintroduced-success,
health=healthy3, sentinel=untouched; no scratch files. All stages discarded/empty.

Actual spawn uses Host, --safe, --overlaynet-deny-all, captured stdio, explicit
stage and tokenized Python fixture. Recorded owner PIDs gone, no token/cwd matching
descendants, readable mount-namespace fixture mounts or stage sockets. Failed
Session returned in0.256s before background child's natural3s deadline.

Worker checks, standard gates/limits:

- 17:22:34–17:24:13 UTC: `mise run build -- debug product` passed98.62s,
  existing local firmware, no downloads.
- Supported static product CLI executed scenario. Explicit GNU product target
  and musl nextest attempts failed during setup (unsupported target/missing
  musl compiler); not counted as lifecycle evidence.
- Three selected native mise pvisor-cli apply tests passed.
- Integrated final daemon store truncation/restoration regression passed afterward.
- Diff check passed. All30 guarded invocations admitted: minimum available memory
  23,986,913,280 bytes, maximum load1 2.70, minimum boundary diskfree
  354,402,889,728 bytes; final17:28:39 audit diskfree354,205,315,072.
- No source code changed. Diagnostic artifacts retained child-local .t054 and
  target-chaos-054, not merged. Temporary worktree target redirect removed;
  no source-checkout outputs written.
- No cancellation injection/VM/OCI/other-platform/concurrent-writer/full package/
  workspace/Python coverage in054.

### 055 — Real Host deadline and retained stage

Assigned to same isolated worker after054 for validated build reuse. Explore
documented bounded deadline/cancellation with owned descendant cleanup, selected
acceptance of retained stage and healthy successor, no unrelated process kills.

Result: no defect reproduced, same validated static CLI reused without build.
At 17:31:35.899–17:31:38.870 UTC three real safe Host Sessions:

- --timeout2s returned1 in2.172s; Bundle failed/deadline_exceeded, no workload
  exit code. Ready parent/descendant each had8s natural fallback.
- After pipe readiness, `pvisor kill STAGE --json` reported cooperative=true,
  returned0; spawn returned143 in0.232s, Bundle cancelled, no fabricated process
  failure/workload exit code. Descendant ignored SIGTERM but was cleaned.
- Healthy successor returned0 in0.184s and verified prior accepted state before
  staging next health file. Deadline/cancel stages selectively accepted result,
  dropped scratch; healthy stage fully applied/zero candidates.

All host sources unchanged before apply, preview inert. No owners/token/cwd
descendants, readable fixture mounts or stage sockets afterward, before8s natural
fallback. Final stages discarded/discarded/applied. Of23 CLI commands:20 exit0,
one expected deadline1, cancellation143, and fully-applied-drop refusal1.
Harness initially expected drop after full apply; product correctly refused,
accepted files unchanged. Token checker initially matched its own parent shell;
fixed to exclude only known checker/parent, not workload descendants.

One selected native apply CLI test passed at17:33:26 UTC; diff check passed.
All24 resource gates: minimum MemAvailable24,398,442,496 bytes, maximum load1
1.045, minimum disk free354,012,622,848 bytes. Final17:33:45 audit load0.65.
No tracked code changes; childlocal .t055 evidence not merged.
No frontend disconnect/setsid/concurrent writer/VM/OCI/other-platform/fullsuite/
Python coverage. Observed SIGTERM cancellation143 is not generic130 assumption.

### 056 — Real Host frontend disconnection

Assigned to same isolated worker for build reuse after055. Explore bounded owned
frontend disconnection and descendant/runner ownership, receipt authority and
Unknown admission before apply/drop. No unrelated process signaling.

Result: no defect reproduced, three real serial Host Sessions. Caller-owned stdio
EOF completed0 in0.216s with valid completed/exit0 receipt. Verified frontend
identity/parent/starttime then pidfd SIGKILL targeted only frontend, not timeout
wrapper or runner; wrapper Popen reported-9 in0.212s and owner committed cancelled
without workload exitcode. Healthy successor completed0 in0.179s. Both fault cases
had ready descendants with8s natural fallback, cleaned well before it.
Host unchanged before apply; product receipt/Bundle binding validated before
selective apply/drop; final accepted result/health/sentinel correct, scratch absent.

Neither actual fault produced Unknown. A separate existing synthetic missing-
receipt test passed (1 passed,765 skipped) and verifies typed Unknown review plus
blocked apply/drop/no invented completion. Do not claim real Unknown observation.
An initial harness expected shell137 instead of Popen-9; corrected classification
on original stage, without relaunching variant.

Fault execution17:37:06–17:37:07 UTC, same-stage reconciliation/healthy17:40:03,
synthetic test17:40:19–17:40:38; final audit17:41:00. Of23CLI commands22 exit0,
one expectedSIGKILL. Minimum available memory24,301,289,472 bytes, maximum load1
0.924, minimum diskfree354,046,697,472 bytes, all gates passed. No owners/
fixture descendants/mounts/sockets remain. No tracked source changes; evidence
childlocal. No real uncommitted window/setsid/VM/OCI/other-platform/concurrency/
fullsuite coverage.

### 057 — Real staged rename/deletion conflicts

Assigned same worker after056. Explore intervening owned host edits/type changes,
whole-selection refusal and selective recovery without overwriting host changes.
Completed machine artifacts stay ignored/childlocal, not campaign deliverables.

At17:42:07 UTC all33 CI checks for published revision`0cc99847` were successful.
Rounds054–056 add no code changes; their workflow evidence remains separate
from those CI checks and from pending readiness045 semantics.
