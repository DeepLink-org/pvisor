---
status: todo
search:
  exclude: true
---

# Journal design

!!! warning "Planned"
    Implementation-owner review is pending. See [Operation and Event](operations-events.md) for event fields and causality.

## Question

How are append order, durability, causality, write failures, and crashes handled?

## Requirements

- Commit/fsync/receipt sequence.
- The meaning of Journal position and `caused_by`, and why `observed_at_unix_ms` is not an ordering basis.
- Deduplication and tail recovery.
- Poisoning and behavior afterward.

## Acceptance criteria

- Code/test references per mechanism.
- Keep field contracts in Operation and Event; mechanisms here.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: `crates/pvisor-journal`, [Operation and Event](operations-events.md)

## Current commit path

Implementation is in `crates/pvisor-journal/src/lib.rs`. File Journals use an exclusive lock and one writer. Log files use 0600 permissions and reject symlinks on open. Shared fields belong in [Operation and Event](operations-events.md).

`Journal::append` validates the Event, computes its digest, acquires the writer lock, checks deduplication/causal cycles, allocates a consecutive position, writes a complete JSON line and calls `sync_all`, updates indexes/notifies subscribers, then returns a receipt. File receipts are `LocalSync`; memory Journals are `Volatile`. Live notifications provide no stronger durability than receipts.

| Condition | Behavior |
| --- | --- |
| Same event ID/content | Return the original position without another append |
| Same ID/different content | `Rejected`; it cannot overwrite an existing event |
| Cycle in the known causal graph | `Rejected`; forward references can remain unresolved temporarily, without replacing references with timestamps |
| Write/sync failure | `Unknown`; poison the handle and reject later appends/history reads until release/reopen for recovery |
| Async waiter canceled | An append already handed to a blocking task can still finish; canceled waiting does not imply no commit |

## Reopen and tail recovery

`scan` validates format version, Events, consecutive positions, unique IDs and causality. Reopening can truncate/sync an unfinished final record lacking a newline. A corrupt complete JSON line, position discontinuity or unsupported header produces an error rather than being skipped.

Journal positions order commits in that Journal, not effects across Jobs. After unknown writes, recover and check stable event IDs; do not repeat remote requests to fill a log gap.

```bash
just test pvisor-journal
```

`write_error_requires_recovery_before_another_receipt` and `cancelling_waiter_does_not_cancel_accepted_append` in the same file cover poisoning/async cancellation. A complete fault matrix and implementation-owner review remain pending.
