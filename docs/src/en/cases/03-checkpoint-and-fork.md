# 3. Save a proposal and explore a branch

A workspace checkpoint saves proposed files and conflict preimages for refactoring, review, and alternate executions. A fork starts a new command; it does not continue the old process memory. Full VM restore is covered separately in chapter 5.

### S-USE-009: Retry checkpoint creation

Automation may retry a request. Keep --request-id stable and use list/show to inspect the same savepoint.

**Contract**: The same request returns the same checkpoint, with reused=true on retry and one list entry.

**Violation**: A retry creates a duplicate checkpoint or a different ID.

Execution and assertions: [Scrut S-USE-009](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-USE-009.md).

### S-USE-010: Review a savepoint after applying files

Create a checkpoint before accepting a proposal so you can review the saved view later.

**Contract**: After apply, current changes are empty; checkpoint review still lists report.txt and drop refuses the fully applied Session and preserves its checkpoint.

**Violation**: apply or drop rewrites the immutable savepoint.

Execution and assertions: [Scrut S-USE-010](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-USE-010.md).

### S-USE-011: Start two branches from one proposal

Compare implementations by forking twice from one checkpoint. Keep the source proposal and both child Sessions separate.

**Contract**: Both branches read seed and independently write branch-a/branch-b; the source retains seed and the host has no report.txt. Deleting the referenced checkpoint fails.

**Violation**: Branches contaminate each other, edit the source, or delete a referenced savepoint.

Execution and assertions: [Scrut S-USE-011](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-USE-011.md).

[Learning path](index.md) · [4. Set boundaries for untrusted work](04-boundaries.md)
