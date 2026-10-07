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

## Field types and presence {#fields}

`required` means the Rust reader requires the field. `defaulted` means serde accepts its absence; it does not mean a decision should treat absence as an observed empty result. `omitted when empty` marks optional or empty collections that the writer leaves out. The type and field coverage below are checked against the source at docs build time. This is a field reference, not a complete generated JSON Schema for every nested protocol.

<!-- bundle-fields:start -->

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `schema_version` | `u32` | required | Exactly 4 for the current reader |
| `generated_at_unix_ms` | `u64` | required | Bundle generation wall-clock time, Unix milliseconds |
| `run` | `BundleRun` | required | Execution outcome and identity |
| `lineage` | `Option<RunLineage>` | omitted when empty | Parent Run and checkpoint identity, when forked |
| `executor_observations` | `ExecutorObservations` | required | Executor receipt; authoritative installed controls |
| `executor_plan` | `Option<ExecutorPlan>` | omitted when empty | Admission plan, never an enforcement receipt |
| `safety` | `SafetySummary` | required | Booleans derived from receipts and staging state |
| `filesystem` | `Option<FilesystemSummary>` | omitted when empty | Staged filesystem; absent for direct-write tasks |
| `network` | `NetworkSummary` | required | Base policy and available interception evidence |
| `environment` | `crate::runtime::EnvironmentProjection` | defaulted | Variable names only; see nested fields |
| `resources` | `ResourceSummary` | defaulted | Requested/effective limits and implementation notes |
| `agentctl` | `AgentCtlSnapshot` | required | Cooperating clients and directive snapshot |
| `orchestration` | `std::collections::BTreeMap<String, serde_json::Value>` | omitted when empty | Application-specific metadata; absent when empty |
| `operation` | `Option<pvisor_core::operation::Operation>` | omitted when empty | Effective operation contract |
| `run_observation` | `Option<pvisor_core::operation::OperationObservation>` | omitted when empty | Operation outcomes and access observations |
| `artifacts` | `Vec<BundleArtifact>` | defaulted | References to local files, not file contents |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `run.run_id` | `String` | required | Run identity |
| `run.parent_run_id` | `Option<String>` | omitted when empty | Parent identity when supplied |
| `run.task_id` | `Option<String>` | omitted when empty | Task identity when supplied |
| `run.attempt_id` | `String` | required | Attempt identity |
| `run.session_id` | `String` | required | Session identity |
| `run.agent` | `String` | required | Agent label |
| `run.command` | `Vec<String>` | required | Exact argument vector |
| `run.executor` | `Option<ExecutorIdentity>` | omitted when empty | Selected executor identity; not installation evidence |
| `run.state` | `RunState` | required | Execution state; completed, failed, cancelled are terminal |
| `run.started_at_unix_ms` | `u64` | required | Execution start, Unix milliseconds |
| `run.finished_at_unix_ms` | `u64` | required | Execution finish, Unix milliseconds |
| `run.duration_ms` | `u64` | required | Execution duration, milliseconds |
| `run.exit_code` | `Option<i32>` | omitted when empty | Workload exit status when available |
| `run.failure` | `Option<RunFailure>` | omitted when empty | Typed execution failure when available |
| `run.warnings` | `Vec<String>` | defaulted | Execution warnings; default empty |
| `run.output` | `ProcessOutput` | defaulted | Captured stdout/stderr and truncation flags; default empty |
| `run.metrics` | `std::collections::BTreeMap<String, f64>` | defaulted | Named numeric execution metrics; default empty |
| `run.result_artifacts` | `Vec<ArtifactRef>` | defaulted | Executor result references; default empty |
| `run.event_stream_ref` | `Option<String>` | omitted when empty | Trace Event stream reference when supplied |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `safety.safe_profile_requested` | `bool` | required | Requested profile, not a proof of installation |
| `safety.host_process` | `bool` | required | Unisolated host-process identity |
| `safety.filesystem_changes_staged` | `bool` | required | Staged changes currently retained |
| `safety.filesystem_non_bypassable` | `bool` | required | Both read and write dimensions enforced |
| `safety.filesystem_read_non_bypassable` | `bool` | defaulted | Read dimension enforced; serde default false |
| `safety.filesystem_write_non_bypassable` | `bool` | defaulted | Write dimension enforced; serde default false |
| `safety.network_non_bypassable` | `bool` | required | Network dimension enforced |
| `safety.warnings` | `Vec<String>` | defaulted | Boundary limitations; default empty |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `filesystem.state` | `OverlayState` | required | active, staged, applied, discarded; distinct from execution state |
| `filesystem.target` | `PathBuf` | required | Host apply target |
| `filesystem.upper` | `PathBuf` | required | Writable Stage backing path |
| `filesystem.changed_files` | `usize` | required | Changed-path count, not a count of file syscalls |
| `filesystem.whiteouts` | `usize` | required | Deletion/opacity representation count |
| `filesystem.root_overlay` | `bool` | defaulted | Whether the target is root `/`; default false |
| `filesystem.excluded_paths` | `Vec<PathBuf>` | defaulted | Paths excluded from root overlay; default empty |
| `filesystem.access_policy` | `pvisor_core::overlay::FileAccessPolicy` | defaulted | Resolved file policy; default empty |
| `filesystem.host_root_device` | `Option<u64>` | omitted when empty | Original host root device identity for root overlays |
| `filesystem.host_root_inode` | `Option<u64>` | omitted when empty | Original host root inode identity for root overlays |
| `filesystem.host_uid` | `Option<u32>` | omitted when empty | Mirrored host user ID for root overlays |
| `filesystem.host_gid` | `Option<u32>` | omitted when empty | Mirrored host group ID for root overlays |
| `filesystem.sample_paths` | `Vec<String>` | defaulted | Display sample, not the complete changeset; default empty |
| `filesystem.changes` | `Vec<ChangeEntry>` | defaulted | Classified net changes; default empty |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `network.policy` | `serde_json::Value` | required | Serialized base network policy |
| `network.interception` | `Option<InterceptionProfile>` | omitted when empty | Driver/profile identity when supplied |
| `network.intercepted` | `Option<InterceptionSnapshot>` | omitted when empty | Final driver counters when supplied; absent is not zero |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `resources.requested` | `ResourceLimits` | required | Requested ResourceLimits; optional dimensions in bytes/ms/counts |
| `resources.effective` | `ResourceLimits` | required | Effective ResourceLimits supported by observations |
| `resources.mechanisms` | `Vec<String>` | defaulted | Installed resource mechanisms; default empty |
| `resources.limitations` | `Vec<String>` | defaulted | Unenforced or platform-specific limits; default empty |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `artifacts[].kind` | `String` | required | Artifact role, such as run-record |
| `artifacts[].path` | `PathBuf` | required | Local reference; resolve and retain separately |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `environment.inherits_host` | `bool` | defaulted | Whether host environment is inherited; default false |
| `environment.projected_keys` | `Vec<String>` | defaulted | Host variable names passed to task; default empty |
| `environment.runtime_injected_keys` | `Vec<String>` | defaulted | Names supplied by the runtime; default empty |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `lineage.parent_run_id` | `String` | required | Parent Job identity |
| `lineage.checkpoint_id` | `String` | required | Checkpoint used for the fork |

| JSON path | Rust type | Presence | Meaning |
| --- | --- | --- | --- |
| `filesystem.changes[].path` | `String` | required | Display path; byte identity may be supplied separately |
| `filesystem.changes[].path_bytes` | `Option<Vec<u8>>` | omitted when empty | Lossless Unix bytes when the display cannot represent identity |
| `filesystem.changes[].kind` | `ChangeKind` | required | added, modified, deleted, type_changed, opaque |
| `filesystem.changes[].old_type` | `Option<ChangeEntryType>` | omitted when empty | Previous type: file, directory, symlink, other |
| `filesystem.changes[].new_type` | `Option<ChangeEntryType>` | omitted when empty | Current type: file, directory, symlink, other |
| `filesystem.changes[].size_bytes` | `Option<u64>` | omitted when empty | Current size when applicable |
| `filesystem.changes[].mode` | `Option<u32>` | omitted when empty | Unix numeric mode when applicable, e.g. 420 is 0644 |
<!-- bundle-fields:end -->

## Read a change without confusing it with an operation {#changes}

The [real sample](../assets/examples/json/run-bundle.json) contains `obsolete.txt` as `deleted`, `src` as an added directory, and `src/result.txt` as an added file. `filesystem.changes` describes the net staged result; `run_observation.filesystem` describes actual accesses, allowed/denied decisions, and counters. A file written repeatedly can appear once in the changeset. A denied read may appear in observations without producing a change.

`path` is for display. If `path_bytes` exists, use those Unix bytes for path identity; do not authorize a mutation by the display text alone. Directory changes and `opaque` entries have subtree effects and must be considered when selecting files to apply. Use the CLI's selective apply rather than copying raw upper-layer whiteouts into the project.

The saved `run-bundle.json` describes execution at capture time. `review --json` additionally supplies `review_context` and refreshes the selected filesystem view; it does not recreate the executor observations. See [JSON envelopes and samples](json-output.md#envelopes).
