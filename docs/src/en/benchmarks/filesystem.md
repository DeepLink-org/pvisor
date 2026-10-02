---
status: todo
search:
  exclude: true
---

# Filesystem overhead

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How much slower are cargo build and npm install through pVisor?

## Requirements

- Metrics: metadata latency, read/write throughput, task time ratio.
- Controls: native filesystem, Docker bind mount, overlay2.
- Workload: large-repository git status, npm install, cargo build, ripgrep.
- Environments: Linux FUSE, macFUSE, and FSKit measured separately.

## Acceptance criteria

- Relative overhead and p50/p95/p99.
- Data for all three environments.
- One-command reproduction.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Methodology](methodology.md), [limitations](../security/known-limitations.md)
