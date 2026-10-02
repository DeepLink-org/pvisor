---
status: todo
search:
  exclude: true
---

# apply/drop cost and crash consistency

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

How long does apply take for a large changeset, and is the workspace consistent after a crash?

## Requirements

- Metric: apply/drop time as a function of changeset size; conflict-detection cost; crash consistency.
- Control group: `cp -a`; `git apply`.
- Workload: changesets of 10, 1k, and 100k files.
- Environment: `kill -9` in each of the Prepared, TargetApplied, and Committed states.

## Acceptance criteria

- Workspace consistency must be 100% after crash injection, with the recovery path explained.
- Conflict detection has regression coverage.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [methodology](methodology.md), [known limitations](../security/known-limitations.md)
