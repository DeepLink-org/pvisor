---
status: todo
search:
  exclude: true
---

# End-to-end agent task overhead

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

How much do the success rate and time change when a real agent task runs under pVisor?

## Requirements

- Metric: wall-clock time, token usage, task success rate.
- Control group: the same agent without pVisor.
- Workload: a SWE-bench Lite subset (or a self-built task set), one group each for Claude Code and Codex.
- Environment: pinned model and tool versions.

## Acceptance criteria

- Success-rate difference within statistical error.
- Report the overhead percentage.
- Publish the task set and configuration.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [methodology](methodology.md), [known limitations](../security/known-limitations.md)
