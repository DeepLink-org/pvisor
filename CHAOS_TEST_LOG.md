# Bounded chaos testing campaign

## Campaign

- Requested duration: eight hours of wall-clock time.
- Started: 2026-11-07 00:26:09 UTC.
- Deadline: 2026-11-07 08:26:09 UTC.
- Initial revision: `47afd887` on `main`.
- PR destination: a new dedicated PR, authorized by the user.
- Branch: `test/bounded-chaos-20261107`.
- Draft PR: https://github.com/DeepLink-org/pvisor/pull/111.
- Coordinator: this Delta thread. Each scenario runs in a separate worker
  subthread; the coordinator reviews and integrates results before starting
  the next scenario.

This is an execution log, not a claim that eight hours have completed.
Record interruptions and environment blockers separately from passing checks.
Do not merge into `main` as part of this campaign.

## Safety limits

- One active build/test workload at a time.
- At most two Cargo build jobs and one nextest/Rust test thread.
- Prefer `nice -n 10`; bound each build/test command to 900 seconds with
  termination followed by forced termination after 30 seconds.
- Check host memory, load, and disk before and after each workload. Do not
  start another workload if available memory is below 6 GiB, one-minute load
  exceeds 8, or free disk space is below 20 GiB.
- Keep fixtures and fault injection inside isolated workspace test directories.
  Never fill the host disk, change host networking, kill unrelated processes,
  use production credentials, or call paid services.
- Do not approve semspec cases, modify real approval ledgers/snapshots, or
  weaken assertions to make a scenario pass.

Initial host observation: 16 CPUs, approximately 31 GiB RAM, 20 GiB available
memory, 345 GiB free disk, load average 0.20, and approximately 8 GiB swap
already used. `/dev/kvm` and `/dev/fuse` exist; their existence alone does
not establish that an end-to-end VM/FUSE workflow works.

## Rounds

### 001 — Stage lifecycle failure paths

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOAA71HZIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: choose and execute a bounded stage/apply/drop failure scenario,
  using existing contracts to determine expected recovery and ownership
  behavior. Reproduce a defect before making a minimal fix and adding
  regression coverage.
- Added a regression matrix in `crates/pvisor-cli/tests/apply_cli.rs` for
  file, nonempty-directory, and symlink destination conflicts. Each case
  checks inert preview, whole-batch rejection on repeated execution, no
  apply ledger, successful selective retry after removing the obstacle,
  and idempotent drop preserving accepted files and a sentinel.
- Worker validation: `mise run test -- pvisor-cli -- --test apply_cli
  --no-fail-fast` passed all 16 tests, zero skipped. The worker also passed
  targeted rustfmt and `git diff --check`.
- Coordinator independently reran the same mise test command with
  `CARGO_BUILD_JOBS=2 NEXTEST_TEST_THREADS=1 RUST_TEST_THREADS=1`,
  explicit per-process mise trust/noninteractive settings, `nice -n 10`,
  and a 120-second timeout with 30-second kill grace. All 16 tests passed,
  zero skipped (nextest ID `46118438-1d78-4c1d-a6f4-7e1953b263a1`).
  Compilation took 61 seconds; test execution took 0.836 seconds.
  `git diff --check` also passed.
- Earlier unsuccessful attempts: worker initially hit a mise trust prompt
  and timed out at 900 seconds without compiling. Its first actual test
  run exposed an overly strict new assertion about the empty session lock's
  timestamps, not a product defect. The new test now permits only those
  execution-time timestamp updates while preserving other snapshot checks;
  preview still requires full equality. Existing assertions were unchanged.
- Coordinator resource readings before/after validation: available memory
  19,227,016 / 20,613,700 KiB; load1 3.33 / 3.39; free disk
  368,761,122,816 / 368,207,310,848 bytes. Swap remained almost full.
- Not covered: real VM/FUSE startup, interrupted processes, truncated metadata,
  other platforms, full-package tests, and Python suite.

### 002 — Truncated stage metadata

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOAA84HJIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: simulate interrupted metadata writes inside small stage fixtures.
  Check explicit failure, preservation of staged/accepted files, absence of
  unintended target writes, and recovery after restoring valid metadata,
  according to the existing contracts.
- Worker execution: 00:50:41–00:54:46 UTC (about 4 minutes).
- Added two tests in `crates/pvisor-cli/tests/apply_cli.rs`, injecting
  0-, 1-, and 32-byte prefixes of `run.json` and the committed apply ledger.
  Truncated run records block review/apply/drop without fixture changes.
  Truncated ledgers allow read-only review but block apply/drop, preserving
  accepted files, staged candidates, and metadata. Restoring known-good
  bytes permits normal operation; no automatic corruption recovery is claimed.
- Worker final validation: the same targeted mise command as round 001
  passed 18/18, zero skipped, in 21 seconds. Its first run passed 17/18:
  the new test incorrectly expected a direct EOF diagnostic rather than
  the existing discovery error for an unreadable indexed record. Only the
  new expectation was corrected; existing checks were not weakened.
- Diagnostic limitation observed: CLI discovery reports no readable Runs
  rather than the underlying corrupt run-record error.
- Coordinator independently reran `mise run test -- pvisor-cli -- --test
  apply_cli --no-fail-fast` with the same constrained environment and a
  180-second timeout. All 18 tests passed, zero skipped; compilation took
  16.65 seconds and execution 1.028 seconds. Nextest ID:
  `1334b165-335f-4266-a8ea-d39d9a342b1e`. `git diff --check` passed.
- Coordinator resource readings before/after: available memory
  21,023,788 / 20,897,952 KiB; load1 2.16 / 1.92; free disk
  367,449,432,064 / 367,423,479,808 bytes. Swap remained almost full.
- Not covered: power loss/fsync faults, intermediate apply transaction
  states, real VM/FUSE, other platforms, full-package/Python/semspec suites.
- Local commit: `f88493aa`. Two pushes to origin failed with a TLS
  unexpected-EOF error; this round is not yet published to PR #111.
  Retry publication after a later round rather than continuously retrying.
- Publication recovered after round 003: commits through `4d006e6a`,
  including round 002, were successfully pushed to PR #111.

### 003 — Interrupted apply transaction recovery

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOAA9z_pIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: inspect apply transaction recovery contracts and simulate a
  bounded intermediate state. Check repeated recovery, candidate preservation,
  and protection of subsequent host edits. Distinguish simulated state from
  actual process interruption or power loss.
- Worker execution: 00:57:09–01:01:40 UTC (about 4.5 minutes).
- Added two private tests (six combinations, legacy and compact journals) in
  `crates/pvisor-overlay-core/src/apply.rs`, plus README coverage notes.
  Prepared partial publication rejects later host conflicts repeatedly without
  consuming candidates. TargetApplied completion handles metadata publication
  failure and restart after journal consumption without replaying target writes
  or consuming twice. Accepted host edits and unselected candidates survive.
- These are precise simulated interruption states, not real process kills or
  power loss. Tests invoke lock-owned internals to avoid global lock files.
- Worker validation: six selected tests passed, 169 skipped; targeted rustfmt
  and diff checks passed. No product or test-logic bug reported.
- Coordinator independently ran the same six-test filter through
  `mise run test -- pvisor-overlay-core -- --lib -E FILTER`, with offline
  Cargo, constrained parallelism, nice, and a 180-second timeout.
  Six passed, 169 skipped; build 7.41 seconds, tests 0.021 seconds.
  Nextest ID: `0a6544d0-9093-4347-a0eb-4df26b72f1f6`.
  `git diff --check` passed.
- Coordinator available memory before/after: 20,349,216 / 20,035,512 KiB;
  load1 0.53 / 0.57; free disk 367,255,699,456 / 367,259,844,608 bytes.
- Not covered: public-entry lock behavior, whole-package/CLI/runtime/Python
  suites, true process crashes, power loss, concurrent host-write races,
  VM/FUSE, and other platforms.

### 004 — Gateway disconnect faults

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOAA-ib5IBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: use bounded local loopback upstream faults to test Gateway
  termination/error handling and a subsequent healthy request. At most
  20 requests, concurrency at most two, with explicit request timeouts.
- Worker execution: approximately 01:03–01:06 UTC.
- Added a raw loopback TCP test in
  `crates/pvisor-gateway/tests/model_wait_http.rs` and test README notes.
  Injected EOF before headers, during headers, and during a declared-length
  body. Each fault is followed by a healthy request on the same Gateway/story:
  six serial requests with five-second exchange/request/drain limits.
- All faults returned 502; subsequent requests returned exact healthy bodies.
  Each request had exactly one causal terminal event and lifecycle delivery.
  Upstream tasks joined and Gateway drained within the test limits.
- Worker validation: new test passed, then new plus adjacent failure test
  passed (two selected tests); targeted rustfmt and diff checks passed.
- Coordinator independently ran `mise run test -- pvisor-gateway -- --test
  model_wait_http -E FILTER` selecting the new EOF test and
  `supplier_connect_or_body_failure_reacquires_admission_before_gateway_error`.
  Two passed, 20 skipped; compilation 19.25 seconds, tests 0.132 seconds.
  Nextest ID: `bcf48470-7ed7-4a27-b2f9-18d1935d8940`.
  Constrained parallelism, offline Cargo, nice and 180-second timeout were
  used; `git diff --check` passed.
- Coordinator available memory before/after: 20,318,468 / 20,076,984 KiB;
  load1 1.16 / 1.52; free disk 367,029,436,416 / 367,002,959,872 bytes.
- Not covered: slow responses/inactivity timeout, TLS, downstream cancellation,
  no-lifecycle mode, whole-crate/Python suites, other platforms, or exhaustive
  task-leak detection. These are real TCP EOFs, not process-crash tests.

### 005 — Downstream cancellation isolation

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOAA_PYZIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: choose a currently uncovered downstream cancellation boundary,
  using real local sockets and deterministic handshakes; verify release and
  terminal-event behavior plus a healthy subsequent request before shutdown.
- Worker execution: 01:08:21–01:12:21 UTC (about four minutes).
- Added a three-mode matrix (no, single, repeated lifecycle) in
  `model_wait_http.rs`: client receives valid protocol terminal content,
  but disconnects before supplier HTTP EOF. Deterministic handshakes verify
  supplier body release within five seconds, one causal cancellation,
  precise received-byte count, and same-story healthy recovery before
  shutdown. Fixture capture notifications replace any need to poll.
- Worker final test passed; its initial compile failed due to a new test's
  `Option<String>` comparison, corrected without production changes.
  Targeted rustfmt and diff checks passed.
- Because the common fixture changed, coordinator ran the entire integration
  binary: `mise run test -- pvisor-gateway -- --test model_wait_http
  --no-fail-fast`. All 23 tests passed, zero skipped, including rounds 004
  and 005. Build 1.20 seconds; tests 1.766 seconds. Nextest ID:
  `0632835b-e7c9-4a09-8825-24ca15210f6c`. Offline Cargo, constrained
  parallelism, nice and 180-second timeout used; diff check passed.
- Coordinator available memory before/after: 20,869,868 / 20,601,292 KiB;
  load1 0.95 / 0.96; free disk unchanged at 364,894,683,136 bytes.
- Not covered: HTTP/2, TCP half-close, provider-side compute/billing
  cancellation, VM/native reclaim, other integration binaries/platforms,
  workspace or Python suites. Body-owner release is not proof that a real
  remote supplier stops computing.

### 006 — Journal torn-write recovery

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABAGVZIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: select an uncovered journal persistence boundary under the
  documented public API, inject a small torn-write/corrupt-tail or inconsistent
  auxiliary-file state, and check committed-prefix preservation and subsequent
  append/query behavior according to the actual recovery contract.
- Added one external API contract test in
  `crates/pvisor-journal/tests/api_contract.rs`: a complete record closes an
  unresolved forward reference into a causal cycle, followed by an incomplete
  tail. Read/validate/open reject without truncating any bytes. After explicit
  fixture repair removes only the complete corrupt record, open repairs only
  the torn tail and preserves receipts, pending references and append offsets.
- Worker full-crate validation passed 29/29, zero skipped, in 11 seconds;
  targeted rustfmt and diff checks passed, with no test-logic failure.
- Coordinator independently ran `mise run test -- pvisor-journal`, using
  constrained parallelism, offline Cargo, nice and a 180-second timeout.
  All 29 tests passed, zero skipped (including API boundary guards and the
  existing seeded state-machine test); build 5.42 seconds, tests 0.452 seconds.
  Nextest ID: `66d21879-d853-4d13-acc8-ecb2c0fd883d`.
  `git diff --check` passed.
- Coordinator available memory before/after: 20,311,064 / 19,961,852 KiB;
  load1 0.50 / 0.59; free disk unchanged at 364,713,431,040 bytes.
- Not covered: actual crashes, power loss, page-cache loss, fsync/device I/O
  faults, auxiliary-file inconsistency, other platforms, clippy/rustdoc/doctest,
  Python or workspace suites. This is closed-file state injection.

### 007 — Overlay namespace failure sequences

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABAZcpIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: select an uncovered bounded rename/link/unlink failure sequence
  and compare filesystem behavior against native operations or documented
  contracts. Attempt a small mounted workflow if supported; distinguish
  API-only evidence from real FUSE operations.
- Added an ignored-by-default Linux FUSE integration test and crate README
  instructions. Fixed trace 007 performs 24 operations against a native
  filesystem oracle, comparing errno, visible tree/content, and hardlink
  equivalence after each step. It checks lower content/namespace isolation
  and explicit unmount after repeated rename/link failures and retries.
- Worker ran the explicit mounted test twice successfully and ran
  `mise run test -- pvisor-overlayfs pvisor-overlay-core`: 310 passed,
  15 skipped. It also passed targeted rustfmt and diff checks.
- Scope deviation: that existing regular overlay-core suite accesses its
  fixed `/tmp/pvisor-apply-locks-1000` namespace despite TMPDIR. The worker
  did not remove the shared directory. The new mounted trace itself keeps
  fixtures inside the workspace. Do not claim this entire round had zero
  writes outside the workspace; avoid repeating that broad suite.
- Coordinator independently ran `mise run test -- pvisor-overlayfs --
  --test namespace_sequence_mount --run-ignored ignored-only --no-capture`.
  Real mount, all 24 operations, and unmount passed (one test, zero skipped).
  Compilation 4.99 seconds, test 0.013 seconds; nextest ID:
  `f19c2d3f-52da-4a41-b99d-cc3c28787e4c`. Constrained parallelism, offline
  Cargo, nice and 180-second timeout used; diff check passed. Nextest warns
  no-capture ignores thread count, but only one test was selected.
- Coordinator available memory before/after: 19,772,892 / 19,727,648 KiB;
  load1 4.36 / 4.33; free disk 363,586,682,880 / 363,535,523,840 bytes.
- Not covered: concurrent races, crash recovery, permission/xattr contracts,
  complete metadata isolation, long-TTL cache policy, macOS, VM, Python or
  workspace suites. Skipped tests are not counted as passing.

### 008 — OverlayNet malformed packet isolation

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABBBk5IBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; one product defect reproduced and fixed.
- Budget: 30 minutes.
- Assignment: select an uncovered malformed/truncated packet or DNS policy
  boundary through a real input path, at most 128 small deterministic packets.
  Check rejection/no panic and healthy subsequent input using memory or
  loopback only, without host network configuration changes.
- Defect: DNS wire-name length accounting omitted the final root octet,
  accepting a 256-byte encoded name and allocating a synthetic address.
  RFC 1035's 255-octet limit includes that terminator. Minimal implementation
  fix initializes encoded length to one instead of zero in `vm.rs`.
- Added `tests/vm_dns.rs` and README contract notes. A deterministic framed
  Ethernet/IP/UDP/DNS trace traverses the public `VmNetwork` guest link and
  actual smoltcp backend under NoNetwork policy. It covers truncated
  questions, overlong labels, unsupported compression, 256/257-byte names,
  valid 254/255-byte boundaries, and healthy queries after each rejection.
  Sequential addresses confirm invalid questions do not allocate slots.
- Worker reproduced the unfixed defect: targeted vm_dns exit 100 after
  21 seconds, with RCODE 0 for a 256-byte name instead of FORMERR.
  After the fix, 14 selected VM/DNS tests passed, 76 skipped.
  No test-logic error reported; targeted rustfmt and diff checks passed.
- Coordinator independently ran `mise run test -- pvisor-overlaynet --
  --lib --test vm_dns -E 'test(vm::tests::) | binary(vm_dns)'`:
  14 passed, 76 skipped; build 8.03 seconds, tests 0.087 seconds.
  Nextest ID: `f830897e-8361-4928-8d6e-21844965e92c`.
  Then `mise run check -- -p pvisor-cli` passed in 26.01 seconds, compiling
  affected runtime/CLI consumers. Offline Cargo, two build jobs, single
  test thread, nice and bounded command timeouts were used.
- Coordinator available memory initially/finally: 19,926,756 / 19,021,464 KiB;
  load1 0.75 / 1.54; free disk 362,940,379,136 / 362,915,807,232 bytes.
- Not covered: new IP/TCP/UDP fragmentation cases, peer full suite, external
  egress/security attacks, real VM boot, host isolation, other platforms,
  workspace or Python suites. This is public packet-link testing, not a
  claim of host-wide network isolation.

### 009 — Replay mismatch recovery boundaries

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABBl05IBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: choose an uncovered replay mismatch/cancellation/corrupt-trace
  boundary through a public entry, checking isolation, cursor consumption
  and behavior after rejection against documented semantics. Small synthetic
  records only, no external LLM services.
- Added one public-entry `replay_contract.rs` test: eight native records each
  for Codex and OpenCode, corrupt middle observation twice, then explicitly
  repair and retry the identical PrepareOnly request. Rejections name the
  correct line and leave input/workspace/state/output untouched. Retry prepares
  exactly the selected first-batch prefix, excludes later user work, and
  executes zero tools. No real agent or model is invoked.
- Worker selected-suite run passed 14 tests, 135 skipped; targeted rustfmt
  and diff checks passed. Only an initial formatting difference was corrected.
- Coordinator independently ran `mise run test -- pvisor-replay -- -E
  'binary(replay_contract) | test(parser_tolerates_only_a_truncated_final_jsonl_line)'`.
  Fourteen passed, 135 skipped; compilation 13.25 seconds, tests 0.651 seconds.
  Nextest ID: `00751c1c-2235-458d-a7e2-42c5fdf34ab8`. Offline Cargo,
  constrained parallelism, nice and 180-second timeout used; diff check passed.
- Coordinator available memory before/after: 19,605,264 / 19,308,200 KiB;
  load1 0.81 / 1.25; free disk 362,246,897,664 / 362,219,892,736 bytes.
- Not covered: cross-session mixing, HTTP request ordering/idempotency, stream
  cancellation, real CLI/agent/LLM end-to-end, other platforms or full suite.

### 010 — Executor startup/termination failures

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABCDipIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: exercise a public executor/Session failure boundary, preferably
  real Host staged execution with abnormal child termination, and check
  cleanup, review/drop preservation, and a subsequent healthy Session.
  At most eight serial Sessions with bounded child execution; no downloads
  or global process/mount operations.
- Added a public CLI `rootless_local.rs` regression with three serial real
  Host safe Sessions: stage a file then exit 23, stage then self-TERM
  (exit 143), and a subsequent healthy Session. It checks terminal/live
  status, ProcessExit failure kind, unchanged lower, reviewable upper,
  owner absence under /proc, no fixture mounts, drop and discarded state.
- Worker ran the test twice (six Sessions total); both passed. Targeted
  rustfmt and diff checks passed; no test-logic error.
- Coordinator independently ran `mise run test -- pvisor-cli -- --test
  rootless_local -E 'test(safe_abnormal_exits_retain_reviewable_stages_and_release_owners_before_retry)'
  --no-capture`, explicitly unsetting `PVISOR_TEST_ALLOW_NO_USERNS`.
  One passed, 18 skipped; real per-scenario diagnostics prove no capability
  skip. Build 16.99 seconds; test 0.539 seconds. Owners 1839057, 1839124,
  1839190 were gone; all three review/drop flows succeeded.
  Nextest ID: `6ff15e24-3884-48ac-9aaf-6228514f88ac`.
- Coordinator available memory before/after: 19,560,780 / 18,845,188 KiB;
  load1 0.88 / 0.88; free disk 361,967,767,552 / 361,890,631,680 bytes.
  Constrained jobs, offline Cargo, nice and 180-second timeout; diff check passed.
- Not covered: startup-path failure, cancellation, escaped descendants,
  VM boot/retry, other platforms or full-package/workspace suites.

### 011 — Mounted open-file namespace churn

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABCXyZIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: real FUSE/native-oracle comparison while file descriptors remain
  open across rename-overwrite/unlink boundaries. Check inode ownership,
  continued descriptor access, lower isolation, and explicit unmount, using
  a bounded single-thread trace rather than repeating trace 007.
- Extended the existing mounted-oracle fixture with fixed 21-step trace 011:
  held independent/duplicated descriptors survive rename replacement,
  hardlink/last-link removal, detached writes and same-name recreation.
  After each step, compare all live fd bytes, EOF and inode-equivalence
  groups, visible tree and lower isolation against native filesystem behavior.
  Real mountinfo presence and absence after explicit unmount are checked.
- Worker selected trace plus eight API contracts passed (nine tests).
  First run failed before the trace because the new mountinfo check accepted
  only `fuse.*`, not valid `fuse`; that test-only assumption was corrected.
  Targeted rustfmt and diff checks passed.
- Coordinator ran both mounted traces and API contracts:
  `mise run test -- pvisor-overlayfs -- --test namespace_sequence_mount
  --test api_contract --run-ignored all --no-fail-fast`.
  Ten passed, zero skipped; build 0.35 seconds, tests 0.109 seconds.
  Nextest ID: `ae433342-ebc1-45fc-8499-dc66b708c329`.
  Offline Cargo, constrained parallelism, nice and 180-second timeout;
  diff check passed.
- Coordinator available memory before/after: 19,324,660 / 19,227,468 KiB;
  load1 0.52 / 0.63; free disk unchanged at 361,781,010,432 bytes.
- Not covered: directory fds, cross-process ownership, concurrency, mmap,
  other cache policies/macOS, full-package/workspace/Python suites or rustdoc.
- Publication initially failed with transient TLS EOF; a single retry
  successfully published commit `55385b50` and the round summary to PR #111.

### 012 — Core policy/accounting failures

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABC-U5IBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: choose an uncovered budget/policy rejection, cancellation or
  transition boundary on a real Core controller path, checking accounting
  against its documented guarantees with at most 128 bounded operations.
- Actual focus was Core authorization, not a budget ledger (Core does not
  own that ledger). Added `tests/network_controller_recovery.rs`, exercising
  48 combinations of address class, resolution mode and controller transition,
  each followed by a healthy authorization: 96 public operations total.
  Checks preflight precedes controller, explicit deny blocks alias fallback,
  invalid controller state fails closed, no cached decision, and unchanged input.
- Worker passed the full Core crate: 88 tests, zero skipped. Its first compile
  failed due to moving a non-Copy Result in the new test; corrected with clone.
  Fixture deny placement was corrected to use canonical capability before
  assertions ran. Targeted rustfmt and diff checks passed.
- Coordinator independently ran `mise run test -- pvisor-core`: 88 passed,
  zero skipped; build 5.26 seconds, tests 0.310 seconds. Nextest ID:
  `79b80c96-347d-40b7-ba74-b398d5dd3e06`. Offline Cargo, constrained
  parallelism, nice and 180-second timeout; diff check passed.
- Coordinator available memory before/after: 19,281,792 / 18,847,836 KiB;
  load1 0.77 / 1.05; free disk 361,650,708,480 / 361,606,340,608 bytes.
- Not covered: budget accounting, cancel/complete idempotency, real DNS/network,
  cross-crate execution, concurrency, other platforms, workspace/Python/doctest.

### 013 — VM lifecycle faults

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABDeeJIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed; second product defect reproduced and minimally fixed,
  with post-fix real-boot acceptance assigned to round 014.
- Budget: 30 minutes.
- Assignment: check existing local KVM boot dependencies without downloading
  images/firmware. If available, exercise at most four serial tiny VM failure/
  healthy lifecycles; otherwise record the boot blocker and test public API
  initialization failure separately. Reuse sufficient existing coverage.
- Local KVM API 12 and existing firmware/musl tooling enabled real boot without
  downloads. Four VM constructions across three bounded runners, each 256 MiB
  and one vCPU: missing init returned 125; real guest marker and exit 7 were
  observed; ready-callback Interrupted returned the original error, invalidated
  the stale control handle and released KVM fds, then same-process healthy
  boot produced a guest marker and exit 0.
- Defect: callback abort drops the exit-status receiver; the vCPU exit path's
  send `expect` panics with `failed to send Exited status: SendError(..)`.
  Minimal Linux implementation change finishes the state machine when the
  owner is disconnected; live-owner status delivery/event notification stay
  intact. Two private no-hardware tests cover disconnected and live owners.
- Worker post-fix validation: targeted `mise run test -- pvisor-vm --
  --offline -E 'test(exit_notification_) | binary(api_contract) |
  binary(api_boundary) | binary(repository_boundary)'` passed 24 tests,
  651 skipped, in 35.06 seconds. CLI/daemon/shim consumers compiled in
  46.07 seconds; rustdoc with warnings denied passed in 7.05 seconds;
  edition-2024 rustfmt and diff checks passed. Initial edition-2021 format
  invocation was the wrong check, corrected without changing old code.
- Worker resources initially/finally: available memory
  19,216,632 / 19,150,592 KiB; load1 0.68 / 3.52; free disk
  361,584,832,512 / 358,922,768,384 bytes. All runners reaped, KVM fd count
  returned to zero on abort; owned rootfs fixtures removed with no mounts.
- Ignored `target/chaos013/` holds temporary harness sources; do not commit
  these machine-specific probes as portable product tests.
- Limitation: the four-VM cap was reached before the fix. Round 013's real
  boots are pre-fix evidence, not post-fix acceptance. No whole-workspace,
  cross-platform or power-loss validation.

### 014 — Post-fix real KVM acceptance

- Worker: same subthread as 013, follow-up turn.
- Status: completed; post-fix real KVM acceptance passed.
- Budget: 20 minutes, at most four VM constructions.
- Assignment: rebuild/relink from repaired source and run callback-abort then
  same-process healthy boot twice. Check original error, invalid stale handle,
  KVM fd release, absence of background panic and real guest success.
- Worker execution: 02:01:00–02:02:33 UTC. Rebuilt repaired library and
  relinked the temporary probe from fresh Cargo artifact paths. Two serial
  abort-then-boot runners (four VM constructions) passed in 0.156/0.113 seconds,
  each 256 MiB/one vCPU. Original Interrupted preserved, stale handle invalid,
  KVM fds zero, actual aborted vCPU TID disappeared after bounded wait,
  subsequent guest printed `VM_EXIT_SOURCE_OK` and exited zero. Both complete
  stderr captures were empty. Unlike round 013, this is post-fix evidence.
- Worker repeated the 24 selected VM/API/boundary tests: 24 passed, 651 skipped.
  Runners reaped, owned rootfs removed, no fixture mounts. No new tracked
  implementation changes; hardware evidence is worker-run, not coordinator-run.
- Coordinator reviewed the implementation and independently reran the same
  targeted `mise run test -- pvisor-vm -- --offline -E FILTER`:
  24 passed, 651 skipped; compilation 32.70 seconds, tests 2.378 seconds.
  Nextest ID: `140acd2c-e886-4f35-9fdf-4b6d5aa71253`.
  Diff check passed; constrained parallelism, offline Cargo, nice and
  180-second timeout maintained.
- Coordinator available memory before/after: 19,213,792 / 18,927,140 KiB;
  load1 0.75 / 1.46; free disk 358,174,982,144 / 358,174,674,944 bytes.
- Not covered: SMP, snapshots, running-VM forced abort, other platforms or
  whole-workspace tests. No additional permanent probe was added.

### 015 — Bandwidth cancellation/policy changes

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABFhl5IBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: investigate an uncovered OverlayNet bandwidth waiting/cancellation
  or policy-transition boundary through real runtime paths; at most 64 operations
  and two concurrent tasks with bounded waits, no external network traffic.
- Added two `bandwidth_lifecycle.rs` public-router tests with in-memory sink
  and paused Tokio time, plus tower dev-dependency (no dependency upgrade).
  Upload/download cancellation releases ownership; after the reserved deadline,
  stacked budgets admit later requests on the expected schedule. Policy
  replacement retains shared global debt, gives new target rules independent
  buckets and keeps existing requests' policy snapshots. No refund promise
  is assumed. Eight requests total, at most two active.
- Worker passed two new tests, then 13 related bandwidth/egress tests
  (78 skipped); warnings-denied targeted clippy, rustfmt and diff checks passed.
- Coordinator independently ran `mise run test -- pvisor-overlaynet --
  --lib --test bandwidth_lifecycle -E 'test(bandwidth::tests::) |
  test(egress::tests::) | binary(bandwidth_lifecycle)'`: 13 passed, 78 skipped.
  Build 2.01 seconds, tests 0.049 seconds. Nextest ID:
  `7037f6f5-1487-4bd6-83c3-826125d9bf7c`. Offline Cargo, constrained
  parallelism, nice and 180-second timeout; diff check passed.
- Coordinator available memory before/after: 19,079,392 / 19,129,052 KiB;
  load1 2.85 / 2.49; free disk unchanged at 357,608,329,216 bytes.
- Not covered: VM data-plane bandwidth policy replacement (the new swap test
  uses proxy snapshots), real network speed, actual VM, Core budget ledger,
  full OverlayNet/workspace suites or other platforms.

### 016 — Selective apply alias isolation

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABGJRJIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: investigate an uncovered hardlink/symlink alias boundary in
  selective apply/drop, preserving unselected candidates and later host edits
  under the documented selection rules. Small fixture, no real user paths.
- Added two CLI regressions covering upper hardlink dependency selection,
  excluded required siblings, host-alias edits after preview, preservation of
  unselected candidates, and accepting symlinks without accepting or writing
  through referents. Accepted hardlinks share an inode; uncopied host aliases
  keep their old content. Later host edits block the entire dependency group.
- Worker passed the two regressions and separately exercised real Host/FUSE
  spawn→preview→host edit→blocked apply→symlink apply→drop. Early probe
  assumptions were corrected: default safe deny rejects multi-linked access
  unless explicitly cleared, and preview needs an existing mutation lock
  (initialized through a public zero-match apply). Neither was a new bug.
- Coordinator ran the full apply_cli binary: `mise run test -- pvisor-cli --
  --test apply_cli --no-fail-fast`: 20 passed, zero skipped. Build 23.28
  seconds, tests 1.180 seconds; nextest ID:
  `51b300db-6d1f-4676-848b-aed2e0b3a406`. Diff check passed.
- Host load briefly reached 7.81 before validation, so coordinator reduced
  build jobs to one and used nice 15. An explicit pre-exec gate measured
  load1 6.14, available memory 18,949,432 KiB and free disk 356,485,435,392
  bytes, all safe. After validation: load1 4.54, available memory
  18,483,832 KiB, free disk 356,347,248,640 bytes.
- Public apply uses fixed /tmp lock files; this known behavior is not
  described as zero workspace-external writes and no shared locks were removed.
- Not covered: concurrent namespace attacks, power loss, macOS, full-package,
  workspace or Python suites. Worker real-chain evidence is separate from
  coordinator fixture-based CLI regressions.

### 017 — Concurrent apply/drop admission

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABHaZZIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed and integrated; no product defect reproduced.
- Budget: 30 minutes.
- Assignment: deterministic same-stage mutation contention through public CLI
  operations, at most eight small groups/two processes; check single ownership,
  no duplicate consumption, accepted-file preservation and later usability.
- Resource adjustment: one Cargo job and nice 15 by default following observed
  higher host load; existing memory/load/disk admission gates remain mandatory.
- Added one public CLI regression: hold the actual admission inode, then start
  two contenders (apply/drop and apply/apply). All reject as busy without
  fixture changes. After release, selective apply succeeds exactly once,
  diagnostic owner clears, both leases reacquire, and drop preserves accepted
  content. Deterministic admission exclusion, not random scheduling.
- Worker passed the new test and two existing runtime lease/owner tests;
  corrected only formatting. CLI children individually bounded to eight seconds.
- Coordinator reran full apply_cli: `mise run test -- pvisor-cli -- --test
  apply_cli --no-fail-fast`: 21 passed, zero skipped. Build 1.02 seconds,
  tests 1.161 seconds; nextest ID:
  `c3a23d77-67f8-4be3-ba18-aa53e8fdd2db`. One build job, single test thread,
  nice 15, offline Cargo and 180-second timeout; diff check passed.
- Coordinator available memory before/after: 18,982,644 / 18,556,644 KiB;
  load1 0.99 / 0.91; free disk 353,707,933,696 / 353,504,993,280 bytes.
- Not covered: contention after a writer enters writeback, all possible
  interleavings, interrupted processes, macOS/VM/FUSE or workspace tests.
  Known fixed /tmp apply locks were used, not removed.

### 018 — Host background-child teardown

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABIMD5IBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed; target teardown contract passed, no tracked code change.
- Budget: 30 minutes.
- Assignment: inspect and test a documented Host Session teardown boundary with
  background descendants/held pipes, bounded to six serial Sessions and at most
  three processes each. Descendants must naturally stop within three seconds;
  no unrelated process signaling. Check stage retention and healthy reuse.
- Worker actual CLI scenarios: background child holds directory/file fds,
  inherited stdout/stderr and a local listener, signals readiness, then has
  a three-second natural deadline. Foreground normal exit, exit 7, and
  public owner cancellation completed in 0.168–0.184 seconds; later healthy
  safe Session completed in 0.151 seconds. Expected completed/failed/cancelled
  states, retained initial staged write, unchanged lower and review/drop passed.
- Process identities (PID plus starttime) disappeared, listener closed,
  no fixture mounts/processes remained, six stages had no live control/socket.
  Six Session-start cap included two unsuccessful probes, not six target passes.
- Non-counted attempts: initial cwd/stage layout setup failed; one probe
  executable had not been written; a checker hit Unix socket path length
  (rechecked in the same Session using relative path). Initial layout failure
  is not classified as expected or a bug yet; round 019 investigates.
- Worker built actual CLI offline with one job/nice 15 (116.19 seconds).
  No `mise run test` was executed this round: evidence is actual workflows,
  not a conventional test-suite pass. Existing related tests were read only.
- Worker resources: available memory 17.6–18.2 GiB, load1 0.98–1.84,
  free disk 328–329 GiB. Ignored `target/chaos018/` holds local probes;
  no need to merge them as permanent tests. Coordinator did not duplicate
  real Session runs.
- Not covered: setsid escape, signal-ignoring descendants, sustained writes,
  macOS/OCI/VM or full suite.

### 019 — Host cwd/stage layout failure

- Worker: same subthread as 018, follow-up turn.
- Status: third product defect reproduced, fixed and independently reviewed.
- Budget: 20 minutes, at most six serial Session starts.
- Assignment: reproduce and classify round 018's setup failure with exact
  cwd/HOME/TMPDIR/stage topology, comparing documented supported layouts.
  Do not relax recursive-backing safety or misclassify a rejected configuration.
- Worker reproduced an actual supported-layout failure: external stage
  succeeded; `workspace/experiment` stage failed before workload with Landlock
  ENOENT opening the hidden merged backing. HOME/TMPDIR need not be nested to
  trigger it. Existing nested-stage exclusions establish this is supported,
  not a dangerous recursive layout to reject.
- Fix removes only the nested staged-workspace source's redundant read-write
  grant after synthetic-root rebinding hides that source. Visible COW workspace
  grant remains; no generic path errors are ignored. Added a three-layout real
  CLI regression and runtime README notes.
- Worker passed the new three-Session regression, three existing planning/
  exclusion tests, and an extra real run with cwd/HOME/TMPDIR/RUN_HOME/stage
  all inside the project. Original six-start budget met. Review/drop, lower
  isolation and owned process/mount cleanup passed.
- Coordinator independently ran three selected rootless_local tests through
  `mise run test -- pvisor-cli -- --test rootless_local -E FILTER --no-capture`,
  unsetting optional-userns skipping. New external/nested/nested-home cases,
  existing ordinary-vs-safe behavior and abnormal-exit cleanup all executed:
  three passed, 17 skipped. Build 19.31 seconds, tests 1.270 seconds.
  Nextest ID: `90862d92-e7c2-4f41-bf94-74e8467f777c`.
  One build job, nice 15, offline Cargo and 180-second timeout; diff check passed.
- Coordinator available memory before/after: 18,894,108 / 18,522,748 KiB;
  load1 0.60 / 0.84; free disk 351,367,610,368 / 351,384,051,712 bytes.
- Independent static reviewer:
  `ksQQ3UP44YPgTC-msj8WDYNdyJLOABJli5IBxBDuNWdurkVNCYkuySUr0iCB`.
  Approved the production fix: canonical component-aware descendant check,
  subtractive exact-path change, retained rebound workspace COW grant and
  unchanged non-nested behavior. Suggested nonblocking regression improvement
  for literal relative `--stage ./experiment` and explicit backing probes;
  assigned to round 020 rather than claiming existing tests establish these.
- Not covered: full rootless_local/workspace suites, macOS, VM, every recursive
  layout or concurrent namespace attack. Machine-specific ignored probes are
  not staged for Git.

### 020 — Relative nested-stage backing boundaries

- Worker: same subthread as 018/019, follow-up turn.
- Status: completed; reviewer-requested regression strengthened and final
  coordinator rerun passed after fixture portability correction.
- Budget: 20 minutes, at most six serial Sessions.
- Assignment: strengthen the round 019 regression with literal relative stage
  arguments and precise stage/backing invisibility/protection probes, including
  the real external path rather than an unrelated relative path. Distinguish
  harmless new COW names from access to actual management backing.
- Worker strengthened literal relative arguments and actual absolute management/
  backing probes. External merged alias is a permitted COW workspace view,
  not direct management backing; nested stage/backing remain hidden.
- Coordinator targeted rootless rerun failed (nextest
  `f96dd693-6b3f-42dc-a1ee-244f3b9998a7`): external workload exited one before
  printing its probe path when tempfile used the terminal's default private
  /tmp rather than an explicitly workspace-local TMPDIR. This is not a passing
  regression and has not been classified as a new product bug. Worker asked
  to locate the exact failing assertion and make the new fixture explicitly
  workspace-local, without weakening real backing protection.
- This also exposes a validation-scope limitation: coordinator commands that
  did not explicitly set TMPDIR allowed existing tempfile-based tests to use
  the terminal's per-turn /tmp. Do not claim those fixtures were all physically
  inside the worktree; they were disposable test roots, not real user data.
- Failed rerun resources: available memory 18,832,920 / 18,676,476 KiB,
  load1 0.54 / 0.54; free disk 351,344,267,264 / 351,350,886,400 bytes.
- Follow-up makes the fixture explicitly local with
  `tempdir_in(CARGO_MANIFEST_DIR)` and captures shell trace/Bundle workload
  output on failure. Exact original failing subcommand was not retained;
  private-/tmp alias masking and path canonicalization were hypotheses,
  not independently proven root causes. No product behavior was changed.
- Literal `--stage ./experiment`, actual recorded merged paths, run/overlay/
  upper invisibility, forbidden management-marker creation, lower isolation,
  and drop cleanup are now asserted. If external merged is visible it must
  accept an ordinary COW write into upper only; if hidden it must reject a
  write. Nested merged is always required hidden. No errors are blindly ignored.
- Worker intermediate version passed both workspace-TMPDIR and default-TMPDIR
  environments (six more Sessions). Final stricter visibility branch was
  not run by the worker due to its cap; coordinator supplied that final check.
- Coordinator `env -u TMPDIR -u PVISOR_TEST_ALLOW_NO_USERNS mise run test --
  pvisor-cli -- --test rootless_local -E
  'test(safe_shell_supports_nested_stages_without_exposing_their_backing)'
  --no-capture` passed: one test, 19 skipped, three real Sessions.
  External alias wrote only to upper; nested/nested-home aliases stayed hidden.
  Build 0.57 seconds, tests 0.626 seconds; nextest ID:
  `c7e5cbdf-4a9f-4871-ab42-2d9fff76cab2`. Diff check passed.
- Final coordinator resources before/after: available memory
  18,690,432 / 18,426,500 KiB; load1 0.45 / 0.46; free disk
  351,286,890,496 / 351,294,894,080 bytes.
- No independent product defect in this round. Not covered: concurrent
  namespace/symlink attacks, macOS/OCI/VM or whole-package suites.

### 021 — Malformed runtime control messages

- Worker: `ksQQ3UP44YPgTC-msj8WDYNdyJLOABKxvZIBxBDuNWdurkVNCYkuySUr0iCB`.
- Status: completed; behavior passed and current-source artifact provenance
  confirmed, with a build-output isolation deviation.
- Budget: 30 minutes.
- Assignment: choose an uncovered actual local control-protocol rejection/
  continued-service boundary, at most 16 small messages/two connections,
  preserving valid owner operations and cleanup. Do not infer malicious
  same-UID protection from an internal trusted interface.
- Worker actual local endpoint trace: seven malformed/version/correlation/
  stale-execution cases each followed by healthy live status, then one stalled
  partial line alongside legitimate termination (16 small messages total).
  Malformed terminate requests did not cancel; valid terminate yielded
  cancelled/143, owner/frontend reaped, socket/control state cleared.
- The public endpoint is newline JSON, not a length-prefixed protocol; fake
  length fields do not establish oversized-frame coverage. Maximum actual
  payload 472 bytes, one bounded Host Session and two connections.
- Worker conventional tests: one selected host_cli_service owner test and five
  Core host-protocol tests passed. Earlier wrong Rust 1.90 toolchain rejection
  and 50-second build timeout were environment/limit failures, not passes.
- Evidence caveat: actual probe command names
  `/home/reiase/workspace/pvisor/target/debug/pvisor`, apparently outside the
  isolated worktree. Coordinator requested build/artifact path and hash
  confirmation without additional Session runs.
- Provenance follow-up confirmed current isolated-worktree source compilation
  in historical logs. The new test dep-info explicitly binds CARGO_BIN_EXE to
  the probe path; that binary and its deps artifact share inode 284154552 and
  current SHA-256
  `a34dd0d90381df5f775af98d456ff794ec17185e307f6a53367083bd5298aad8`,
  with timestamps matching the successful 03:23:50 build. There was no hash
  captured at execution time, so this is retrospective artifact evidence.
- Scope deviation: worker explicitly set CARGO_TARGET_DIR to the source
  checkout's target outside the attached worktree, replacing generated build
  outputs there. No source edit is reported, but this was not isolated build
  output. Do not delete or roll back that shared target; future rounds must
  explicitly use their own worktree target. No more Session runs were used
  for provenance verification.
- Worker resource envelope: available memory 17.51–17.84 GiB, load1
  0.39–1.07, free disk 351.245–351.329 GB. No tracked code changes.
- Not covered: real >1 MiB frame, connection saturation, full inactivity timeout,
  guest/private owner wire, cross-UID authorization or full suite.

## CI observation

At 03:20 UTC, `gh pr checks 111` reported two pending benchmark-build checks
for the latest push and no completed pass/fail results. This is not CI success.
Local targeted evidence above remains distinct from GitHub CI.
