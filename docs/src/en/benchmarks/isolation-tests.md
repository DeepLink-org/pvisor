---
status: todo
search:
  exclude: true
---

# Isolation effectiveness tests

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

Are there known escape paths?

## Requirements

- Metric: escape-corpus pass rate.
- Controls: compare executors.
- Workload: S-STAGE semspec plus public escape cases such as symlink replacement, traversal, Unix sockets, and /proc.
- Report PASS/FAIL/XFAIL per executor.

## Acceptance criteria

- Link XFAIL to limitations.
- Publish corpus and reproducible scripts.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Methodology](methodology.md), [limitations](../security/known-limitations.md)
