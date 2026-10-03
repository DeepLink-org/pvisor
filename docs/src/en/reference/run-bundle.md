# Run Bundle format

A Run Bundle is the handoff record for a task: what ran, how it ended, which changes remain, and which controls were installed. Reviews, CI, and batch jobs can consume the same record.

Start with `pvisor status --review STAGE` for a summary. Automation can use `--json` and select its reader by `schema_version`. The full file is the `run-bundle.json` path printed with the task output.

## Top-level schema-4 records

| Field | Source and purpose |
| --- | --- |
| `schema_version` / `generated_at_unix_ms` | Format and generation time; current version 4 |
| `run` | Run/Attempt/Session identity, command, terminal state, times, exit/failure, output, metrics, result artifacts |
| `executor_plan` | Optional admission plan; not installation evidence |
| `executor_observations` | Required executor observations; sole enforcement source |
| `safety` | Derived file/network booleans and warnings |
| `filesystem` | Staged target/upper/state, net changes, deletions, samples; may be omitted without staging |
| `network` | Policy, optional interception profile/final counters |
| `environment` | Inheritance and projected/injected names; no variable values |
| `resources` | Requested/effective budgets, mechanisms, limitations |
| `agentctl` | Cooperation snapshot, not an installation receipt |
| `lineage` / `orchestration` | Optional fork ancestry and orchestration metadata |
| `operation` / `run_observation` | Optional effective operation and result/rule/boundary observations |
| `artifacts` | Local references such as capture; contents are not embedded |

Execution terminal states `completed`, `failed`, and `cancelled` in `run.state` differ from the apply lifecycle in `filesystem.state`. Successful execution does not mean applied files.

## Reading and sharing

```bash
pvisor status --review --json ../stage-001 > ../bundle-001.json
jq '{schema_version, run: {id: .run.run_id, state: .run.state}, safety, resources}' ../bundle-001.json
```

Bundles are written with mode `0600`. Readers require exactly schema 4 and reject unknown versions or missing required observation contracts instead of manufacturing evidence. Missing fields do not mean safe.

Arguments, paths, stdout/stderr, and capture may contain secrets. Omitting environment values does not redact the entire Bundle. Inspect it and referenced artifacts before sharing. Types: `crates/pvisor/src/runtime/bundle.rs`; retention: [Jobs](../concepts/jobs.md).
