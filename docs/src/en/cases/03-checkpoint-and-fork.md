# 3. Save a proposal and explore a branch

A workspace checkpoint saves proposed files and conflict preimages for refactoring, review, and alternate attempts. A fork starts a new command; it does not continue the old process memory. Full VM restore is covered separately in chapter 5.

### S-USE-009: Retry checkpoint creation

Automation may retry a request. Keep --request-id stable and use list/show to inspect the same savepoint.

**Contract**: The same request returns the same checkpoint, with reused=true on retry and one list entry.

**Violation**: A retry creates a duplicate checkpoint or a different ID.

```bash
journey_setup
pvisor run --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf proposal > report.txt'
pvisor checkpoint create "$CASE_ROOT/source" --request-id before-refactor --json > "$CASE_ROOT/first.json"
pvisor checkpoint create "$CASE_ROOT/source" --request-id before-refactor --json > "$CASE_ROOT/retry.json"
checkpoint=$(journey_json "$CASE_ROOT/first.json" /checkpoint_id)
json_expect "$CASE_ROOT/retry.json" /checkpoint_id "\"$checkpoint\""
json_expect "$CASE_ROOT/retry.json" /reused true
pvisor checkpoint list "$CASE_ROOT/source" --json > "$CASE_ROOT/list.json"
json_expect "$CASE_ROOT/list.json" /checkpoints/0/checkpoint_id "\"$checkpoint\""
json_length "$CASE_ROOT/list.json" /checkpoints 1
pvisor checkpoint show "$CASE_ROOT/source" "$checkpoint" --json > "$CASE_ROOT/show.json"
json_expect "$CASE_ROOT/show.json" /branch_references 0
```

### S-USE-010: Review a savepoint after applying files

Create a checkpoint before accepting a proposal so you can review the saved view later.

**Contract**: After apply, current changes are empty; checkpoint review still lists report.txt and drop refuses the fully applied Job and preserves its checkpoint.

**Violation**: apply or drop rewrites the immutable savepoint.

```bash
journey_setup
pvisor run --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf proposal > report.txt'
pvisor checkpoint create "$CASE_ROOT/source" --json > "$CASE_ROOT/checkpoint.json"
checkpoint=$(journey_json "$CASE_ROOT/checkpoint.json" /checkpoint_id)
pvisor apply "$CASE_ROOT/source" --all
assert_content report.txt proposal
pvisor review "$CASE_ROOT/source" --json > "$CASE_ROOT/current.json"
json_paths "$CASE_ROOT/current.json"
pvisor review "$CASE_ROOT/source" --checkpoint "$checkpoint" --json > "$CASE_ROOT/saved.json"
json_paths "$CASE_ROOT/saved.json" report.txt
expect_refused pvisor drop "$CASE_ROOT/source"
assert_content report.txt proposal
pvisor checkpoint show "$CASE_ROOT/source" "$checkpoint" --json > "$CASE_ROOT/show.json"
```

### S-USE-011: Start two branches from one proposal

Compare implementations by forking twice from one checkpoint. Keep the source proposal and both child Jobs separate.

**Contract**: Both branches read seed and independently write branch-a/branch-b; the source retains seed and the host has no report.txt. Deleting the referenced checkpoint fails.

**Violation**: Branches contaminate each other, edit the source, or delete a referenced savepoint.

```bash
journey_setup
pvisor run --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf seed > report.txt'
pvisor checkpoint create "$CASE_ROOT/source" --json > "$CASE_ROOT/checkpoint.json"
checkpoint=$(journey_json "$CASE_ROOT/checkpoint.json" /checkpoint_id)
pvisor fork "$CASE_ROOT/source" --checkpoint "$checkpoint" --stage "$CASE_ROOT/branch-a" -- /bin/sh -c 'test "$(cat report.txt)" = seed; printf branch-a > report.txt'
pvisor fork "$CASE_ROOT/source" --checkpoint "$checkpoint" --stage "$CASE_ROOT/branch-b" -- /bin/sh -c 'test "$(cat report.txt)" = seed; printf branch-b > report.txt'
for branch in source branch-a branch-b; do
  pvisor inspect "$CASE_ROOT/$branch" -- /bin/cat report.txt > "$CASE_ROOT/$branch.txt"
done
assert_content "$CASE_ROOT/source.txt" seed
assert_content "$CASE_ROOT/branch-a.txt" branch-a
assert_content "$CASE_ROOT/branch-b.txt" branch-b
assert_absent report.txt
pvisor checkpoint show "$CASE_ROOT/source" "$checkpoint" --json > "$CASE_ROOT/show.json"
json_expect "$CASE_ROOT/show.json" /branch_references 2
expect_refused pvisor checkpoint delete "$CASE_ROOT/source" "$checkpoint"
```

[Learning path](index.md) · [4. Set boundaries for untrusted work](04-boundaries.md)
