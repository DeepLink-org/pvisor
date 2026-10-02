---
status: todo
search:
  exclude: true
---

# Concurrency density and resource use

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How many Jobs can one host run concurrently?

## Requirements

- Metrics: concurrent Jobs, per-Job CPU/memory, tail latency.
- Control: Docker at matching density.
- Workload: 1, 8, 32, and 128 Jobs.
- Environment: measure each executor separately.

## Acceptance criteria

- Report host capacity to inform L2/L3 planning.
- Tail-latency data.
- Reproducible resource model.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Methodology](methodology.md), [limitations](../security/known-limitations.md)
