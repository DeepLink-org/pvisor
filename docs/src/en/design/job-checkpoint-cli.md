# Job-centered checkpoint and fork command design

> CLI update: the standalone `pvisor snapshot` entry is removed. Old interfaces/measurements below belong to their historical artifacts, not current executable instructions. See [CLI reference](../reference/cli.md) for current entries and capability boundaries.


> Status: native execution capture, suspension, continuation and branches are integrated with ordinary Jobs; section 10 defines support and acceptance.
> Constraint: preserve ordinary run configuration and default execution behavior; actual executor/profile determines capability.

## 1. Objects and basic constraints

**Job is the sole running object.** A Job retains task identity, policy, workspace targets, run history and parent/child relationships. `stage` is the persistent directory where users manage that Job; file staging and execution checkpoints within it have different lifecycles.

| Object | Identity and purpose | Mutability |
| --- | --- | --- |
| Job | JobId; operated on by run/status/kill/suspend/resume/fork | Lifecycle, current Attempt and working view are mutable |
| Attempt | AttemptId; one runner lifecycle | A new runner must obtain a new AttemptId and execution-ownership generation |
| workspace | Working view, staged changes, preimages and acceptance records | A new generation is produced when the Job runs or apply/drop executes |
| checkpoint | CheckpointId; saves consistent files or full execution state | Manifest and content are immutable; incremental parent references are permitted |
| Content object | Files or RAM blocks stored by content identity | Immutable; Job, checkpoint and restore references pin it |

Checkpoint kinds:

- `workspace`: file version, staged changes, preimages and associated configuration; starts a new command without continuing CPU/RAM state.
- `execution`: CPU, RAM, devices, file version, working-view ownership and compatibility; continues saved processes.

Compression, deduplication and full/incremental capture are checkpoint storage mechanisms; they do not create a separate set of Jobs.

## 2. Command tree

```text
pvisor
├── run                    Create and run a Job
├── status                 Inspect Job state, capabilities and retained objects
├── review                 Review changes bound to a file version
├── inspect                Inspect a read-only Job / checkpoint file view
├── apply                  Accept selected file changes from a stopped Job
├── drop                   Discard pending file changes from a stopped Job
├── kill                   Terminate a Job without creating a checkpoint
├── suspend                Save an execution checkpoint, exit runner, retain Job
├── resume                 Continue the same Job from its current suspended checkpoint
├── fork                   Create a new Job from file or execution state
└── checkpoint
│   ├── create             Create a consistent checkpoint for a Job
│   ├── list               List checkpoints within a Job
│   ├── show               Inspect kind, source, dependencies, compatibility and references
│   ├── delete             Delete checkpoints without retained references
│   └── gc                 Collect unreferenced content and leftover staging in the store
```

`tui/replay/cache/memory-pool` remain tools; they do not create product instance identities parallel to Jobs. The default execution shorthand `pvisor -- COMMAND` remains equivalent to `pvisor run -- COMMAND`.

The target public workflow does not add `job run`, `vm run` or `snapshot run`.

## 3. Selecting Jobs and storage

The unified `JOB` argument accepts a JobId, stage directory, existing run.json / internal Job path, or `last`. It retains the existing resolver; multiple candidates are refused rather than silently selected.

- `last` resolves once within the current workspace and selected storage index; it does not automatically search for the most recent suspendable or stopped Job.
- `--name` is display information, not a substitute for stable identity; multiple Jobs with the same name do not form a selection rule.
- `status/review/inspect` may omit JOB, equivalent to `last`.
- `apply/drop/kill/suspend/resume/fork` require an explicit JOB; writing `last` counts as explicit selection.
- `--output-dir/-o` continues to control the existing Job lookup root; explicit stage paths do not depend on another index. It does not export runtime content into a new directory.
- `CHECKPOINT_ID` resolves within the selected Job. Unique digest prefixes are accepted; ambiguity or mismatched ownership is refused. This version introduces no `latest` checkpoint alias; historical selection must be explicit.
- `resume` uses only the suspended head durably recorded by the Job, accepting neither `latest` nor arbitrary old checkpoints. Running from an old state requires a fork.

Logical layout; existing upper-level file layouts may map compatibly without a single migration:

```text
stage/job-a/
├── job.json
├── attempts/<attempt-id>/
├── workspace/             # writable state, preimages, acceptance records
└── checkpoints/
    ├── index.json
    └── <checkpoint-id>/   # manifest, CPU/device state, content references

<per-user-content-store>/  # not a separate VM instance manager
└── content/<content-id>
```

Checkpoint manifests, ownership and references live within the stage; shared content may live in a common content store. Configuration selects the store; common commands do not require an additional `--store`. Moving a stage across filesystems requires explicit export/import tools; simply moving a directory cannot be promised to preserve hard-link references.

`drop` does not delete Job records, Attempt history or checkpoints. Checkpoint deletion does not modify the current workspace. Job deletion and archiving are not added to this CLI version; `drop` must not imply either action.

## 4. Command syntax and behavior

### 4.1 run: the sole startup entry point

```sh
pvisor run [现有运行、策略、记录与资源选项] \
  [--stage PATH] [--name NAME] -- COMMAND...
```

Keep existing `--vm/--executor`, `--rootfs`, `--safe`, `--mount`, networking and Gateway options and configuration precedence.

**No new mandatory startup flags; no requirement for `--checkpointable`.** Save capabilities derive from the selected executor, rootfs, filesystem, devices and network configuration and are recorded in Job metadata and status. Users still start Jobs as before, requesting suspend, checkpoint or execution fork when needed.

Preserve run argument parsing, default executor, command and environment, CPU/memory, stdio, TUI, policy installation, stage selection and result acceptance semantics as far as possible. Potential future saving must not silently disable DAX, networking or devices, change the rootfs view, enable compression/paging by default or fully copy all inputs.

The initial support matrix remains limited to validated configurations. Declare capabilities only after full snapshots, Run/Attempt/Bundle, stage, file versions and preimage ownership are connected. The historical standalone snapshot runner does not establish that ordinary Jobs are connected.

Save requests use two checks: record static capabilities at startup, then check dynamic conditions at operation time (active connections, file handles, in-flight device access, compatibility, space and execution ownership). If conditions are unmet, return specific reasons while keeping the original Job running. Do not restart the workload to retrofit capabilities or silently reduce isolation policy.

If implementation requires optional configuration, reuse existing executor configuration first and discuss its necessity separately; do not add another run option group merely to integrate snapshots. `--ram-storage raw|compressed` belongs on commands that actually capture state, not on run.

### 4.2 status and review

```sh
pvisor status [JOB] [--json]
pvisor status --review [JOB] [--diff]       # 保留兼容入口
pvisor review [JOB] [--checkpoint ID] [--diff] [--json]
```

Status must show at least JobId, AttemptId, executor, workspace/stage, execution state, workspace generation, pending acceptance state, suspended head, checkpoint capabilities and blocking reasons, and source Job/checkpoint.

Show lifecycle and file state separately, for example `suspended` plus `pending changes`; an absent PID must not be displayed as completion.

Review output must bind to JobId, workspace version / checkpoint ID, change-set digest, target and preimages. Stopped/suspended Jobs read a stable view. If a running Job can obtain a consistent file version, freeze and capture a workspace checkpoint, continue running, then generate the diff; otherwise explicitly refuse. `--checkpoint` views historical file versions without granting the ability to apply them directly to the current Job.

Keep existing diff size limits. Later changes invalidate an old review's correspondence to the current workspace; apply must recheck the version, selected set and host preimages.

### 4.3 inspect

```sh
pvisor inspect [JOB] [--checkpoint ID] -- COMMAND...
```

Inspect the current stable file view or selected checkpoint read-only. This is not guest exec and does not execute guest CPU state. Mutable views of running Jobs use the same consistency threshold as review.

### 4.4 apply and drop

```sh
pvisor apply JOB [--path RELATIVE_PATH ...] \
  [--include GLOB ...] [--exclude GLOB ...] [--all] [--target PATH]
pvisor drop JOB
```

Keep selective apply, original-state/conflict checks, target selection and existing acceptance-record contracts. Do not apply automatically or change approval or acceptance identity because of fork/suspend.

Both commands permit only confirmed stopped Jobs. Running, transitional, suspended and unknown execution-ownership states are refused. A suspended Job must first use `kill JOB` to relinquish continuation within the original Job before its working view can be processed.

Success creates a new working-view generation; retained immutable checkpoints are not rewritten. Execution restore from an old checkpoint can only create a new Job with an independent working view, without rolling back the existing Job's acceptance history.

Branches inherit preimages from the fork point. If two branches apply to the same target in sequence, the second must check whether the host has changed; a shared source does not waive conflict checks.

### 4.5 suspend

```sh
pvisor suspend JOB [--ram-storage raw|compressed] \
  [--timeout DURATION] [--request-id ID] [--json]
```

Operate only on running Jobs. Freeze CPU and device writes, save an execution checkpoint and file version at the same freeze boundary; after publication, confirm the source runner has exited, commit the suspended head, and output JobId, CheckpointId and state.

`--ram-storage` controls persistent RAM encoding for this capture: raw by default, compressed for existing block compression/deduplication. It does not change the online pager or guarantee incremental capture. No empty `--incremental` / `--lazy` options are provided.

`--timeout` budgets waiting and freezing; timeout is neither a force kill nor proof that a worker has ended. If cleanup is still in progress, return a durable operation ID and refuse concurrent restore. A client exit must not start another runner while the checkpoint is published but the source may still be running.

Repeated suspension of an already suspended Job returns the existing head without creating another checkpoint. Workspace-only Jobs or Jobs without valid execution capture explicitly return a capability error.

### 4.6 resume

```sh
pvisor resume JOB [--tui] [--request-id ID]
```

Continue the same Job only from its suspended head. Retain JobId, create a new AttemptId and execution-ownership generation, validate compatibility and complete references, create a writable working view, and execute after installing state.

Do not accept a new command, rootfs, CPU/memory or policy overrides. Configuration changes use workspace fork or a new run; the first version does not offer unreliable execution-state parameter changes.

The command holds the terminal in the foreground, like run. `--tui` changes host presentation only; it cannot pretend to restore old host TTYs/pipes or external connections. Executor profiles must declare permitted console endpoint reconstruction.

On failure before execution starts, retain the suspended head for retry. Failure after execution starts must not automatically mark the head safe for retry without side effects. Record the new Attempt and actual state; returning to the old point requires explicit fork.

### 4.7 fork

```sh
pvisor fork JOB [--state workspace|execution] [--checkpoint ID] \
  [--stage NEW_PATH] [--name NAME] [--request-id ID] \
  [--ram-storage raw|compressed] [--tui] [-- COMMAND...]
```

Default to `--state workspace`, retaining existing file fork behavior. Execution must be selected explicitly; finding a VM checkpoint must not switch modes automatically.

| Condition | workspace fork | execution fork |
| --- | --- | --- |
| running, no checkpoint selected | Obtain a consistent file save point; refuse without freeze capability | Freeze, publish a complete save point, establish child Job and references, then unfreeze parent |
| suspended, no checkpoint selected | Derive from suspended file version and launch command | Reference suspended head and create child Job without changing parent |
| stopped, no checkpoint selected | Capture and derive from stable file view | Refuse: no live CPU state; do not automatically select a historical point |
| Selected checkpoint | Must be workspace or explicitly extract an execution file view; first version only accepts matching kinds | Must be execution with matching source and capabilities |
| Replacement command | Allowed; defaults to saved command configuration | Refused; continues saved processes |

Selecting an immutable checkpoint does not freeze a running parent or read its current mutable files; only ownership, configuration and retained references are checked. Omit `--checkpoint` to capture the parent's current state.

`--ram-storage` applies only to newly captured execution save points; refuse it for workspace mode or reuse of existing checkpoints. `--stage NEW_PATH` must be empty or nonexistent; never overwrite an old Job. If omitted, retain existing persistent Job storage selection rules and generate a new directory.

The child uses a new JobId and AttemptId, independent execution ownership and independently writable workspace/RAM. Its manifest records parent JobId, source checkpoint ID and state kind. Guest PID/boot ID may retain their saved values; they are not global Job identities. Future network and management identities require independent rebinding contracts.

Copy or COW-clone working state and inherit immutable content references. Do not recursively copy the parent's execution locks, sockets, temporary writes, terminal state, acceptance authorization or checkpoint index. Checkpoint creation history belongs to the parent; the child holds a source reference without claiming ownership of that history.

Freeze the parent only through durable save-point commit and child branch reference/identity creation. The child runner may start after the parent unfreezes; child startup failure must not leave the parent paused indefinitely. Failed child Job records remain queryable and reclaimable; request IDs prevent retries from creating multiple children for the transaction.

### 4.8 checkpoint management

```sh
pvisor checkpoint create JOB [--kind workspace|execution] \
  [--ram-storage raw|compressed] [--timeout DURATION] \
  [--request-id ID] [--json]
pvisor checkpoint list JOB [--kind workspace|execution] [--json]
pvisor checkpoint show JOB CHECKPOINT_ID [--json]
pvisor checkpoint delete JOB CHECKPOINT_ID [--json]
pvisor checkpoint gc JOB [--json]
```

Create defaults to workspace. Running execution create freezes, publishes an independent object, then unfreezes the parent; **it does not suspend the Job**. Its capture transaction is shared with execution fork. Stopped Jobs cannot create execution checkpoints. Suspended Jobs may reference an existing execution head without starting the guest; returning an existing object reports `reused=true`.

Create does not offer checkpoint overwrite. Content may be full, compressed, deduplicated or later incremental; the CLI always outputs immutable identity and actual format.

Show reports kind, source Job/Attempt, file version, compatibility, incremental parent dependencies, logical/encoded bytes and references blocking deletion. Logical/encoded bytes are not physical residency or density metrics.

Suspended heads, child Job sources, incremental descendants, active restores and staged writes protect referenced checkpoints from deletion. The first version refuses deletion when these references exist, with no `--force` or implicit cascade. Future detach/compaction must also create independent objects first and atomically replace references.

GC locates the content store through JOB. It may reclaim any unreferenced content and leftover staging within that store, not merely scan one Job. JSON output explicitly reports scope and counts by category. Active writes, restores, retained checkpoints and branch dependencies must not be reclaimed.

### 4.9 kill

```sh
pvisor kill JOB [--json]
```

Running: retain the existing graceful termination request; keep stopping/unknown until exit is confirmed, without prematurely allowing apply/drop.

Suspended: there is no runner to kill. Explicitly end the Job's right to continue, release the suspended head's automatic pin, transition to stopped and record termination reason cancelled without inventing a natural guest exit code. Historical checkpoints and references from other branches are not deleted. The original Job cannot resume, but a retained execution checkpoint can still be explicitly forked.

Stopped: idempotent success. Suspending/resuming/forking transitions refuse conflicting operations. Unknown execution ownership does not treat a missing PID as proof of stopping.

## 5. State, execution ownership and failures

```text
running → suspending → suspended → resuming → running
running → stopping → stopped
suspended → stopped                  # kill; relinquish continuation in original Job
```

Starting, failed and unknown retain/extend actual records; do not collapse every failure into stopped. File acceptance state is recorded separately, for example pending, partially_applied, applied and dropped.

Each Job has at most one valid execution owner at a time. Control operations durably record request ID, source generation, published checkpoint, source exit confirmation and target Attempt. Resume and fork must acquire execution ownership before entering the guest.

Critical failure boundaries:

1. Failure before publication: do not allow target restore. Continue the parent if it can safely unfreeze; otherwise record actual pending/unknown state.
2. Publication succeeded but source exit is unconfirmed: retain the object, but refuse resume of the original Job. Object existence alone does not prove suspension completed.
3. Child identity established but runner not started: the parent may continue; retrying the same request ID locates the same child Job.
4. Response lost: query status and operation records without creating side effects again. Lifecycle commands allow retries with `--request-id`; mismatched parameter digests are refused.
5. Delete/GC failed: retain traceable staging and references and retry cleanup; do not discard restore dependencies merely to free space.

In-process locks, disk locks and control-plane leases have separate responsibilities. This version remains on one host; copying a stage does not confer execution ownership across nodes.

## 6. Output and errors

Short control/inspection commands support `--json`, writing one result object to stdout and diagnostics to stderr. Structures include schema_version, operation, request_id, job_id, attempt_id, state, checkpoint_id and applicable fields; status also includes file state and capabilities.

Run/resume/fork execute in the foreground, preserving guest stdout/stderr and writing Job/stage information to the host diagnostic channel. This version does not offer `--json` that would mix with workload output. Automation reads status JSON for an explicit stage; run retains its existing result-file contract.

CLI syntax errors exit 2; management failures exit 1. Foreground workload exit codes propagate under the existing convention. Successful suspension makes the foreground runner return management success, while the Job records suspended rather than natural workload completion.

Errors need stable categories and actionable explanations: JOB_BUSY, CAPABILITY_UNSUPPORTED, CHECKPOINT_KIND_MISMATCH, CHECKPOINT_REFERENCED, COMPATIBILITY_MISMATCH, TARGET_CONFLICT, EXECUTION_UNKNOWN and STORAGE_FAILURE. Never silently downgrade to file fork, restart the command or ignore requested isolation policy.

## 7. Workflows

These commands illustrate the target interface; they do not imply current product support.

### Accept workspace results: keep the existing workflow

```sh
pvisor run --safe --stage ./stage/task -- codex
pvisor review ./stage/task --diff
pvisor fork ./stage/task --stage ./stage/alternative -- codex
pvisor apply ./stage/task --path src
# 或：pvisor drop ./stage/task
```

### Full suspension and continuation

```sh
# 终端 A；沿用正常 Job启动路径
pvisor run --vm --stage ./stage/task \
  --rootfs /path/to/rootfs -- bash

# 终端 B；保存并释放源 runner
pvisor suspend ./stage/task --ram-storage compressed
pvisor status ./stage/task

# 新 runner、相同 Job；前台继续
pvisor resume ./stage/task
```

### Fork a running VM

```sh
# 另一终端；父 Job 短暂冻结，然后继续
pvisor fork ./stage/task --state execution \
  --stage ./stage/branch-a --ram-storage compressed

# 从同一个已保存分叉点产生另一分支，无需再次冻结父 Job
pvisor checkpoint list ./stage/task --kind execution
pvisor fork ./stage/task --state execution --checkpoint CHECKPOINT_ID \
  --stage ./stage/branch-b
```

### Accept results after suspension

```sh
pvisor review ./stage/task --diff
# 先放弃在原 Job 内继续；不删除历史 checkpoint
pvisor kill ./stage/task
pvisor apply ./stage/task --all
```

## 8. Compatibility and implementation order

1. Preserve existing run, stage selection, workspace fork and apply/drop contracts; first extend Job lifecycle and checkpoint kinds.
2. Integrate the standalone snapshot runner's capture/restore capabilities into the existing VM executor, Run/Attempt/Bundle, stage, control channel and execution ownership. Reuse the original run entry point; do not pretend to unify it by redirecting to old snapshot run. Connect file versions and preimage ownership before declaring save capabilities; add no mandatory run flags.
3. Deliver suspend/resume, accurate status projection and failure cleanup; full VM saving must not masquerade as normal exit.
4. Deliver capture-and-continue transactions for execution create/fork, sharing capture implementation. Private stage copies form the baseline; optimize incremental capture and COW in backends over time.
5. Deliver checkpoint references, deletion/GC and historical selection. Preserve reading of old Jobs when changing internal layout; do not interpret old logical checkpoints as execution checkpoints.
6. Retain `status --review` as a compatible review entry point. Retain `fork --checkpoint`, requiring checkpoint kind to match explicit state.
7. The standalone snapshot command is removed, without a hidden legacy frontend. Storage SDKs still validate compatibility, provenance and references for old objects; no ordinary Job records are fabricated. Migration capability needs explicit acceptance.

During migration, do not offer run aliases that appear unified but bypass the stage. Before exposing each new command, require Job lifecycle, file acceptance and real VM validation; unsupported platforms/configurations return capability errors.

## 9. Acceptance gates for preserving run behavior

Integration must compare normal run behavior before and after changes, not only validate new commands:

- Preserve existing run arguments, default execution shorthand, configuration precedence, command arguments and environment.
- Identical configuration produces the same executor, policy requests, mount view and stage location.
- Without a save request, do not create execution checkpoints, add compression or fully copy RAM/file trees.
- Preserve output, exit codes, Bundle and apply/drop contracts for normal completion, cancellation and failure.
- Jobs that cannot be saved must still run normally; suspend or execution fork returns a capability error without changing Job state or permissions.
- Preserve default workspace fork behavior; changes from running execution fork require explicit operations.

Job and Attempt records may add versioned capability and lifecycle fields with compatible reads; these fields do not change user-facing run semantics.

## 10. Current implementation and acceptance boundary {#10-当前实现与验收边界}

Ordinary Jobs now integrate native VM capture and restore. Run parsing, defaults, rootfs, DAX, networking and executor selection retain their contracts. The standalone snapshot runner is not restored, and another manager does not fabricate Jobs.

| Feature | Current implementation |
| --- | --- |
| workspace create/fork/review/inspect | Retains stopped-Job upper layers, conflict preimages, generation and hard-link branch references; no process memory capture |
| execution create | Calls RunControlHandle and native capture through the running Job's private control socket; publication continues the source VM |
| suspend | Persists the request and seals the checkpoint; validates Job/Attempt/request against the terminal ExecutionSuspension receipt before creating the suspended head |
| resume | Continues only the current head, retaining the Job ID while runtime creates a new Attempt; private directories retain leases, records and Bundles, with the original stage selecting the current Attempt |
| execution fork | Captures and continues a running source, or creates a new Job from explicit history/a suspended head; RAM and upper layers are private, with lineage and durable branch references |
| request retries | Capture/suspend/resume/execution fork accept durable request IDs; a key never repeats startup or capture, bound-option changes refuse, and timeout retains admitted requests |
| list/show/delete/verify | Checks Job ownership; execution verify runs full SnapshotStore compatibility and content audits; deletion checks heads, branch references and reader leases |
| gc | Collects workspace transactions and pending objects, tombstones and unreferenced RAM in Job-owned native stores; no published object deletion or cross-Job/Cluster store-wide collection |
| import-base/verify-base | Reuses immutable SnapshotStore rootfs import and auditing; ordinary run accepts the returned rootfs path |
| apply/drop/kill | Refuses workspace mutation while suspended, handing off or uncertain; killing a suspended Job withdraws continuation rights but retains history before allowing workspace decisions |
| TUI | Resume with --tui starts the restored Attempt through the existing terminal frontend |

The current execution profile requires Linux x86_64 or macOS ARM64, no network devices, private RAM and an owned complete rootfs. Host root /, networking, writable RAM backing, memory pools and cold-page compression return capability errors without changing run configuration. Restore binds to the same host boot, binary and firmware. The captured guest environment is retained instead of being replaced by the resume/fork shell environment.

`execution-job.json` retains Job metadata, and `execution-job-root.json` associates Attempts with the root Job. Request admission, checkpoint publication and native termination are separate commits. Preparation failure before accepting a RunHandle retains a retryable suspended head; interrupted handoff or missing receipts stay conservative and never start another VM based only on a missing PID. Status reports requests/current Attempt, and explicit fork restores historical execution points.

Stages inside the workspace use an independent capture store whose ownership is recorded by the Job. Native launch bindings authenticate exclusions of already hidden management directories, while visible content and metadata remain fully audited. `resume`/execution `fork --eager-ram` fully reads RAM before startup; omission keeps lazy loading.

Workspace checkpoints still reference external lowers, and historical workspace operations conservatively acquire the source Job lease. Their file views are not treated as sealed complete machines. Execution metadata locking is independent of the source Attempt lease, allowing selection of published historical execution checkpoints while the parent VM runs.

Workspace branches use manifest hard links and require the same filesystem. Execution branches use storage leases, content references and durable Job branch records. Drop/kill do not automatically release these references; Job deletion/archiving has no release interface yet. Old standalone snapshot stores are not automatically converted into Job checkpoints.

The job_execution_vm acceptance test uses real KVM, ordinary CLI commands and a static guest to check external/nested stages, shared filesystem pools, eager RAM, capture-and-continue, raw/compressed RAM, same-Job/new-Attempt continuation, open descriptor and memory-counter continuity, historical branch isolation, restoration after deleting the source rootfs, request retries and head/branch deletion protection. Regular regressions additionally cover terminal receipt validation, refusing mutation of uncertain/suspended Jobs, stable Job selectors and retained Attempt records. The native test requires KVM/FUSE and is skipped by default:

```bash
PVISOR_TEST_LIBRARY_DIR=/path/to/firmware cargo nextest run --locked -p pvisor --test job_execution_vm --run-ignored only --test-threads 1
```

Execution Job records use version 2 with typed lifecycle states, resume requests (stage and RAM policy together), and fork requests. Workspace checkpoint manifests use schema 3 and require explicit kind, Attempt, generation, filesystem layers and access policy. Older Job records and workspace manifests are rejected without conversion. Native checkpoint payloads retain their independent integrity and host-binding validation.

Immutable imported base seals use version 2 and require a digest-bound content index; generations without that receipt are rejected.
