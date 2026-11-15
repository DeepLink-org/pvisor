# 2. Review and accept file changes

For an Agent that edits code, add `--stage`, review with `review` and `inspect`, then choose `apply` or `drop`. Staging covers workspace changes; host paths remain accessible. Use chapter 4 for the `--safe` access boundary.

### S-USE-005: Read the Agent file view

Before accepting an edited report, compare staged content with the original. `inspect` opens a read-only view.

**Contract**: The original remains original, inspect reads proposed, and review lists the report change.

**Violation**: The edit reaches the original or review omits it.

Execution and assertions: [Scrut S-USE-005](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-005.md).

### S-USE-006: Accept selected files

When an Agent edits a report and an experiment, accept only the report and discard the remainder.

**Contract**: apply --path commits only report.txt; drop discards scratch.txt and preserves the Session record.

**Violation**: Selective apply commits everything or drop removes the Session record.

Execution and assertions: [Scrut S-USE-006](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-006.md).

### S-USE-007: Refuse an apply conflict

If you edit the same file during review, apply must protect your new content and retain the proposal.

**Contract**: An apply preimage conflict fails; the host retains human and the stage retains agent.

**Violation**: The old proposal overwrites the human edit or disappears on failure.

Execution and assertions: [Scrut S-USE-007](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-007.md).

### S-USE-008: Stop a Session before deciding

For a stuck task, kill it, confirm it has stopped, then discard files. Do not apply a live Session.

**Contract**: A live Session refuses apply; after kill it is cancelled and no longer live, with no workspace edit.

**Violation**: Live apply succeeds, the task remains alive after kill, or writes escape staging.

Execution and assertions: [Scrut S-USE-008](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-008.md).

[Learning path](index.md) · [3. Save a proposal and explore a branch](03-checkpoint-and-fork.md)
