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
