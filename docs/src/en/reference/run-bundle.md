---
status: todo
search:
  exclude: true
---

# Run Bundle format

!!! warning "Planned"
    The complete reference is pending. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for field meanings and [Run project discovery](cli.md#run-项目发现) for storage layout.

## Question

Which fields does `run-bundle.json` (currently schema version 4) contain? Which are enforcement evidence, which are derived summaries, and how does cross-version compatibility work?

## Requirements

- Generate a JSON Schema from the Bundle type definitions and publish it with each version.
- Describe each top-level field: source (admission plan, executor observations, OverlayFS, OverlayNet, Gateway), whether it can be `null`, and the difference between `null` and zero.
- State the schema version policy: when it is upgraded and whether old Bundles can be read (old Bundles that lack the observation contract are currently rejected).

## Acceptance criteria

- Release schema files; check real output in CI.
- Annotated minimal Bundle example.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [JSON output](json-output.md), [stability](stability.md)

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
