# 2. Review and accept file changes

For an Agent that edits code, add `--stage`, review with `review` and `inspect`, then choose `apply` or `drop`. Staging covers workspace changes; host paths remain accessible. Use chapter 4 for the `--safe` access boundary.

### S-USE-005: Read the Agent file view

Before accepting an edited report, compare staged content with the original. `inspect` opens a read-only view.

**Contract**: The original remains original, inspect reads proposed, and review lists the report change.

**Violation**: The edit reaches the original or review omits it.

```bash
journey_setup
printf original > report.txt
pvisor run --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf proposed > report.txt'
assert_content report.txt original
pvisor inspect "$CASE_ROOT/draft" -- /bin/cat report.txt > "$CASE_ROOT/view.txt"
assert_content "$CASE_ROOT/view.txt" proposed
pvisor review "$CASE_ROOT/draft" --diff > "$CASE_ROOT/review.txt"
journey_contains "$CASE_ROOT/review.txt" report.txt
pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
json_paths "$CASE_ROOT/review.json" report.txt
```

### S-USE-006: Accept selected files

When an Agent edits a report and an experiment, accept only the report and discard the remainder.

**Contract**: apply --path commits only report.txt; drop discards scratch.txt and preserves the Job record.

**Violation**: Selective apply commits everything or drop removes the Job record.

```bash
journey_setup
pvisor run --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf accepted > report.txt; printf scratch > scratch.txt'
pvisor apply "$CASE_ROOT/draft" --path report.txt
assert_content report.txt accepted
assert_absent scratch.txt
pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
json_paths "$CASE_ROOT/review.json" scratch.txt
pvisor drop "$CASE_ROOT/draft"
assert_absent scratch.txt
pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
json_paths "$CASE_ROOT/review.json"
```

### S-USE-007: Refuse an apply conflict

If you edit the same file during review, apply must protect your new content and retain the proposal.

**Contract**: An apply preimage conflict fails; the host retains human and the stage retains agent.

**Violation**: The old proposal overwrites the human edit or disappears on failure.

```bash
journey_setup
printf base > report.txt
pvisor run --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf agent > report.txt'
printf human > report.txt
expect_refused pvisor apply "$CASE_ROOT/draft" --all
assert_content report.txt human
pvisor inspect "$CASE_ROOT/draft" -- /bin/cat report.txt > "$CASE_ROOT/view.txt"
assert_content "$CASE_ROOT/view.txt" agent
```

### S-USE-008: Stop a Job before deciding

For a stuck task, kill it, confirm it has stopped, then discard files. Do not apply a live Job.

**Contract**: A live Job refuses apply; after kill it is cancelled and no longer live, with no workspace edit.

**Violation**: Live apply succeeds, the task remains alive after kill, or writes escape staging.

```bash
journey_setup
pvisor run --stage "$CASE_ROOT/live" -- /bin/sh -c 'printf ready > ready.txt; exec /bin/sleep 30' > "$CASE_ROOT/live.log" 2>&1 &
runner=$!
trap 'kill "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true' EXIT
journey_wait_file "$CASE_ROOT/live/upper/ready.txt" "$runner"
expect_refused pvisor apply "$CASE_ROOT/live" --all
pvisor kill "$CASE_ROOT/live"
expect_refused wait "$runner"
trap - EXIT
pvisor status "$CASE_ROOT/live" --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /run/state '"cancelled"'
json_expect "$CASE_ROOT/status.json" /live false
pvisor drop "$CASE_ROOT/live"
assert_absent ready.txt
```

[Learning path](index.md) · [3. Save a proposal and explore a branch](03-checkpoint-and-fork.md)
