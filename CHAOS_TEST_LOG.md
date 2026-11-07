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
