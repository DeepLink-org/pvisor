# File-level oversight and isolation {#apply}

<span id="staging-and-apply-semantics"></span>

pVisor manages Agent access and modifications at file granularity: it constrains access by path, isolates workspace changes, and supports review and selective acceptance. Access rules and execution results correspond directly to files and directories, making it easier to inspect what an Agent can access and what it changed.

## Constrain access and modification {#access-scope}

OverlayCore provides a managed file view. File policies define read-only inputs, writable paths and prohibited access; the executor restricts processes from bypassing that view to access host files. The isolation scope depends on the executor; see [Executor boundaries](../security/executor-boundaries.md).

Staging isolates candidate changes; it does not replace access control. See [File policies](../guides/policies/files.md) to configure access for a task.

## From staging to accepting changes {#candidate-changes}

File changes follow **Stage → Review → Apply / Drop**. Stage is a mechanism enabled with `--safe` or `--stage`, not a separate `stage` subcommand.

| Operation | Purpose |
| --- | --- |
| **Stage: retain candidate changes** | The Agent reads base files and saves changes in a private upper layer. Later tool calls see those changes without writing directly to the target workspace. Success, failure or termination does not automatically accept candidates. |
| **Review: inspect changes** | Inspect additions, modifications and deletions alongside execution results, installed controls and warnings, then choose which files to accept. Review itself does not modify the target workspace. |
| **Apply: accept changes** | After checking for conflicts, write all candidates or selected paths into the target workspace. Unselected changes remain available for later batches. |
| **Drop: discard candidates** | Discard the remaining unaccepted changes. Discarded candidates can no longer be applied; accepted changes are not undone. |

For example, if a run changes `src/` and temporary files, review the result, accept only `src/`, then discard the remaining candidates. See [Review and apply changes](../guides/review-apply.md) for commands.

To preserve current candidates and try another approach, use [workspace checkpoints and branches](resource-reuse.md#state-scope) before applying everything or dropping the stage.

<a id="staging-semantics"></a>
<a id="promises"></a>

## File-change handling rules {#file-change-rules}

| Rule | Description |
| --- | --- |
| Show net changes | Review presents final net additions, modifications and deletions. Temporary files created and then deleted are absent; renames appear as deletion plus addition. |
| Preserve file attributes | Applying changes preserves executable bits and symbolic-link targets as well as file contents, without replacing links with their targets' contents. See the platform exceptions below. |
| Apply in batches or repeat | Selective apply accepts chosen paths and their subtrees. Batches covering all changes produce the same result as applying everything at once; repeating apply on accepted paths adds no changes. See [Selective apply rules](../guides/review-apply.md) for path dependencies. |
| Check conflicts and path boundaries | A conflict in the selected scope rejects the batch before writing. External edits to untouched paths do not block application. Directory deletion checks for external additions, and paths replaced with links outside the workspace cannot redirect writes beyond it. |

<a id="known-gaps"></a>
<a id="不可逆的部分"></a>

## Scope and limitations {#scope-and-limitations}

- Workspace staging does not cover external effects such as remote API calls, database writes or sent messages. It also does not protect host paths explicitly shared for writing with `--mount SOURCE:write`.
- Apply writes to the target workspace; Drop cannot roll those writes back.
- Multi-file apply is not atomic to external editors; stop other writers during application. See [Review and apply changes](../guides/review-apply.md) for interrupted-apply recovery and conflict handling.

See [Staging and storage](../reference/cli.md#暂存与存储) for writes to HOME, the VM root and other locations, and [Known limitations](../security/known-limitations.md) for platform behavior such as macOS symbolic links.

See the [semantic specifications (unreviewed drafts)](../cases/06-stage-apply.md) for complete Stage / Apply / Drop behavior, including comparison of applying everything with direct execution. See [Testing and semspec](../community/testing.md) for the distinction between test results and human approval.
