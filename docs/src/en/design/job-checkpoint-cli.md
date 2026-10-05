# Job-centered checkpoint and fork command design

> CLI update: the standalone `pvisor snapshot` entry is removed. Old interfaces/measurements below belong to their historical artifacts, not current executable instructions. See [CLI reference](../reference/cli.md) for current entries and capability boundaries.


> Status: implementation in stages. 2026-10-03. Workspace checkpoints and command management are connected; full execution save/restore for ordinary Jobs is not yet connected.
> The user accepted this design and added a constraint: keep `run` as unchanged as possible. The standalone `pvisor snapshot` entry is removed; this design retains the target interface for complete execution integration with the Job lifecycle. Existing full snapshots, persistent compression/deduplication and private-copy forks provide the foundation, without implying ordinary Jobs can already be saved.

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
├── checkpoint
│   ├── create             Create a consistent checkpoint for a Job
│   ├── list               List checkpoints within a Job
│   ├── show               Inspect kind, source, dependencies, compatibility and references
│   ├── delete             Delete checkpoints without retained references
│   └── gc                 Collect unreferenced content and leftover staging in the store
└── extensions             List companion tools
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

## 10. Current implementation and acceptance limits {#10-当前实现与验收边界}

This iteration preserves run parsing, defaults, rootfs, DAX, networking and executor selection. New workspace features reuse existing RunRecord, Job lease, logical checkpoints, OverlayFS preimages and Run Bundle; the old snapshot runner does not create fabricated Jobs.

| Feature | Current implementation |
| --- | --- |
| `review [JOB] [--checkpoint ID]` | Built in; reads a stable upper after acquiring the Job lease; refreshes file changes while preserving historical Bundle execution evidence; JSON identifies the boundary between them |
| `status --review` | Compatible entry point retained |
| `checkpoint create JOB` | Saves upper, preimages, policy, AttemptId and workspace generation when stopped and stage is staged; defaults to workspace |
| workspace create `--request-id` | Durable receipt; retries return the original object; if deleted, refuse recapture rather than reuse the key for a new result |
| `checkpoint list/show/delete` | Job ownership checks, unique prefix resolution and ambiguity refusal; corrupt published objects fail; deletion checks durable branch references |
| `checkpoint gc JOB` | **Scope in this stage is job_workspace_transactions**: only this Job's `.pending-*` and `.deleted-*`; shared execution content store is not connected, so no claim of store-wide GC |
| `fork --state workspace` | Retains default command restart semantics; adds `--stage` and `--name`; independently copies upper and preimages without copying control sockets, execution locks or acceptance records; branch references live in `source-checkpoint.json` |
| `inspect --checkpoint ID` | Mounts the selected workspace checkpoint's read-only file view |
| `apply/drop` | Explicit Job; after acquiring the lease, still checks stopped terminal state and completion time; refuses Jobs with missing processes but no terminal records; success advances workspace generation |
| `kill` | Idempotent when confirmed stopped; `--json` distinguishes already stopped from termination request sent |
| suspend/resume/execution create/execution fork | Capability refusal boundary connected; ordinary Jobs currently return `CAPABILITY_UNSUPPORTED` without freezing, copying or changing state; **full execution functionality has not been delivered** |

Workspace checkpoints retain the existing model: fixed staged upper and conflict preimages, with `lower_dirs` still external path references. They are not independent complete file trees; the Job lease does not protect external changes to host lowers. Review's `file_view` explicitly reports this limit, historical diffs remain relative to external lowers, and apply continues to recheck preimages. Execution saving must eventually seal every relevant layer completely; this model cannot be directly promoted to a full VM save point.

Branch pins in this stage use manifest hard links. Child stages must share a filesystem with the parent checkpoint; cross-filesystem forks explicitly refuse. Drop/kill do not release pins. Job deletion/archiving interfaces are not yet available, so a checkpoint with retained branches cannot be forcibly deleted.

Current workspace checkpoint management, review, inspect and fork conservatively acquire the source Job lease. Therefore, even selecting historical checkpoints refuses a running source Job. Independent checkpoint metadata locks and historical reads during execution are not yet delivered. Capturing a running workspace requires a real freeze boundary; copying a changing upper cannot pretend to yield a consistent version.

Full execution state still requires:

1. Sealing ordinary VM Overlay/DAX file layers and capturing device state, including file inode/handle rebinding after saving, without silently changing existing run configuration.
2. Using the existing control channel and supervisor for capture-and-continue, source runner exit confirmation, suspended head and execution-ownership handoff to a new Attempt.
3. Execution checkpoint Job ownership, shared content references, deletion/store-wide GC, persistent restore failure state and real Job VM validation.

The legacy `snapshot` command has been removed. Its full-copy storage objects remain available to the underlying SDK and have not been converted into Job checkpoints. Complete execution restoration for ordinary Jobs still depends on execution-profile capability and acceptance.

Validation for this iteration: `just fmt` passed; strict Clippy for core and TUI with Gateway enabled passed; the final `just test pvisor` run passed 319 tests and skipped 4. New tests cover real file forks, preimage copies, branch references retained through drop, duplicate requests, corrupt manifests and capability refusal without state changes. Testing identified and corrected an existing vsock test's faulty assumption about background worker scheduling; it now checks queue completion and actual RST content. A memory diagnostic test once returned WouldBlock after page state changed, then passed regression testing. These tests do not establish acceptance of full VM save/restore for ordinary Jobs.
