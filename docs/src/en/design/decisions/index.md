---
status: todo
search:
  exclude: true
---

# Architecture decision records

!!! warning "Planned"
    No ADRs exist yet. See [Design principles](../principles.md) for current constraints.

## Question

What context, options, decisions, and consequences led to major choices?

## Requirements

- Create an ADR template (context, options, decision, consequences, status) and a numbering convention (`NNNN-short-title.md`).
- Backfill the existing key decisions, for example:
    - Admission plans stop at `Planned`; enforcement comes only from executor observations.
    - Gateway and replay are separated from the core closure as optional features.
    - `--safe` does not select an executor and refuses to start rather than degrading when it cannot enforce.
    - The human approval process for semantic specifications.

## Acceptance criteria

- At least four numbered ADRs with titles/status.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: Maintainers
- Related: [Principles](../principles.md)
