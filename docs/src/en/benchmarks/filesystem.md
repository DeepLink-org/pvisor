---
status: todo
search:
  exclude: true
---

# Filesystem overhead

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

How much slower are cargo build and npm install through pVisor?

## Requirements

- Metric: metadata operation latency, read/write throughput, typical task time ratio.
- Control group: native filesystem; Docker bind mount; overlay2.
- Workload: large-repository git status, npm install, cargo build, full-repository ripgrep search.
- Environment: three groups measured separately: Linux FUSE, macFUSE, and FSKit.

## Acceptance criteria

- Report relative overhead percentages and p50/p95/p99.
- Data for each of the three environments.
- A script that reproduces the result in one command.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [methodology](methodology.md), [known limitations](../security/known-limitations.md)
