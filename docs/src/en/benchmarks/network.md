---
status: todo
search:
  exclude: true
---

# Network overhead

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How much overhead does the proxy or VM data plane add?

## Requirements

- Metrics: latency, throughput, connection setup.
- Controls: direct connections and Docker networking.
- Workload: concurrent small requests, large downloads, streamed model responses.
- Paths: host proxy, deny-all, VM smoltcp.

## Acceptance criteria

- Separate results for the three paths.
- Reproducible streaming latency.
- Distinguish cooperative and mandatory boundaries.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Methodology](methodology.md), [limitations](../security/known-limitations.md)
