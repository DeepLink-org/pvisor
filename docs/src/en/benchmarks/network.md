---
status: todo
search:
  exclude: true
---

# Network overhead

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

How much slower are requests that pass through the pVisor proxy or the VM data plane?

## Requirements

- Metric: request latency, throughput, connection setup time.
- Control group: direct connections; Docker networking.
- Workload: many concurrent small requests; large file downloads; LLM streaming responses.
- Environment: covers the host proxy, deny-all, and VM smoltcp.

## Acceptance criteria

- Data for each of the three paths.
- Reproducible streaming-response latency.
- State the difference between cooperative and mandatory boundaries.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [methodology](methodology.md), [known limitations](../security/known-limitations.md)
