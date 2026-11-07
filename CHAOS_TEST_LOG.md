# Bounded chaos testing campaign

## Campaign

- Requested duration: eight hours of wall-clock time.
- Started: 2026-11-07 00:26:09 UTC.
- Deadline: 2026-11-07 08:26:09 UTC.
- Initial revision: `47afd887` on `main`.
- PR destination: a new dedicated PR, authorized by the user.
- Branch: `test/bounded-chaos-20261107`.
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
- Status: dispatched; no result yet.
- Budget: 30 minutes.
- Assignment: choose and execute a bounded stage/apply/drop failure scenario,
  using existing contracts to determine expected recovery and ownership
  behavior. Reproduce a defect before making a minimal fix and adding
  regression coverage.
- Checks, findings, fixes, and PR updates: pending worker evidence.
