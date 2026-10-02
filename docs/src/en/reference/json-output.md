---
status: todo
search:
  exclude: true
---

# Machine-readable output

!!! warning "Planned"
    The complete reference is pending. See [Network policy](../guides/policies/network.md) for examples of `status --review --json`.

## Question

What do status JSON commands emit and which fields can scripts depend on?

## Requirements

- Publish a JSON Schema for each command that supports `--json`.
- Mark stable/experimental fields.
- Provide common `jq` queries: whether any access was denied, whether the network boundary is non-bypassable, and whether changes stay within the given paths.

## Acceptance criteria

- Generate schemas and check output against them in CI.
- Fix the known issue where `status --json` `sample_paths` leaks internal `.wh.d` paths.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Run Bundle](run-bundle.md)

## Choose the right output

| Command | Object | Use |
| --- | --- | --- |
| `status --json` | Status and optional filesystem/network summaries | Liveness/stage overview; not complete control evidence |
| `status --review --json` | Complete versioned Run Bundle | Post-run review and machine consumers |
| `extensions` | Installed companion commands as a JSON array | Check TUI/replay/cache availability |

`--review --json` and `--diff` are mutually exclusive. Do not parse human review text.

## Useful queries

```bash
pvisor status --json ../stage-001
pvisor status --review --json ../stage-001 > ../review-001.json
jq '.safety.network_non_bypassable' ../review-001.json
jq '.filesystem.changes // [] | map({path, kind})' ../review-001.json
jq '.network.intercepted // null' ../review-001.json
jq '[.filesystem.changes[]? | select(.path != "src" and (.path | startswith("src/") | not))]' ../review-001.json
```

The last query lists changes outside `src`. An empty array only establishes scope of the retained changeset, not absence of external effects.

Network counters include `requests_seen`, `policy_allowed`, `policy_denied`, `failures`, `tcp_flows_opened`, `tcp_flows_denied`, and `targets`. Missing `intercepted` means unavailable observations, not zero traffic. File denials are in `run_observation.filesystem`; net changes are in `filesystem.changes`.

Safety decisions must check schema, required fields, execution state, observations, and warnings together. Do not turn unknown evidence into acceptance with `// false` or empty-array defaults. Stability labels and full generated schemas remain pending above.
