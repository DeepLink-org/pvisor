# Machine-readable output

Use JSON to connect tasks to scripts: check the outcome, inspect the actual boundaries, then choose which files to accept. The queries below provide a starting point for CI decisions.

`status --json` is useful for task status; `status --review --json` returns the full review Run Bundle. Use the latter for admission decisions and check the [evidence fields](run-bundle.md).

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

## A scriptable check {#gate}

If a task requires successful completion, enforced file and network controls, and changes confined to `src/`, use this check. It requires `jq`; zero means the requirements match and nonzero means the check failed.

```bash
jq -e '
  .schema_version == 4
  and .run.state == "completed"
  and .run.exit_code == 0
  and .safety.filesystem_read_non_bypassable == true
  and .safety.filesystem_write_non_bypassable == true
  and .safety.network_non_bypassable == true
  and (.filesystem.changes | type == "array")
  and all(.filesystem.changes[]; .path == "src" or (.path | startswith("src/")))
' ../review-001.json
```

Select conditions for your task. A cooperative proxy task will not satisfy mandatory networking; choose the appropriate execution path when that boundary is required. After this check, run project tests and select files to apply. The query only evaluates the record fields shown above.

Stop reading an unrecognized `schema_version`, and stop a decision when required fields are missing. A network statistic of `null` means no observation was supplied; `0` means a recorded zero count. A display can show missing changes as empty, while admission checks should validate the field type as in this example.
