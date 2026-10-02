---
status: todo
search:
  exclude: true
---

# Isolation effectiveness tests

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

Are there any known escape paths?

## Requirements

- Metric: escape-case pass rate.
- Control group: across executors.
- Workload: semspec S-STAGE cases plus a public sandbox-escape corpus (symlink replacement, path traversal, Unix sockets, /proc, and similar).
- Environment: report PASS/FAIL/XFAIL per executor for each case.

## Acceptance criteria

- XFAIL cases link to known limitations.
- Publish the corpus and reproducible scripts.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [methodology](methodology.md), [known limitations](../security/known-limitations.md)
