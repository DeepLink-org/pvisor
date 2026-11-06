# Session-centered checkpoint and fork command design

> CLI update: the standalone `pvisor snapshot` entry is removed. Old interfaces/measurements below belong to their historical artifacts, not current executable instructions. See [CLI reference](../reference/cli.md) for current entries and capability boundaries.


> Status: native execution capture, suspension, continuation and branches are integrated with ordinary Sessions; section 10 defines support and acceptance.
> Constraint: preserve ordinary run configuration and default execution behavior; actual executor/profile determines capability.
>
> How to read: sections 1–9 are the target-state design (the design contract) of the Session-centered command tree, not an availability promise; whether a capability is integrated is governed by the implementation boundary in [section 10](#10-当前实现与验收边界).

## 1. Objects and basic constraints

**Session is the sole running object.** A Session retains task identity, policy, workspace targets, run history and parent/child relationships. `stage` is the persistent directory where users manage that Session; file staging and execution checkpoints within it have different lifecycles.

| Object | Identity and purpose | Mutability |
| --- | --- | --- |
| Session | SessionId; operated on by run/status/kill/suspend/resume/fork | Lifecycle, current Execution and working view are mutable |
| Execution | ExecutionId; one runner lifecycle | A new runner must obtain a new ExecutionId and execution-ownership generation |
| workspace | Working view, staged changes, preimages and acceptance records | A new generation is produced when the Session runs or apply/drop executes |
| checkpoint | CheckpointId; saves consistent files or full execution state | Manifest and content are immutable; incremental parent references are permitted |
| Content object | Files or RAM blocks stored by content identity | Immutable; Session, checkpoint and restore references pin it |

Checkpoint kinds:

- `workspace`: file version, staged changes, preimages and associated configuration; starts a new command without continuing CPU/RAM state.
- `execution`: CPU, RAM, devices, file version, working-view ownership and compatibility; continues saved processes.

Compression, deduplication and full/incremental capture are checkpoint storage mechanisms; they do not create a separate set of Sessions.

## 2. Command tree

```text
pvisor
├── run                    Create and run a Session
├── status                 Inspect Session state, capabilities and retained objects
├── review                 Review changes bound to a file version
├── inspect                Inspect a read-only Session / checkpoint file view
├── apply                  Accept selected file changes from a stopped Session
├── drop                   Discard pending file changes from a stopped Session
├── kill                   Terminate a Session without creating a checkpoint
├── suspend                Save an execution checkpoint, exit runner, retain Session
├── resume                 Continue the same Session from its current suspended checkpoint
├── fork                   Create a new Session from file or execution state
└── checkpoint
│   ├── create             Create a consistent checkpoint for a Session
│   ├── list               List checkpoints within a Session
│   ├── show               Inspect kind, source, dependencies, compatibility and references
│   ├── delete             Delete checkpoints without retained references
│   └── gc                 Collect unreferenced content and leftover staging in the store
```

`replay/cache/memory-pool` remain tools; they do not create product instance identities parallel to Sessions. The default execution shorthand `pvisor -- COMMAND` remains equivalent to `pvisor spawn -- COMMAND`.

The target public workflow does not add `session run`, `vm run` or `snapshot run`.

## 3. Selecting Sessions and storage

The unified `SESSION` argument accepts a SessionId, stage directory, existing run.json / internal Session path, or `last`. It retains the existing resolver; multiple candidates are refused rather than silently selected.

- `last` resolves once within the current workspace and selected storage index; it does not automatically search for the most recent suspendable or stopped Session.
- `--name` is display information, not a substitute for stable identity; multiple Sessions with the same name do not form a selection rule.
- `status/review/inspect` may omit SESSION, equivalent to `last`.
- `apply/drop/kill/suspend/resume/fork` require an explicit SESSION; writing `last` counts as explicit selection.
- `--output-dir/-o` continues to control the existing Session lookup root; explicit stage paths do not depend on another index. It does not export runtime content into a new directory.
- `CHECKPOINT_ID` resolves within the selected Session. Unique digest prefixes are accepted; ambiguity or mismatched ownership is refused. This version introduces no `latest` checkpoint alias; historical selection must be explicit.
- `resume` uses only the suspended head durably recorded by the Session, accepting neither `latest` nor arbitrary old checkpoints. Running from an old state requires a fork.

Logical layout; existing upper-level file layouts may map compatibly without a single migration:

```text
stage/session-a/
├── session.json
├── executions/<execution-id>/
├── workspace/             # writable state, preimages, acceptance records
└── checkpoints/
    ├── index.json
    └── <checkpoint-id>/   # manifest, CPU/device state, content references

<per-user-content-store>/  # not a separate VM instance manager
└── content/<content-id>
```

Checkpoint manifests, ownership and references live within the stage; shared content may live in a common content store. Configuration selects the store; common commands do not require an additional `--store`. Moving a stage across filesystems requires explicit export/import tools; simply moving a directory cannot be promised to preserve hard-link references.

`drop` does not delete Session records, Execution history or checkpoints. Checkpoint deletion does not modify the current workspace. Session deletion and archiving are not added to this CLI version; `drop` must not imply either action.

## 4. Command syntax and behavior

### 4.1 run: the sole startup entry point

```sh
pvisor spawn [现有运行、策略、记录与资源选项] \
  [--stage PATH] [--name NAME] -- COMMAND...
```

Keep existing `--vm/--executor`, `--rootfs`, `--safe`, `--mount`, networking and Gateway options and configuration precedence.

**No new mandatory startup flags; no requirement for `--checkpointable`.** Save capabilities derive from the selected executor, rootfs, filesystem, devices and network configuration and are recorded in Session metadata and status. Users still start Sessions as before, requesting suspend, checkpoint or execution fork when needed.

Preserve run argument parsing, default executor, command and environment, CPU/memory, stdio, policy installation, stage selection and result acceptance semantics as far as possible. Potential future saving must not silently disable DAX, networking or devices, change the rootfs view, enable compression/paging by default or fully copy all inputs.

The initial support matrix remains limited to validated configurations. Declare capabilities only after full snapshots, Run/Execution/Bundle, stage, file versions and preimage ownership are connected. The historical standalone snapshot runner does not establish that ordinary Sessions are connected.

Save requests use two checks: record static capabilities at startup, then check dynamic conditions at operation time (active connections, file handles, in-flight device access, compatibility, space and execution ownership). If conditions are unmet, return specific reasons while keeping the original Session running. Do not restart the workload to retrofit capabilities or silently reduce isolation policy.

If implementation requires optional configuration, reuse existing executor configuration first and discuss its necessity separately; do not add another run option group merely to integrate snapshots. `--ram-storage raw|compressed` belongs on commands that actually capture state, not on run.

### 4.2 status and review

```sh
pvisor status [SESSION] [--json]
pvisor status --review [SESSION] [--diff]       # 保留兼容入口
pvisor review [SESSION] [--checkpoint ID] [--diff] [--json]
```

Status must show at least SessionId, ExecutionId, executor, workspace/stage, execution state, workspace generation, pending acceptance state, suspended head, checkpoint capabilities and blocking reasons, and source Session/checkpoint.

Show lifecycle and file state separately, for example `suspended` plus `pending changes`; an absent PID must not be displayed as completion.

Review output must bind to SessionId, workspace version / checkpoint ID, change-set digest, target and preimages. Stopped/suspended Sessions read a stable view. If a running Session can obtain a consistent file version, freeze and capture a workspace checkpoint, continue running, then generate the diff; otherwise explicitly refuse. `--checkpoint` views historical file versions without granting the ability to apply them directly to the current Session.

Keep existing diff size limits. Later changes invalidate an old review's correspondence to the current workspace; apply must recheck the version, selected set and host preimages.

### 4.3 inspect

```sh
pvisor inspect [SESSION] [--checkpoint ID] -- COMMAND...
```

Inspect the current stable file view or selected checkpoint read-only. This is not guest exec and does not execute guest CPU state. Mutable views of running Sessions use the same consistency threshold as review.

### 4.4 apply and drop

```sh
pvisor apply SESSION [--path RELATIVE_PATH ...] \
  [--include GLOB ...] [--exclude GLOB ...] [--all] [--target PATH]
pvisor drop SESSION
```

Keep selective apply, original-state/conflict checks, target selection and existing acceptance-record contracts. Do not apply automatically or change approval or acceptance identity because of fork/suspend.

Both commands permit only confirmed stopped Sessions. Running, transitional, suspended and unknown execution-ownership states are refused. A suspended Session must first use `kill SESSION` to relinquish continuation within the original Session before its working view can be processed.

Success creates a new working-view generation; retained immutable checkpoints are not rewritten. Execution restore from an old checkpoint can only create a new Session with an independent working view, without rolling back the existing Session's acceptance history.

Branches inherit preimages from the fork point. If two branches apply to the same target in sequence, the second must check whether the host has changed; a shared source does not waive conflict checks.

### 4.5 suspend

```sh
pvisor suspend SESSION [--ram-storage raw|compressed] \
  [--timeout DURATION] [--request-id ID] [--json]
```

Operate only on running Sessions. Freeze CPU and device writes, save an execution checkpoint and file version at the same freeze boundary; after publication, confirm the source runner has exited, commit the suspended head, and output SessionId, CheckpointId and state.

`--ram-storage` controls persistent RAM encoding for this capture: raw by default, compressed for existing block compression/deduplication. It does not change the online pager or guarantee incremental capture. No empty `--incremental` / `--lazy` options are provided.

`--timeout` budgets waiting and freezing; timeout is neither a force kill nor proof that a worker has ended. If cleanup is still in progress, return a durable operation ID and refuse concurrent restore. A client exit must not start another runner while the checkpoint is published but the source may still be running.

Repeated suspension of an already suspended Session returns the existing head without creating another checkpoint. Workspace-only Sessions or Sessions without valid execution capture explicitly return a capability error.

### 4.6 resume

```sh
pvisor resume SESSION [--request-id ID]
```

Continue the same Session only from its suspended head. Retain SessionId, create a new ExecutionId and execution-ownership generation, validate compatibility and complete references, create a writable working view, and execute after installing state.

Do not accept a new command, rootfs, CPU/memory or policy overrides. Configuration changes use workspace fork or a new run; the first version does not offer unreliable execution-state parameter changes.

The command holds the terminal in the foreground, like run. It cannot restore old host TTYs/pipes or external connections. Executor profiles must declare permitted console endpoint reconstruction.

On failure before execution starts, retain the suspended head for retry. Failure after execution starts must not automatically mark the head safe for retry without side effects. Record the new Execution and actual state; returning to the old point requires explicit fork.

### 4.7 fork

```sh
pvisor fork SESSION [--state workspace|execution] [--checkpoint ID] \
  [--stage NEW_PATH] [--name NAME] [--request-id ID] \
  [--ram-storage raw|compressed] [-- COMMAND...]
```

Default to `--state workspace`, retaining existing file fork behavior. Execution must be selected explicitly; finding a VM checkpoint must not switch modes automatically.

| Condition | workspace fork | execution fork |
| --- | --- | --- |
| running, no checkpoint selected | Obtain a consistent file save point; refuse without freeze capability | Freeze, publish a complete save point, establish child Session and references, then unfreeze parent |
| suspended, no checkpoint selected | Derive from suspended file version and launch command | Reference suspended head and create child Session without changing parent |
| stopped, no checkpoint selected | Capture and derive from stable file view | Refuse: no live CPU state; do not automatically select a historical point |
| Selected checkpoint | Must be workspace or explicitly extract an execution file view; first version only accepts matching kinds | Must be execution with matching source and capabilities |
| Replacement command | Allowed; defaults to saved command configuration | Refused; continues saved processes |

Selecting an immutable checkpoint does not freeze a running parent or read its current mutable files; only ownership, configuration and retained references are checked. Omit `--checkpoint` to capture the parent's current state.

`--ram-storage` applies only to newly captured execution save points; refuse it for workspace mode or reuse of existing checkpoints. `--stage NEW_PATH` must be empty or nonexistent; never overwrite an old Session. If omitted, retain existing persistent Session storage selection rules and generate a new directory.

The child uses a new SessionId and ExecutionId, independent execution ownership and independently writable workspace/RAM. Its manifest records parent SessionId, source checkpoint ID and state kind. Guest PID/boot ID may retain their saved values; they are not global Session identities. Future network and management identities require independent rebinding contracts.

Copy or COW-clone working state and inherit immutable content references. Do not recursively copy the parent's execution locks, sockets, temporary writes, terminal state, acceptance authorization or checkpoint index. Checkpoint creation history belongs to the parent; the child holds a source reference without claiming ownership of that history.

Freeze the parent only through durable save-point commit and child branch reference/identity creation. The child runner may start after the parent unfreezes; child startup failure must not leave the parent paused indefinitely. Failed child Session records remain queryable and reclaimable; request IDs prevent retries from creating multiple children for the transaction.

### 4.8 checkpoint management

```sh
pvisor checkpoint create SESSION [--kind workspace|execution] \
  [--ram-storage raw|compressed] [--timeout DURATION] \
  [--request-id ID] [--json]
pvisor checkpoint list SESSION [--kind workspace|execution] [--json]
pvisor checkpoint show SESSION CHECKPOINT_ID [--json]
pvisor checkpoint delete SESSION CHECKPOINT_ID [--json]
pvisor checkpoint gc SESSION [--json]
```

Create defaults to workspace. Running execution create freezes, publishes an independent object, then unfreezes the parent; **it does not suspend the Session**. Its capture transaction is shared with execution fork. Stopped Sessions cannot create execution checkpoints. Suspended Sessions may reference an existing execution head without starting the guest; returning an existing object reports `reused=true`.

Create does not offer checkpoint overwrite. Content may be full, compressed, deduplicated or later incremental; the CLI always outputs immutable identity and actual format.

Show reports kind, source Session/Execution, file version, compatibility, incremental parent dependencies, logical/encoded bytes and references blocking deletion. Logical/encoded bytes are not physical residency or density metrics.

Suspended heads, child Session sources, incremental descendants, active restores and staged writes protect referenced checkpoints from deletion. The first version refuses deletion when these references exist, with no `--force` or implicit cascade. Future detach/compaction must also create independent objects first and atomically replace references.

GC locates the content store through SESSION. It may reclaim any unreferenced content and leftover staging within that store, not merely scan one Session. JSON output explicitly reports scope and counts by category. Active writes, restores, retained checkpoints and branch dependencies must not be reclaimed.

### 4.9 kill

```sh
pvisor kill SESSION [--json]
```

Running: retain the existing graceful termination request; keep stopping/unknown until exit is confirmed, without prematurely allowing apply/drop.

Suspended: there is no runner to kill. Explicitly end the Session's right to continue, release the suspended head's automatic pin, transition to stopped and record termination reason cancelled without inventing a natural guest exit code. Historical checkpoints and references from other branches are not deleted. The original Session cannot resume, but a retained execution checkpoint can still be explicitly forked.

Stopped: idempotent success. Suspending/resuming/forking transitions refuse conflicting operations. Unknown execution ownership does not treat a missing PID as proof of stopping.

## 5. State, execution ownership and failures

```text
running → suspending → suspended → resuming → running
running → stopping → stopped
suspended → stopped                  # kill; relinquish continuation in original Session
```

Starting, failed and unknown retain/extend actual records; do not collapse every failure into stopped. File acceptance state is recorded separately, for example pending, partially_applied, applied and dropped.

Each Session has at most one valid execution owner at a time. Control operations durably record request ID, source generation, published checkpoint, source exit confirmation and target Execution. Resume and fork must acquire execution ownership before entering the guest.

Critical failure boundaries:

1. Failure before publication: do not allow target restore. Continue the parent if it can safely unfreeze; otherwise record actual pending/unknown state.
2. Publication succeeded but source exit is unconfirmed: retain the object, but refuse resume of the original Session. Object existence alone does not prove suspension completed.
3. Child identity established but runner not started: the parent may continue; retrying the same request ID locates the same child Session.
4. Response lost: query status and operation records without creating side effects again. Lifecycle commands allow retries with `--request-id`; mismatched parameter digests are refused.
5. Delete/GC failed: retain traceable staging and references and retry cleanup; do not discard restore dependencies merely to free space.

In-process locks, disk locks and control-plane leases have separate responsibilities. This version remains on one host; copying a stage does not confer execution ownership across nodes.

## 6. Output and errors

Short control/inspection commands support `--json`, writing one result object to stdout and diagnostics to stderr. Structures include schema_version, operation, request_id, session_id, execution_id, state, checkpoint_id and applicable fields; status also includes file state and capabilities.

Run/resume/fork execute in the foreground, preserving guest stdout/stderr and writing Session/stage information to the host diagnostic channel. This version does not offer `--json` that would mix with workload output. Automation reads status JSON for an explicit stage; run retains its existing result-file contract.

CLI syntax errors exit 2; management failures exit 1. Foreground workload exit codes propagate under the existing convention. Successful suspension makes the foreground runner return management success, while the Session records suspended rather than natural workload completion.

Errors need stable categories and actionable explanations: SESSION_BUSY, CAPABILITY_UNSUPPORTED, CHECKPOINT_KIND_MISMATCH, CHECKPOINT_REFERENCED, COMPATIBILITY_MISMATCH, TARGET_CONFLICT, EXECUTION_UNKNOWN and STORAGE_FAILURE. Never silently downgrade to file fork, restart the command or ignore requested isolation policy.

## 7. Workflows

These commands illustrate the target interface; they do not imply current product support.

### Accept workspace results: keep the existing workflow

```sh
pvisor spawn --safe --stage ./stage/task -- codex
pvisor review ./stage/task --diff
pvisor fork ./stage/task --stage ./stage/alternative -- codex
pvisor apply ./stage/task --path src
# 或：pvisor drop ./stage/task
```

### Full suspension and continuation

```sh
# 终端 A；沿用正常 Session启动路径
pvisor spawn --vm --stage ./stage/task \
  --rootfs /path/to/rootfs -- bash

# 终端 B；保存并释放源 runner
pvisor suspend ./stage/task --ram-storage compressed
pvisor status ./stage/task

# 新 runner、相同 Session；前台继续
pvisor resume ./stage/task
```

### Fork a running VM

```sh
# 另一终端；父 Session 短暂冻结，然后继续
pvisor fork ./stage/task --state execution \
  --stage ./stage/branch-a --ram-storage compressed

# 从同一个已保存分叉点产生另一分支，无需再次冻结父 Session
pvisor checkpoint list ./stage/task --kind execution
pvisor fork ./stage/task --state execution --checkpoint CHECKPOINT_ID \
  --stage ./stage/branch-b
```

### Accept results after suspension

```sh
pvisor review ./stage/task --diff
# 先放弃在原 Session 内继续；不删除历史 checkpoint
pvisor kill ./stage/task
pvisor apply ./stage/task --all
```

## 8. Compatibility and implementation order

1. Preserve existing run, stage selection, workspace fork and apply/drop contracts; first extend Session lifecycle and checkpoint kinds.
2. Integrate the standalone snapshot runner's capture/restore capabilities into the existing VM executor, Run/Execution/Bundle, stage, control channel and execution ownership. Reuse the original run entry point; do not pretend to unify it by redirecting to old snapshot run. Connect file versions and preimage ownership before declaring save capabilities; add no mandatory run flags.
3. Deliver suspend/resume, accurate status projection and failure cleanup; full VM saving must not masquerade as normal exit.
4. Deliver capture-and-continue transactions for execution create/fork, sharing capture implementation. Private stage copies form the baseline; optimize incremental capture and COW in backends over time.
5. Deliver checkpoint references, deletion/GC and historical selection. Preserve reading of old Sessions when changing internal layout; do not interpret old logical checkpoints as execution checkpoints.
6. Retain `status --review` as a compatible review entry point. Retain `fork --checkpoint`, requiring checkpoint kind to match explicit state.
7. The standalone snapshot command is removed, without a hidden legacy frontend. Storage SDKs still validate compatibility, provenance and references for old objects; no ordinary Session records are fabricated. Migration capability needs explicit acceptance.

During migration, do not offer run aliases that appear unified but bypass the stage. Before exposing each new command, require Session lifecycle, file acceptance and real VM validation; unsupported platforms/configurations return capability errors.

## 9. Acceptance gates for preserving run behavior

Integration must compare normal run behavior before and after changes, not only validate new commands:

- Preserve existing run arguments, default execution shorthand, configuration precedence, command arguments and environment.
- Identical configuration produces the same executor, policy requests, mount view and stage location.
- Without a save request, do not create execution checkpoints, add compression or fully copy RAM/file trees.
- Preserve output, exit codes, Bundle and apply/drop contracts for normal completion, cancellation and failure.
- Sessions that cannot be saved must still run normally; suspend or execution fork returns a capability error without changing Session state or permissions.
- Preserve default workspace fork behavior; changes from running execution fork require explicit operations.

Session and Execution records may add versioned capability and lifecycle fields with compatible reads; these fields do not change user-facing run semantics.

## 10. Current implementation and acceptance boundary {#10-当前实现与验收边界}

Ordinary Sessions now integrate native VM capture and restore. Run parsing, defaults, rootfs, DAX, networking and executor selection retain their contracts. The standalone snapshot runner is not restored, and another manager does not fabricate Sessions.

| Feature | Current implementation |
| --- | --- |
| workspace create/fork/review/inspect | Retains stopped-Session upper layers, conflict preimages, generation and hard-link branch references; no process memory capture |
| execution create | Calls Execution and native capture through the running Session's private control socket; publication continues the source VM |
| suspend | Persists the request and seals the checkpoint; validates Session/Execution/request against the terminal ExecutionSuspension receipt before creating the suspended head |
| resume | Continues only the current head, retaining the Session ID while runtime creates a new Execution; private directories retain leases, records and Bundles, with the original stage selecting the current Execution |
| execution fork | Captures and continues a running source, or creates a new Session from explicit history/a suspended head; RAM and upper layers are private, with lineage and durable branch references |
| request retries | Capture/suspend/resume/execution fork accept durable request IDs; a key never repeats startup or capture, bound-option changes refuse, and timeout retains admitted requests |
| list/show/delete/verify | Checks Session ownership; execution verify runs full SnapshotStore compatibility and content audits; deletion checks heads, branch references and reader leases |
| gc | Collects workspace transactions and pending objects, tombstones and unreferenced RAM in Session-owned native stores; no published object deletion or cross-Session/Cluster store-wide collection |
| import-base/verify-base | Reuses immutable SnapshotStore rootfs import and auditing; ordinary run accepts the returned rootfs path |
| apply/drop/kill | Refuses workspace mutation while suspended, handing off or uncertain; killing a suspended Session withdraws continuation rights but retains history before allowing workspace decisions |

The current execution profile requires Linux x86_64 or macOS ARM64, no network devices, private RAM and an owned complete rootfs. Host root /, networking, writable RAM backing, memory pools and cold-page compression return capability errors without changing run configuration. Restore binds to the same host boot, binary and firmware. The captured guest environment is retained instead of being replaced by the resume/fork shell environment.

`execution-session.json` retains Session metadata, and `execution-session-root.json` associates Executions with the root Session. Request admission, checkpoint publication and native termination are separate commits. Preparation failure before accepting a RunHandle retains a retryable suspended head; interrupted handoff or missing receipts stay conservative and never start another VM based only on a missing PID. Status reports requests/current Execution, and explicit fork restores historical execution points.

Stages inside the workspace use an independent capture store whose ownership is recorded by the Session. Native launch bindings authenticate exclusions of already hidden management directories, while visible content and metadata remain fully audited. `resume`/execution `fork --eager-ram` fully reads RAM before startup; omission keeps lazy loading.

Workspace checkpoints still reference external lowers, and historical workspace operations conservatively acquire the source Session lease. Their file views are not treated as sealed complete machines. Execution metadata locking is independent of the source Execution lease, allowing selection of published historical execution checkpoints while the parent VM runs.

Workspace branches use manifest hard links and require the same filesystem. Execution branches use storage leases, content references and durable Session branch records. Drop/kill do not automatically release these references; Session deletion/archiving has no release interface yet. Old standalone snapshot stores are not automatically converted into Session checkpoints.

The session_execution_vm acceptance test uses real KVM, ordinary CLI commands and a static guest to check external/nested stages, shared filesystem pools, eager RAM, capture-and-continue, raw/compressed RAM, same-Session/new-Execution continuation, open descriptor and memory-counter continuity, historical branch isolation, restoration after deleting the source rootfs, request retries and head/branch deletion protection. Regular regressions additionally cover terminal receipt validation, refusing mutation of uncertain/suspended Sessions, stable Session selectors and retained Execution records. The native test requires KVM/FUSE and is skipped by default:

```bash
PVISOR_TEST_LIBRARY_DIR=/path/to/firmware cargo nextest run --locked -p pvisor --test session_execution_vm --run-ignored only --test-threads 1
```

Session records use version 3 with typed lifecycle states, resume requests (stage and RAM policy together), and fork requests. Workspace checkpoint manifests use schema 3 and require explicit kind, Execution, generation, filesystem layers and access policy. Older Session records and workspace manifests are rejected without conversion. Native checkpoint payloads retain their independent integrity and host-binding validation.

Immutable imported base seals use version 2 and require a digest-bound content index; generations without that receipt are rejected.
