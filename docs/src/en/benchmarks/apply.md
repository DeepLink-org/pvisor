---
status: todo
search:
  exclude: true
---

# apply/drop cost and crash consistency

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How long does apply take for large changesets, and what state remains after a crash?

## Requirements

- Metrics: apply/drop time by size, conflict-check cost, crash consistency.
- Controls: `cp -a`, `git apply`.
- Workload: 10, 1k, and 100k changed files.
- Environment: inject `kill -9` at Prepared, TargetApplied, and Committed.

## Acceptance criteria

- 100% consistency in the tested crash corpus, with recovery paths explained.
- Conflict regression checks.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Methodology](methodology.md), [limitations](../security/known-limitations.md)
