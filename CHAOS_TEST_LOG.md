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
