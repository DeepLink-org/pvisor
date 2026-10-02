---
status: todo
search:
  exclude: true
---

# Concurrency density and resource use

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

How many Jobs can one host run at the same time?

## Requirements

- Metric: concurrent Jobs per host, per-Job CPU and memory cost, tail latency.
- Control group: Docker at the same density.
- Workload: 1, 8, 32, and 128 concurrent Jobs.
- Environment: measured separately for each executor.

## Acceptance criteria

- Report the per-host limit as input for L2/L3 planning.
- Tail-latency data.
- A reproducible resource model.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [methodology](methodology.md), [known limitations](../security/known-limitations.md)
