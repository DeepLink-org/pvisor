# Machine-readable output

Use JSON to connect tasks to scripts: check the outcome, inspect the actual boundaries, then choose which files to accept. The queries below provide a starting point for CI decisions.

`status --json` is useful for task status; `status --review --json` returns the full review Run Bundle. Use the latter for admission decisions and check the [evidence fields](run-bundle.md).

## Choose the right output

| Command | Object | Use |
| --- | --- | --- |
| `status --json` | Status and optional filesystem/network summaries | Liveness/stage overview; not complete control evidence |
| `status --review --json` | Complete versioned Run Bundle | Post-run review and machine consumers |
| `review --json` | Schema-4 Bundle plus `review_context` | Same review path; `--checkpoint ID` selects a saved workspace |
| `kill --json` | Schema 1, operation `kill` | Distinguish a termination request from an already stopped Job |
| `checkpoint create/list/show/delete/gc --json` | Schema 1, operation `checkpoint.*` | Job-scoped workspace and execution checkpoint management |

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

Safety decisions must check schema, required fields, execution state, observations, and warnings together. Do not turn unknown evidence into acceptance with `// false` or empty-array defaults. Use the version rules in [Stability](stability.md) when upgrading readers.

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
  and all(.filesystem.changes[]; .path_bytes == null and (.path == "src" or (.path | startswith("src/"))))
' ../review-001.json
```

Select conditions for your task. A cooperative proxy task will not satisfy mandatory networking; choose the appropriate execution path when that boundary is required. After this check, run project tests and select files to apply. The query only evaluates the record fields shown above.

Stop reading an unrecognized `schema_version`, and stop a decision when required fields are missing. A network statistic of `null` means no observation was supplied; `0` means a recorded zero count. A display can show missing changes as empty, while admission checks should validate the field type as in this example.

## JSON versions and command envelopes {#envelopes}

The schema number belongs to the output format, not the CLI as a whole. `status --json` has no top-level schema version and is an overview. Do not look for `schema_version = 4` there. Its fields are:

| Field | Meaning |
| --- | --- |
| `run` | Stored RunRecord; includes its lifecycle and storage references |
| `live` | Boolean liveness observation |
| `checkpoint_capability` | `workspace`, `workspace_capture_requires`, `execution`, `execution_blocker` |
| `checkpoints` | Array of workspace checkpoint records |
| `execution` | Native Job state, suspended head, current Attempt, checkpoints, requests and store ownership; null without native handoff |
| `workspace_generation` | Integer, or null without an overlay |
| `apply_history` | Previous apply transaction records |
| `observations.filesystem/network` | Available access/traffic observations, or null |
| `filesystem` | State, changed_files, whiteouts, sample_paths; null without staging |

Review outputs add `review_context` to the Bundle: `job_id`, `attempt_id`, nullable `checkpoint_id`, `workspace_generation`, `file_view`, and `execution_evidence`. Execution evidence remains historical; the selected filesystem view is refreshed at review time, including after apply/drop. Reviewing a checkpoint selects its saved files without rerunning its workload.

All checkpoint success envelopes include `schema_version = 1`, `operation`, and `job_id`:

| Operation | Additional fields |
| --- | --- |
| `checkpoint.create` | `request_id` (nullable), `checkpoint_id`, `kind`, `reused` (workspace only), `checkpoint` |
| `checkpoint.list` | `kind_filter` (nullable), `execution_blocker`, `checkpoints` |
| `checkpoint.show` | `kind`, `branch_references`, `checkpoint` |
| `checkpoint.delete` | `checkpoint_id`, `deleted = true` |
| `checkpoint.gc` | `scope`, `root`, `removed_transactions`, `execution_removed_transactions`, `published_checkpoints_deleted = 0` |

`kill --json` returns `already_stopped = true` with the stored state for a stopped Job. Otherwise it returns `state = "stopping"`, `termination_requested = true`; this confirms the request, not completed shutdown. Poll status for the resulting state.

Companion commands such as replay own their output formats. Ordinary `run`, `resume`, `fork`, `apply`, and `drop` do not emit a JSON success envelope; read status/the Bundle and preserve exit status. `suspend --json` succeeds only after checkpoint publication and native termination: it returns `schema_version = 1`, `operation = "suspend"`, `job_id`, `state = "suspended"`, `request_id`, `checkpoint_id`, `kind = "execution"`, and `checkpoint`. Unsupported profiles report errors on stderr without a checkpoint success object. `pvisor --help` lists the commands available in the current installation.
